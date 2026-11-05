use super::*;

/// Advisory lock keys for work one server instance does at a time when several share a database.
pub(crate) mod exclusive {
    pub(crate) const POLL: i64 = 0x6f63_725f_706f_6c6c; // "ocr_poll"
    pub(crate) const REINDEX: i64 = 0x6f63_725f_7265_6978; // "ocr_reix"
    pub(crate) const RETENTION: i64 = 0x6f63_725f_7265_746e; // "ocr_retn"
    pub(crate) const RESULT_RECONCILIATION: i64 = 0x6f63_725f_7265_636e; // "ocr_recn"
    pub(crate) const CLEAN_RESULT_RETRY: i64 = 0x6f63_725f_636c_6e72; // "ocr_clnr"
    pub(crate) const QUEUE_ACKNOWLEDGEMENTS: i64 = 0x6f63_725f_6163_6b73; // "ocr_acks"
    pub(crate) const ORPHAN_REAPER: i64 = 0x6f63_725f_7265_6170; // "ocr_reap"
}

/// This server's identity among the instances sharing the database.
pub(crate) struct Instance {
    pub(crate) id: String,
    pub(crate) host: String,
}

impl Instance {
    pub(crate) fn new() -> Result<Self, String> {
        Ok(Self {
            id: crate::sandbox::secret().map_err(|error| error.to_string())?,
            host: host_name(),
        })
    }
}

fn host_name() -> String {
    let mut name = [0u8; 256];
    if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } != 0 {
        return "unknown".into();
    }
    let length = name.iter().position(|byte| *byte == 0).unwrap_or(name.len());
    String::from_utf8_lossy(&name[..length]).into_owned()
}

/// Keeps this instance marked alive, so other instances leave its reviews and indexing alone.
pub(crate) fn start_instance_heartbeat(db_handle: Db, instance: Instance) {
    thread::spawn(move || loop {
        thread::sleep(crate::db::INSTANCE_HEARTBEAT);
        if let Err(error) = db_handle
            .open()
            .and_then(|db| db.heartbeat_instance(&instance.id, &instance.host))
        {
            eprintln!("instance heartbeat failed: {error}");
        }
    });
}

/// Interrupts reviews and indexing whose instance stopped (its heartbeat ran out), drops their
/// per-review indexes and settles their half-done posts. One instance does it at a time.
pub(crate) fn start_orphan_reaper(db_handle: Db, repos_dir: PathBuf) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(crate::db::INSTANCE_TIMEOUT_MS as u64 / 2));
        let _ = db_handle.run_exclusive(exclusive::ORPHAN_REAPER, || {
            let db = db_handle.open()?;
            db.reclaim_orphaned_work()?;
            db.reap_snapshot_indexes()?;
            drop(db);
            // Posts and result publications a stopped instance left half done; under the startup
            // reconciliation's key, so the two never post the same thing at once.
            db_handle.run_exclusive(exclusive::RESULT_RECONCILIATION, || {
                reconcile_review_results(&db_handle, &repos_dir)
            })?;
            Ok::<_, String>(())
        });
    });
}

/// Cancels reviews this instance owns in the database but has no job for, so a run whose start or
/// finish failed to record its outcome does not block its PR until this instance restarts.
pub(crate) fn start_untracked_run_sweeper(state: SharedState) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(60));
        // Held while sweeping: a review start records its run and its job under this lock.
        let Ok(jobs) = state.review_jobs.lock() else {
            continue;
        };
        let running: Vec<i64> = jobs.keys().copied().collect();
        if let Err(error) = state
            .config
            .db
            .open()
            .and_then(|db| db.cancel_untracked_review_runs(&running))
        {
            eprintln!("untracked review sweep failed: {error}");
        }
    });
}

