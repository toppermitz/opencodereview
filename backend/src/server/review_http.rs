use super::*;

pub(crate) fn start_review_http(
    request: &Request,
    state: &SharedState,
    pr_id: i64,
) -> Result<serde_json::Value, String> {
    let payload: StartReviewRequest = if request.body.is_empty() {
        StartReviewRequest { force_full: None }
    } else {
        serde_json::from_slice(&request.body).map_err(|error| error.to_string())?
    };
    start_review_job_with_intent(state, pr_id, payload.force_full.unwrap_or(false), true)?;
    Ok(serde_json::json!({ "ok": true }))
}

pub(crate) fn cancel_review_http(state: &SharedState, pr_id: i64) -> Result<serde_json::Value, String> {
    if let Some(job) = state
        .review_jobs
        .lock()
        .map_err(|_| "review job lock poisoned".to_string())?
        .get(&pr_id)
    {
        for cancel in job.cancel_by_run.values() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
    with_db_raw(state, |db| db.cancel_review_for_pr(pr_id))?;
    Ok(serde_json::json!({ "ok": true }))
}

pub(crate) fn mark_pr_review_seen_http(state: &SharedState, pr_id: i64) -> Result<serde_json::Value, String> {
    with_db_raw(state, |db| {
        db.mark_pending_seen_for_pr(
            pr_id,
            &[
                "review_ready",
                "review_failed",
                "review_result_failed",
                "review_requests_restore_failed",
                "manual_review_needed",
            ],
        )
    })?;
    Ok(serde_json::json!({ "ok": true }))
}

pub(crate) fn decide_review_http(
    request: &Request,
    state: &SharedState,
    pr_id: i64,
) -> Result<serde_json::Value, String> {
    let payload: DecideReviewRequest = serde_json::from_slice(&request.body).map_err(|error| error.to_string())?;
    let (run, full_name, number, installation_id, manual_selection) = with_db_raw(state, |db| {
        let run = db
            .latest_review_run(pr_id)?
            .ok_or("no review run for this PR".to_string())?;
        let manual_selection = db.review_requires_finding_selection(run.id)?;
        if payload.run_id.is_some_and(|id| id != run.id) || (manual_selection && payload.run_id.is_none()) {
            return Err("The selected review run changed. Reload before publishing.".to_string());
        }
        db.guard_finding_decision(pr_id, run.id, payload.selection_revision, || {
            let run = db.review_run_by_id(run.id)?.ok_or("Review run not found")?;
            let (_repo_id, full_name, number, _head) = db.pr_review_coords(pr_id)?;
            Ok((run, full_name, number, db.installation_id()?, manual_selection))
        })
    })?;
    if run.status != "preview" {
        return Err("This review is not awaiting a decision.".to_string());
    }
    if matches!(payload.decision.as_str(), "approve_post" | "request_changes") {
        let author = with_db_raw(state, |db| db.pr_author(pr_id))?;
        if gitlab::current_login().is_some_and(|login| login.eq_ignore_ascii_case(&author)) {
            return Err("GitLab does not allow approving or requesting changes on your own merge request. Use Comment, Mark reviewed, or Skip instead.".to_string());
        }
    }
    if payload.decision == "request_changes"
        && !crate::review::findings::actionable(run.comments_json.as_deref(), run.review_body.as_deref())
    {
        return Err(
            "Request changes requires at least one selected finding. Questions can be published as comments.".into(),
        );
    }
    let (event, run_status, lane, reason) = match payload.decision.as_str() {
        "approve_post" => (Some("APPROVE"), "posted", "posted", "Approved & posted"),
        "request_changes" => (Some("REQUEST_CHANGES"), "posted", "posted", "Changes requested"),
        "comment" => (Some("COMMENT"), "posted", "posted", "Review posted (comment)"),
        "mark_reviewed" => (None, "approved", "approved", "Reviewed — no issues"),
        "skip" => (None, "skipped", "skipped", "Review skipped"),
        other => return Err(format!("unknown decision: {other}")),
    };
    let mut final_reason = reason.to_string();
    let has_findings = review_has_findings(run.comments_json.as_deref(), run.review_body.as_deref());
    let repo_path = state.config.repos_dir.join(&full_name);
    let mut posted_review_id = None;
    if let Some(event) = event {
        with_db_raw(state, |db| {
            validate_live_review_scope(db, run.id, &full_name, number, &run.head_sha, &repo_path)
        })?;
        let draft = compose_review_post(&full_name, number, &run, event, manual_selection);
        let valid_json = serde_json::json!({ "comments": draft.comments }).to_string();
        let evidence_warnings = with_db_raw(state, |db| db.review_evidence_warnings(run.id))?;
        let should_post = validate_review_post_draft(&run, event, &draft)? || !evidence_warnings.is_empty();
        if !should_post && draft.suppressed_count > 0 {
            final_reason = "Findings already posted by another OpenCodeReview run".to_string();
        }
        let post_marker =
            should_post.then(|| review_post_marker(&full_name, number, run.id, &run.head_sha, &installation_id));
        let post = if let Some(marker) = post_marker.as_deref() {
            Some(build_review_payload(
                &full_name,
                &run.head_sha,
                event,
                &draft.body,
                Some(&valid_json),
                Some(marker),
                &evidence_warnings,
            )?)
        } else {
            None
        };
        let began = with_db_raw(state, |db| {
            db.try_begin_review_post(
                run.id,
                pr_id,
                &run.head_sha,
                event,
                &payload.decision,
                "you",
                payload.selection_revision,
            )
        })?;
        if !began {
            return Err("This review or its finding selection changed, or publication is already in progress. Reload before retrying.".to_string());
        }
        let _ = with_db_raw(state, |db| {
            db.set_review_run_comments_json(run.id, &draft.annotated_comments_json)
        });
        if let Some(post) = post {
            if let Err(error) = with_db_raw(state, |db| {
                validate_live_review_scope(db, run.id, &full_name, number, &run.head_sha, &repo_path)
            }) {
                let _ = with_db_raw(state, |db| db.reset_review_posting(run.id));
                return Err(error);
            }
            posted_review_id = publish_review_with_recovery(
                || {
                    let snapshot = gitlab::snapshot_review_requests(&full_name, number)?;
                    with_db_raw(state, |db| {
                        db.prepare_review_request_restoration(
                            run.id,
                            &snapshot,
                            post_marker.as_deref().unwrap_or_default(),
                        )
                    })?;
                    let prepared = gitlab::prepare_review(&post)?;
                    with_db_raw(state, |db| {
                        validate_live_review_scope(db, run.id, &full_name, number, &run.head_sha, &repo_path)
                    })?;
                    Ok(prepared)
                },
                |prepared| gitlab::post_review(&full_name, number, prepared),
                || gitlab::find_review_id_by_marker(&full_name, number, post_marker.as_deref().unwrap_or_default()),
                || with_db_raw(state, |db| db.reset_review_posting(run.id)),
            )?;
            if let Some(review_id) = posted_review_id {
                // Recovery can find the marker if persisting the accepted ID fails.
                let _ = with_db_raw(state, |db| db.accept_review_request_restoration(run.id, review_id));
                let _ = with_db_raw(state, |db| {
                    db.retire_queue_acknowledgement(pr_id, &run.head_sha, Some(run.id))
                });
                spawn_queue_acknowledgements(state.config.db.clone());
                if has_findings {
                    let db_handle = state.config.db.clone();
                    let retire_repo = full_name.clone();
                    let findings_run_id = run.id;
                    thread::spawn(move || {
                        if let Ok(db) = db_handle.open() {
                            retire_clean_comments_after_findings(&db, &retire_repo, number, findings_run_id);
                        }
                    });
                }
            }
            if matches!(event, "APPROVE" | "REQUEST_CHANGES") {
                if let Some(review_id) = posted_review_id {
                    let _ = with_db_raw(state, |db| db.set_ocr_review(pr_id, review_id, event));
                }
            }
        }
        with_db_raw(state, |db| {
            confirm_posted_review_live_head(
                db,
                run.id,
                pr_id,
                &full_name,
                number,
                &run.head_sha,
                &repo_path,
                "Review posted for a head superseded during publication",
            )
        })?;
    }
    let github_review_url = posted_review_id.map(|id| {
        format!(
            "https://{}/{full_name}/-/merge_requests/{number}#note_{id}",
            gitlab::gitlab_host()
        )
    });
    let (observation, effective_state, publish_current_result) =
        effective_review_result_for_decision(&payload.decision, has_findings);
    let finalized = with_db_raw(state, |db| {
        let finalize = || {
            db.finalize_review_decision_or_supersede_if_stale(
                run.id,
                pr_id,
                &payload.decision,
                run_status,
                lane,
                &final_reason,
                event,
                &run.head_sha,
                Some("you"),
                "Review decision superseded by a new PR head",
            )
        };
        if event.is_none() {
            db.guard_finding_decision(pr_id, run.id, payload.selection_revision, finalize)
        } else {
            finalize()
        }
    })?;
    if !finalized {
        return Err("Review decision superseded by a new PR head.".to_string());
    }
    let prior = with_db_raw(state, |db| db.review_result(pr_id))?;
    if let Ok(result) = with_db_raw(state, |db| {
        reserve_effective_review_result(
            db,
            &run,
            &full_name,
            number,
            observation,
            effective_state,
            run.reviewed_head_sha.as_deref().unwrap_or(run.head_sha.as_str()),
            prior.as_ref(),
            posted_review_id,
            github_review_url.as_deref(),
            publish_current_result,
            None,
        )
    }) {
        if result.publication_status == "pending" {
            let _ = with_db_raw(state, |db| {
                publish_effective_review_result(db, &result, &full_name, number, &repo_path)
            });
        }
    }
    Ok(serde_json::json!({ "ok": true }))
}

pub(crate) fn effective_review_result_for_decision(
    decision: &str,
    has_findings: bool,
) -> (&'static str, &'static str, bool) {
    match decision {
        "skip" => (if has_findings { "findings" } else { "clean" }, "skipped", false),
        "mark_reviewed" if has_findings => ("findings", "findings_local", false),
        _ if has_findings => ("findings", "findings_posted", true),
        _ => ("clean", "clean", true),
    }
}

pub(crate) fn retry_review_result_http(state: &SharedState, pr_id: i64) -> Result<serde_json::Value, String> {
    let (db_handle, result, full_name, number) = with_db_raw(state, |db| {
        let result = db.retry_review_result(pr_id)?;
        let (_repo_id, full_name, number, _head) = db.pr_review_coords(pr_id)?;
        Ok((db.handle(), result, full_name, number))
    })?;
    let repo_path = state.config.repos_dir.join(&full_name);
    thread::spawn(move || {
        if let Ok(db) = db_handle.open() {
            let _ = publish_effective_review_result(&db, &result, &full_name, number, &repo_path);
        }
    });
    Ok(serde_json::json!({ "ok": true }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewTargetChange {
    Current,
    HeadChanged,
    Closed,
}

pub(crate) fn classify_review_target(live: &gitlab::LivePrState, expected_head: &str) -> ReviewTargetChange {
    if live.state != "OPEN" {
        ReviewTargetChange::Closed
    } else if live.head_sha != expected_head {
        ReviewTargetChange::HeadChanged
    } else {
        ReviewTargetChange::Current
    }
}

pub(crate) fn is_review_target_changed_error(error: &str) -> bool {
    error == "PR changed during preparation"
        || error.starts_with("PR state/head changed")
        || error.starts_with("PR head changed")
        || error.starts_with("PR base/head scope changed")
}

pub(crate) fn record_review_target_change(
    db: &Database,
    run_id: i64,
    pr_id: i64,
    repo_id: i64,
    auto_enabled: bool,
    change: ReviewTargetChange,
) -> Result<(), String> {
    let (lane, reason) = match change {
        ReviewTargetChange::Closed => ("closed", "PR closed or merged before review preparation"),
        ReviewTargetChange::HeadChanged => (
            if auto_enabled { "open" } else { "manual_required" },
            "Review superseded by a newer PR head before preparation",
        ),
        ReviewTargetChange::Current => return Ok(()),
    };
    db.supersede_review_run_without_changing_pr(run_id, reason)?;
    db.set_pr_lane(pr_id, lane, Some(reason))?;
    if change == ReviewTargetChange::Closed {
        db.resolve_all_pending_for_pr(pr_id)?;
    } else if lane == "manual_required" {
        db.upsert_pending_action("manual_review_needed", repo_id, pr_id, "Manual review needed", reason)?;
    }
    Ok(())
}
