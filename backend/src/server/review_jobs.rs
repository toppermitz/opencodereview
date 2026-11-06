use super::*;

/// A head or base change is not a failed review when no separate superseded flag was
/// set: keep the full-review or reprocess request and supersede the run.
pub(crate) fn settle_preparation_error(
    db: &Database,
    run_id: i64,
    pr_id: i64,
    repo_id: i64,
    auto_enabled: bool,
    force_full: bool,
    reprocess: bool,
    error: &str,
    superseded: bool,
    cancelled: bool,
) -> Result<(), String> {
    let active = db
        .review_run_by_id(run_id)?
        .is_some_and(|run| matches!(run.status.as_str(), "running" | "pending"));
    if !active {
        return Ok(());
    }
    if superseded {
        db.supersede_review_run_without_changing_pr(run_id, "Review superseded by a changed PR target")
    } else if cancelled {
        db.cancel_review_run(run_id)
    } else if is_review_target_changed_error(error) {
        db.restore_review_intent(pr_id, force_full, reprocess)?;
        record_review_target_change(
            db,
            run_id,
            pr_id,
            repo_id,
            auto_enabled,
            ReviewTargetChange::HeadChanged,
        )
    } else {
        let _ = db.update_review_run_progress(
            run_id,
            &review::preparation_failure_phase(false, false),
            "Index preparation failed",
        );
        db.fail_review_run(run_id, &format!("Index preparation failed: {error}"))
    }
}

pub(crate) fn supersede_active_review(db: &Database, pr_id: i64, job: &ReviewJob) -> Result<(), String> {
    db.restore_review_intent(pr_id, job.force_full, job.reprocess)?;
    job.superseded.store(true, Ordering::Relaxed);
    for cancel in job.cancel_by_run.values() {
        cancel.store(true, Ordering::Relaxed);
    }
    Ok(())
}

pub(crate) fn start_review_job(state: &SharedState, pr_id: i64, force_full: bool) -> Result<(), String> {
    start_review_job_with_intent(state, pr_id, force_full, false)
}