/// Every instance starts queued reviews while it has free review slots, so the queue spreads
/// across instances. The PR lock in `start_review_skeleton_with` keeps two instances from both
/// starting the same PR. PRs queued by an author command wait for the poll, which checks them.
pub(crate) fn start_review_queue_drain(state: SharedState) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(15));
        let Ok((settings, queued)) = with_db_read_raw(&state, |db| Ok((db.settings()?, db.queued_pr_ids()?))) else {
            continue;
        };
        for pr_id in queued {
            if active_review_count(&state) >= settings.review_concurrency as usize {
                break;
            }
            if with_db_read_raw(&state, |db| db.comment_queue_requires_eligibility(pr_id)).unwrap_or(true) {
                continue;
            }
            let _ = start_review_job(&state, pr_id, false);
        }
    });
}

pub(crate) fn start_runtime_cleanup_worker() {
    thread::spawn(|| loop {
        thread::sleep(Duration::from_secs(60 * 60));
        cleanup_runtime_artifacts();
    });
}

pub(crate) fn start_review_result_reconciliation(db_handle: Db, repos_dir: PathBuf) {
    thread::spawn(move || {
        let exclusive = db_handle.clone();
        let _ = exclusive.run_exclusive(exclusive::RESULT_RECONCILIATION, || {
            reconcile_review_results(&db_handle, &repos_dir)
        });
    });
}

fn reconcile_review_results(db_handle: &Db, repos_dir: &Path) {
    let Ok(db) = db_handle.open() else {
        return;
    };
    reconcile_interrupted_review_posts(&db, repos_dir);
    // A "reviewing" comment left behind by a review that has since finished.
    if db.retire_finished_queue_acknowledgements().unwrap_or(0) > 0 {
        spawn_queue_acknowledgements(db_handle.clone());
    }
    let enabled = db
        .settings()
        .map(|settings| settings.review_result_mode == "result_comment")
        .unwrap_or(false);
    let results = db.claim_abandoned_review_results().unwrap_or_default();
    for result in results {
        let Ok((repo_id, full_name, number, _head)) = db.pr_review_coords(result.pr_id) else {
            continue;
        };
        let repo_path = repos_dir.join(&full_name);
        if enabled {
            let _ = publish_reserved_review_result(&db, &result, &full_name, number, &repo_path, true);
        } else {
            reconcile_silent_review_result(&db, &result, repo_id, &full_name, number, &repo_path);
        }
    }
}

pub(crate) fn reconcile_silent_review_result(
    db: &Database,
    result: &crate::db::ReviewResult,
    repo_id: i64,
    full_name: &str,
    number: i64,
    repo_path: &Path,
) {
    if verify_result_live_head(
        db,
        result,
        repo_id,
        full_name,
        number,
        repo_path,
        "Review result superseded by a new PR head",
    )
    .is_err()
    {
        return;
    }
    // Only a comment carrying this run's publication marker proves the reserved
    // result reached GitHub; an older clean comment shares the ownership marker.
    let comment = match result.run_id {
        Some(run_id) => find_clean_comment(
            &mut gitlab::GitLabStatusComments,
            full_name,
            number,
            &result.marker_key,
            run_id,
        ),
        None => Ok(None),
    };
    match comment {
        Ok(Some(comment_id)) => {
            let _ = db.finish_review_result_publication(result.id, comment_id);
            let _ = finish_published_result_lane(db, result);
            let _ = db.retire_queue_acknowledgement(result.pr_id, &result.observed_head_sha, result.run_id);
            spawn_queue_acknowledgements(db.handle());
        }
        Ok(None) => record_review_result_failure(
            db,
            result,
            repo_id,
            "Result publication was interrupted before GitHub accepted it. Re-enable result comments and retry.",
        ),
        Err(error) => record_review_result_failure(
            db,
            result,
            repo_id,
            &format!("Could not reconcile interrupted result publication: {error}"),
        ),
    }
}

pub(crate) const CLEAN_RESULT_RETRY_INTERVAL_MS: i64 = 10 * 60 * 1000;

/// Automatic retry of failed clean-result publications. Adoption by run marker
/// makes republication idempotent, a failed attempt refreshes `updated_at` and
/// so spaces the next attempt by the interval, and a stale head or closed PR is
/// superseded by the normal live-head check, which ends retries.
pub(crate) fn retry_failed_clean_results_with(
    db: &Database,
    repos_dir: &Path,
    now: i64,
    mut publish: impl FnMut(&Database, &ReviewResult, &str, i64, &Path) -> Result<(), String>,
) {
    let enabled = db
        .settings()
        .map(|settings| settings.review_result_mode == "result_comment")
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let failed_before = now - CLEAN_RESULT_RETRY_INTERVAL_MS;
    for result in db.failed_clean_results_due(failed_before).unwrap_or_default() {
        let Ok(Some(pending)) = db.claim_due_failed_clean_result(&result, failed_before) else {
            continue;
        };
        let Ok((_, full_name, number, _)) = db.pr_review_coords(result.pr_id) else {
            continue;
        };
        let _ = publish(db, &pending, &full_name, number, &repos_dir.join(&full_name));
    }
}

pub(crate) fn retry_failed_clean_results(db: &Database, repos_dir: &Path) {
    retry_failed_clean_results_with(db, repos_dir, crate::db::now_millis(), publish_effective_review_result);
}

static CLEAN_RESULT_RETRY_RUNNING: AtomicBool = AtomicBool::new(false);

/// Runs `task` on a background thread unless one guarded by `gate` is still
/// running; returns whether it started.
pub(crate) fn spawn_single_flight(gate: &'static AtomicBool, task: impl FnOnce() + Send + 'static) -> bool {
    if gate.swap(true, Ordering::AcqRel) {
        return false;
    }
    thread::spawn(move || {
        struct Reset<'a>(&'a AtomicBool);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _reset = Reset(gate);
        task();
    });
    true
}

/// Failed clean-result publications are retried on a background thread so the
/// poll never waits on GitHub calls; a retry already in flight is not doubled.
pub(crate) fn spawn_failed_clean_result_retry(db_handle: Db, repos_dir: PathBuf) {
    spawn_single_flight(&CLEAN_RESULT_RETRY_RUNNING, move || {
        let _ = db_handle.run_exclusive(exclusive::CLEAN_RESULT_RETRY, || {
            if let Ok(db) = db_handle.open() {
                retry_failed_clean_results(&db, &repos_dir);
            }
        });
    });
}

pub(crate) fn reconcile_interrupted_review_posts(db: &Database, repos_dir: &Path) {
    let Ok(installation_id) = db.installation_id() else {
        return;
    };
    for post in db.interrupted_review_posts().unwrap_or_default() {
        let repo_path = repos_dir.join(&post.full_name);
        let marker = review_post_marker(
            &post.full_name,
            post.number,
            post.run_id,
            &post.head_sha,
            &installation_id,
        );
        let review_id = match gitlab::find_review_id_by_marker(&post.full_name, post.number, &marker) {
            Ok(Some(review_id)) => review_id,
            Ok(None) => {
                let _ = db.reset_review_posting(post.run_id);
                continue;
            }
            Err(_) => continue,
        };
        let _ = db.accept_review_request_restoration(post.run_id, review_id);
        let _ = db.retire_queue_acknowledgement(post.pr_id, &post.head_sha, Some(post.run_id));
        // Under the acknowledgements' own lock, which another instance's pass may hold.
        spawn_queue_acknowledgements(db.handle());
        if db
            .review_run_by_id(post.run_id)
            .ok()
            .flatten()
            .is_some_and(|run| review_has_findings(run.comments_json.as_deref(), run.review_body.as_deref()))
        {
            retire_clean_comments_after_findings(db, &post.full_name, post.number, post.run_id);
        }
        if confirm_posted_review_live_head(
            db,
            post.run_id,
            post.pr_id,
            &post.full_name,
            post.number,
            &post.head_sha,
            &repo_path,
            "Recovered GitHub review belongs to a superseded PR head",
        )
        .is_err()
        {
            continue;
        }
        if matches!(post.event.as_str(), "APPROVE" | "REQUEST_CHANGES") {
            let _ = db.set_ocr_review(post.pr_id, review_id, &post.event);
        }
        let Ok(Some(run)) = db.review_run_by_id(post.run_id) else {
            continue;
        };
        let has_findings = review_has_findings(run.comments_json.as_deref(), run.review_body.as_deref());
        let prior = db.review_result(post.pr_id).ok().flatten();
        let review_url = format!(
            "https://{}/{}/-/merge_requests/{}#note_{review_id}",
            gitlab::gitlab_host(),
            post.full_name,
            post.number
        );
        let effective_state = if has_findings { "findings_posted" } else { "clean" };
        let Ok(result) = reserve_effective_review_result(
            db,
            &run,
            &post.full_name,
            post.number,
            if has_findings { "findings" } else { "clean" },
            effective_state,
            run.reviewed_head_sha.as_deref().unwrap_or(run.head_sha.as_str()),
            prior.as_ref(),
            Some(review_id),
            Some(&review_url),
            true,
            None,
        ) else {
            continue;
        };
        if has_findings || result.publication_status != "pending" {
            let lane = if post.actor == "auto" { "auto_posted" } else { "posted" };
            let reason = match post.event.as_str() {
                "APPROVE" => "Approved & posted",
                "REQUEST_CHANGES" => "Changes requested",
                _ => "Review posted (comment)",
            };
            let Ok(finalized) = db.finalize_review_decision_or_supersede_if_stale(
                post.run_id,
                post.pr_id,
                &post.decision,
                "posted",
                lane,
                reason,
                Some(&post.event),
                &post.head_sha,
                Some(&post.actor),
                "Recovered review superseded by a new PR head",
            ) else {
                continue;
            };
            if !finalized {
                let _ = db.supersede_review_result(result.id, "Recovered review result superseded by a new PR head");
                continue;
            }
        }
        if result.publication_status == "pending" {
            let _ = publish_effective_review_result(db, &result, &post.full_name, post.number, &repo_path);
        }
    }
}