pub(crate) fn start_review_job_with_intent(
    state: &SharedState,
    pr_id: i64,
    force_full: bool,
    reprocess: bool,
) -> Result<(), String> {
    // A stored intent replayed by the queue drain is not a fresh request.
    let fresh_request = force_full || reprocess;
    let mut active_jobs = state
        .review_jobs
        .lock()
        .map_err(|_| "review job lock poisoned".to_string())?;
    if active_jobs.contains_key(&pr_id) {
        return Err("A review is already running for this PR.".to_string());
    }
    let settings = with_db_raw(state, |db| db.settings())?;
    let replayed_revision = if fresh_request {
        None
    } else {
        with_db_raw(state, |db| db.stored_review_intent_revision(pr_id))
            .ok()
            .flatten()
    };
    let (force_full, reprocess) = with_db_raw(state, |db| db.register_review_intent(pr_id, force_full, reprocess))?;
    if !force_full && !reprocess {
        let (head_sha, lane, last_decision_sha) = with_db_raw(state, |db| db.pr_review_start_state(pr_id))?;
        if last_decision_sha.as_deref() == Some(head_sha.as_str())
            && !matches!(lane.as_str(), "manual_required" | "failed")
        {
            let _ = with_db_raw(state, |db| db.restore_decided_pr_lane(pr_id));
            return Ok(());
        }
    }
    if with_db_raw(state, |db| db.pr_changed_file_count(pr_id))
        .ok()
        .flatten()
        .is_some_and(|count| count > settings.review_max_files)
    {
        with_db_raw(state, |db| db.mark_pr_size_skipped(pr_id, settings.review_max_files))?;
        return Ok(());
    }
    // The acknowledgement is best-effort: it must never block a review start.
    let _ = with_db_raw(state, |db| {
        db.acknowledge_review_cycle(pr_id, fresh_request, replayed_revision)
    });
    if active_jobs.len() >= settings.review_concurrency as usize {
        with_db_raw(state, |db| {
            if force_full {
                db.set_force_full_next_review(pr_id, true)?;
            }
            if reprocess {
                db.set_reprocess_next_review(pr_id, true)?;
            }
            db.set_pr_lane(pr_id, "queued", Some("Waiting for a free review slot"))
        })?;
        drop(active_jobs);
        spawn_queue_acknowledgements(state.config.db.clone());
        return Ok(());
    }
    let (repo_id, full_name, number, head_sha) = with_db_raw(state, |db| db.pr_review_coords(pr_id))?;
    let repo = with_db_raw(state, |db| db.repo_by_id(repo_id))?;
    let (last_reviewed_sha, unresolved_review, skill_content, db_handle, run_id, model) = with_db_raw(state, |db| {
        let last_reviewed_sha = if force_full {
            None
        } else {
            db.last_reviewed_sha(pr_id).ok().flatten()
        };
        let unresolved_review = if force_full {
            None
        } else {
            db.latest_unresolved_review(pr_id)?
        };
        // Engine the next account's provider reviews with until a stage records its own.
        let provider = db
            .pick_ai_account(&[])?
            .map_or_else(|| "claude".to_string(), |account| account.provider);
        let (engine_model, effort) = db.review_engine(&provider)?;
        let label = crate::providers::engines::label(&provider, &engine_model, &effort);
        let run_id = db.start_review_skeleton_with(pr_id, Some(&label))?;
        db.set_force_full_next_review(pr_id, false)?;
        db.set_reprocess_next_review(pr_id, false)?;
        Ok((
            last_reviewed_sha,
            unresolved_review,
            db.active_skill_content(),
            db.handle(),
            run_id,
            engine_model,
        ))
    })?;
    let cancel = Arc::new(AtomicBool::new(false));
    let superseded = Arc::new(AtomicBool::new(false));
    with_db_raw(state, |db| {
        db.set_review_run_running(run_id, &review::index_phase_json("running"))
    })?;
    active_jobs.insert(
        pr_id,
        ReviewJob {
            cancel_by_run: HashMap::from([(run_id, cancel.clone())]),
            superseded: superseded.clone(),
            force_full,
            reprocess,
        },
    );
    drop(active_jobs);
    spawn_queue_acknowledgements(state.config.db.clone());
    let state = state.clone();
    thread::spawn(move || {
        let _archive_lease = state.archive_leases.acquire(run_id);
        let preparation = (|| -> Result<review::ReviewInputs, String> {
            let repo_path = ensure_repo_checkout_cancellable(&state.config.repos_dir, &repo, &cancel)?;
            if !with_db_raw(&state, |db| {
                db.update_review_run_progress(
                    run_id,
                    &review::index_phase_json("running"),
                    "Repository available; checking the live PR head and immediate base.",
                )
            })? {
                return Err("cancelled".into());
            }
            let live = gitlab::live_pr_state_cancellable(&full_name, number, &cancel)?;
            match classify_review_target(&live, &head_sha) {
                ReviewTargetChange::Current => {}
                change => {
                    with_db_raw(&state, |db| {
                        if change == ReviewTargetChange::HeadChanged {
                            db.restore_review_intent(pr_id, force_full, reprocess)?;
                        }
                        record_review_target_change(db, run_id, pr_id, repo_id, settings.review_auto_enabled, change)
                    })?;
                    return Err("review target superseded".into());
                }
            }
            let expected_scope = live.review_scope();
            let prior_scope = with_db_raw(&state, |db| {
                if let Some(previous) = db.pr_scope(repo_id, number)? {
                    if !crate::review::scope::equivalent(&repo_path, &previous, &expected_scope, &cancel) {
                        db.invalidate_review_scope(
                            pr_id,
                            Some(run_id),
                            "PR base changed; rebuilding the review scope",
                        )?;
                    }
                }
                if !db.refresh_review_base(pr_id, &expected_scope)? {
                    return Err("PR changed during preparation".into());
                }
                db.last_review_scope(pr_id)
            })?;
            let (pr_title, pr_body) = gitlab::pr_description(&full_name, number)?;
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".to_string());
            }
            if !with_db_raw(&state, |db| {
                db.update_review_run_progress(
                    run_id,
                    &review::index_phase_json("ok"),
                    "Repository and live PR target ready. Context will index the pinned head.",
                )
            })? {
                return Err("cancelled".into());
            }
            let (unresolved_review_head, unresolved_review_body) = unresolved_review
                .map(|(head, body)| (Some(head), Some(body)))
                .unwrap_or((None, None));
            let inputs = review::ReviewInputs {
                frozen: None,
                run_id,
                repo_id,
                pr_id,
                repo_path,
                full_name,
                number,
                base_ref: expected_scope.base_ref.clone(),
                fixed_head_sha: None,
                offline: false,
                expected_scope: Some(expected_scope),
                prior_scope,
                last_reviewed_sha,
                force_full,
                reprocess,
                skill_content,
                unresolved_review_head,
                unresolved_review_body,
                pr_title,
                pr_body,
                model,
                review_depth_mode: settings.review_depth_mode.clone(),
                review_deep_threshold: settings.review_deep_threshold.max(2) as usize,
                review_chunk_size: settings.review_chunk_size.clamp(2, 20) as usize,
                review_parallel_chunks: settings.review_parallel_chunks.clamp(1, 20) as usize,
                db_handle: Some(db_handle.clone()),
                archive_dir: state.config.archive_dir(),
                budget: None,
            };
            Ok(inputs)
        })();
        match preparation {
            Ok(inputs) => run_review_job(db_handle, state.review_jobs.clone(), inputs, cancel, superseded),
            Err(error) => {
                let _ = with_db_raw(&state, |db| {
                    settle_preparation_error(
                        db,
                        run_id,
                        pr_id,
                        repo_id,
                        settings.review_auto_enabled,
                        force_full,
                        reprocess,
                        &error,
                        superseded.load(Ordering::Relaxed),
                        cancel.load(Ordering::Relaxed),
                    )
                });
                if let Ok(mut jobs) = state.review_jobs.lock() {
                    jobs.remove(&pr_id);
                }
            }
        }
    });
    Ok(())
}