pub(crate) fn cleanup_runtime_artifacts() {
    let temp_dir = std::env::temp_dir();
    cleanup_temp_entries(&temp_dir, "ocr-review-pr", Duration::from_secs(6 * 60 * 60));
    cleanup_temp_entries(&temp_dir, "ocr-rgit-", Duration::from_secs(60 * 60));
    cleanup_temp_entries(&temp_dir, "ocr-gh-capture-", Duration::from_secs(60 * 60));
}

pub(crate) fn cleanup_temp_entries(dir: &Path, prefix: &str, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(prefix) || !is_older_than(&entry.path(), max_age) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(path);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) fn is_older_than(path: &Path, max_age: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age > max_age)
        .unwrap_or(false)
}

// Restoration is independent of review/result publication: it must never repost a review.
pub(crate) fn start_review_request_restoration(db_handle: Db) {
    thread::spawn(move || {
        loop {
            // Connections are taken per step and never held across GitHub calls.
            let claimed = db_handle.open().and_then(|db| db.claim_review_request_restoration());
            if let Ok(Some(job)) = claimed {
                let restored = (|| {
                    let review_id = match job.review_id {
                        Some(id) => Some(id),
                        None => gitlab::find_review_id_by_marker(&job.full_name, job.number, &job.marker)?,
                    };
                    let Some(review_id) = review_id else {
                        return Ok(true);
                    };
                    db_handle
                        .open()?
                        .accept_review_request_restoration(job.run_id, review_id)?;
                    gitlab::restore_review_requests(&job.full_name, job.number, review_id, &job.snapshot)
                })();
                let (status, error) = match restored {
                    Ok(true) => ("done", None),
                    Ok(false) => ("changed", Some("Other review activity occurred while OpenCodeReview was publishing. Check whether a human review still needs to be requested on GitHub.".to_string())),
                    Err(error) if job.attempts < 3 => ("pending", Some(error)),
                    Err(error) => ("failed", Some(format!("Review request restoration failed after three attempts. Check the requested reviewers on GitHub. {error}"))),
                };
                let _ = db_handle
                    .open()
                    .and_then(|db| db.finish_review_request_restoration(job.run_id, status, error.as_deref()));
            }
            thread::sleep(Duration::from_secs(15));
        }
    });
}
