use super::*;

/// Preparation cannot publish a review. Only a failed POST needs marker recovery
/// and may retain the posting lock when GitLab acceptance cannot be determined.
pub(crate) fn publish_review_with_recovery<T>(
    prepare: impl FnOnce() -> Result<T, String>,
    post: impl FnOnce(T) -> Result<Option<i64>, String>,
    reconcile: impl FnOnce() -> Result<Option<i64>, String>,
    reset: impl FnOnce() -> Result<(), String>,
) -> Result<Option<i64>, String> {
    let prepared = match prepare() {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = reset();
            return Err(error);
        }
    };
    match post(prepared) {
        Ok(review_id) => Ok(review_id),
        Err(error) => match reconcile() {
            Ok(Some(review_id)) => Ok(Some(review_id)),
            Ok(None) => {
                let _ = reset();
                Err(error)
            }
            Err(reconcile_error) => Err(format!(
                "{error}; GitLab acceptance is ambiguous and OpenCodeReview kept this review locked for startup reconciliation: {reconcile_error}"
            )),
        },
    }
}

pub(crate) fn validate_review_scope_outcome(
    db: &Database,
    run_id: i64,
    full_name: &str,
    number: i64,
    expected_head: &str,
    repo_path: &Path,
) -> Result<(), LiveHeadValidationError> {
    let live = gitlab::live_pr_state(full_name, number).map_err(LiveHeadValidationError::Unavailable)?;
    if live.state != "OPEN" || live.head_sha != expected_head {
        return Err(LiveHeadValidationError::Stale(
            "PR state/head changed; refresh the review".into(),
        ));
    }
    let previous = db
        .review_run_scope(run_id)
        .map_err(LiveHeadValidationError::Unavailable)?
        .ok_or_else(|| {
            LiveHeadValidationError::Unavailable(
                "Review base metadata is unavailable. Run a full review before publication.".into(),
            )
        })?;
    if !crate::review::scope::equivalent(repo_path, &previous, &live.review_scope(), &AtomicBool::new(false)) {
        return Err(LiveHeadValidationError::Stale(
            "PR base/diff scope changed; run a full review before publication".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_live_review_scope(
    db: &Database,
    run_id: i64,
    full_name: &str,
    number: i64,
    expected_head: &str,
    repo_path: &Path,
) -> Result<(), String> {
    validate_review_scope_outcome(db, run_id, full_name, number, expected_head, repo_path).map_err(
        |error| match error {
            LiveHeadValidationError::Stale(error) | LiveHeadValidationError::Unavailable(error) => error,
        },
    )
}

pub(crate) fn confirm_posted_review_live_head(
    db: &Database,
    run_id: i64,
    pr_id: i64,
    full_name: &str,
    number: i64,
    expected_head: &str,
    repo_path: &Path,
    stale_reason: &str,
) -> Result<(), String> {
    match validate_review_scope_outcome(db, run_id, full_name, number, expected_head, repo_path) {
        Ok(()) => Ok(()),
        Err(LiveHeadValidationError::Stale(error)) => {
            db.mark_review_superseded_after_post(run_id, pr_id, stale_reason)?;
            Err(error)
        }
        Err(LiveHeadValidationError::Unavailable(error)) => Err(format!(
            "{error}; GitLab review remains locked for startup reconciliation"
        )),
    }
}

pub(crate) fn review_result_body(
    full_name: &str,
    number: i64,
    installation_id: &str,
    run_id: i64,
    observed_head_sha: &str,
    reviewed_head_sha: &str,
    observation: &str,
    effective_state: &str,
    prior_result_head_sha: Option<&str>,
    reviewer_version: &str,
    skill_version: &str,
    evidence_warnings: &[crate::review::warnings::EvidenceWarning],
) -> String {
    let key = crate::db::result_marker_key(full_name, number, installation_id);
    let run_marker = status_publication_marker(run_id);
    let inherited_from = prior_result_head_sha.unwrap_or("none");
    let data = format!(
        "<!-- ocr-result-data:v1 head={observed_head_sha} reviewed_head={reviewed_head_sha} observation={observation} state={effective_state} inherited_from={inherited_from} reviewer={reviewer_version} skill={skill_version} -->"
    );
    let host = gitlab::gitlab_host();
    let commit_url = |sha: &str| format!("https://{host}/{full_name}/-/commit/{sha}");
    let visible = match (observation, effective_state) {
        ("no_new_changes", "clean") => format!(
            "### ✅ Nenhum problema novo\n\nVerifiquei [`{}`]({}) e não encontrei alterações novas desde a revisão limpa de [`{}`]({}).",
            short_sha(observed_head_sha),
            commit_url(observed_head_sha),
            short_sha(reviewed_head_sha),
            commit_url(reviewed_head_sha)
        ),
        _ => format!(
            "### ✅ Não encontrei problemas\n\nRevisei [`{}`]({}). Nada aqui precisa de ação.",
            short_sha(reviewed_head_sha),
            commit_url(reviewed_head_sha)
        ),
    };
    let visible = crate::review::warnings::append(&visible, evidence_warnings);
    format!("{key}\n{data}\n{run_marker}\n\n{visible}\n\n{}", ocr_footer())
}

pub(crate) fn reserve_effective_review_result(
    db: &Database,
    run: &crate::db::ReviewRun,
    full_name: &str,
    number: i64,
    observation: &str,
    effective_state: &str,
    reviewed_head_sha: &str,
    prior: Option<&ReviewResult>,
    github_review_id: Option<i64>,
    github_review_url: Option<&str>,
    publish_current_result: bool,
    inherited_actionable_run_id: Option<i64>,
) -> Result<ReviewResult, String> {
    let installation_id = db.installation_id()?;
    let reviewer_version = format!("ocr-{}-review-v1", env!("CARGO_PKG_VERSION"));
    let skill_version = format!("v{}", db.active_skill_version_id());
    let publish_result_comment = should_publish_review_result_comment(publish_current_result, effective_state);
    let evidence_warnings = db.review_evidence_warnings(run.id)?;
    let body = publish_result_comment.then(|| {
        review_result_body(
            full_name,
            number,
            &installation_id,
            run.id,
            &run.head_sha,
            reviewed_head_sha,
            observation,
            effective_state,
            (observation == "no_new_changes")
                .then(|| prior.map(|result| result.observed_head_sha.as_str()))
                .flatten(),
            &reviewer_version,
            &skill_version,
            &evidence_warnings,
        )
    });
    let enabled = db.settings()?.review_result_mode == "result_comment";
    let actionable_run_id = actionable_run_id_for_result(
        observation,
        effective_state,
        run.id,
        inherited_actionable_run_id.or_else(|| prior.and_then(|result| result.actionable_run_id.or(result.run_id))),
    );
    db.reserve_review_result(&ReviewResultInput {
        pr_id: run.pr_id,
        run_id: run.id,
        actionable_run_id,
        observed_head_sha: &run.head_sha,
        reviewed_head_sha,
        prior_result_state: prior.map(|result| result.effective_state.as_str()),
        prior_result_head_sha: prior.map(|result| result.observed_head_sha.as_str()),
        observation,
        effective_state,
        publication_status: if enabled && body.is_some() {
            "pending"
        } else {
            "disabled"
        },
        body: body.as_deref(),
        github_review_id,
        github_review_url,
        reviewer_version: &reviewer_version,
        skill_version: &skill_version,
    })
}

pub(crate) fn should_publish_review_result_comment(publish_current_result: bool, effective_state: &str) -> bool {
    publish_current_result && effective_state == "clean"
}

pub(crate) fn actionable_run_id_for_result(
    observation: &str,
    effective_state: &str,
    run_id: i64,
    prior_actionable_run_id: Option<i64>,
) -> Option<i64> {
    (effective_state == "awaiting_action").then(|| {
        if observation == "no_new_changes" {
            prior_actionable_run_id.unwrap_or(run_id)
        } else {
            run_id
        }
    })
}

pub(crate) struct LegacyEffectiveResult {
    pub(crate) effective_state: &'static str,
    pub(crate) reviewed_head_sha: String,
    pub(crate) actionable_run_id: Option<i64>,
}

pub(crate) fn legacy_effective_result(outcome: &crate::db::LegacyReviewOutcome) -> Option<LegacyEffectiveResult> {
    let has_findings = review_has_findings(outcome.comments_json.as_deref(), outcome.review_body.as_deref());
    let effective_state = match outcome.decision.as_deref() {
        Some("skip") => "skipped",
        Some("mark_reviewed") => {
            if has_findings {
                "findings_local"
            } else {
                "clean"
            }
        }
        Some("approve_post" | "request_changes" | "comment") => {
            if has_findings {
                "findings_posted"
            } else {
                "clean"
            }
        }
        _ if matches!(outcome.status.as_str(), "preview" | "superseded") => "awaiting_action",
        _ if matches!(outcome.status.as_str(), "posted" | "approved") => {
            if has_findings {
                "findings_posted"
            } else {
                "clean"
            }
        }
        _ => return None,
    };
    Some(LegacyEffectiveResult {
        effective_state,
        reviewed_head_sha: outcome
            .reviewed_head_sha
            .clone()
            .unwrap_or_else(|| outcome.head_sha.clone()),
        actionable_run_id: (effective_state == "awaiting_action").then_some(outcome.run_id),
    })
}

pub(crate) fn finish_published_result_lane(db: &Database, result: &ReviewResult) -> Result<bool, String> {
    let Some(run_id) = result.run_id else {
        return Ok(true);
    };
    if db.review_run_is_decided(run_id)? {
        return Ok(true);
    }
    match (result.observation.as_str(), result.effective_state.as_str()) {
        (_, "clean") => db.finalize_review_decision_if_current_head(
            run_id,
            result.pr_id,
            "clean_result",
            "approved",
            "reviewed_cleanly",
            "Reviewed cleanly — result posted",
            None,
            &result.observed_head_sha,
            Some("auto"),
        ),
        _ => Ok(true),
    }
}

pub(crate) fn supersede_result_after_head_change(db: &Database, result: &ReviewResult, reason: &str) {
    let _ = db.supersede_review_result_after_stale_head(result.id, result.pr_id, reason);
    if let Some(run_id) = result.run_id {
        let _ = db.mark_review_superseded_after_post(run_id, result.pr_id, reason);
    }
}

pub(crate) fn verify_result_live_head(
    db: &Database,
    result: &ReviewResult,
    repo_id: i64,
    full_name: &str,
    number: i64,
    repo_path: &Path,
    stale_reason: &str,
) -> Result<(), String> {
    match result
        .run_id
        .ok_or_else(|| {
            LiveHeadValidationError::Unavailable("Review scope is unavailable; re-review before publishing".into())
        })
        .and_then(|run_id| {
            validate_review_scope_outcome(db, run_id, full_name, number, &result.observed_head_sha, repo_path)
        }) {
        Ok(()) => Ok(()),
        Err(LiveHeadValidationError::Stale(error)) => {
            supersede_result_after_head_change(db, result, stale_reason);
            Err(error)
        }
        Err(LiveHeadValidationError::Unavailable(error)) => {
            record_review_result_failure(
                db,
                result,
                repo_id,
                &format!("Result publication verification failed: {error}"),
            );
            Err(error)
        }
    }
}

pub(crate) fn verify_published_result_live_head(
    db: &Database,
    result: &ReviewResult,
    full_name: &str,
    number: i64,
    repo_path: &Path,
    stale_reason: &str,
) -> Result<(), String> {
    match result
        .run_id
        .ok_or_else(|| {
            LiveHeadValidationError::Unavailable("Review scope is unavailable; re-review before publishing".into())
        })
        .and_then(|run_id| {
            validate_review_scope_outcome(db, run_id, full_name, number, &result.observed_head_sha, repo_path)
        }) {
        Ok(()) | Err(LiveHeadValidationError::Unavailable(_)) => Ok(()),
        Err(LiveHeadValidationError::Stale(error)) => {
            supersede_result_after_head_change(db, result, stale_reason);
            Err(error)
        }
    }
}

pub(crate) fn publish_reserved_review_result(
    db: &Database,
    result: &ReviewResult,
    full_name: &str,
    number: i64,
    repo_path: &Path,
    already_posting: bool,
) -> Result<(), String> {
    if db.settings()?.review_result_mode != "result_comment" {
        let _ = db.disable_review_result_publication(result.id);
        return Err("review result publication is disabled".to_string());
    }
    let (repo_id, _, _, _) = db.pr_review_coords(result.pr_id)?;
    verify_result_live_head(
        db,
        result,
        repo_id,
        full_name,
        number,
        repo_path,
        "Review result superseded by a new PR head",
    )?;
    if !already_posting
        && !db.begin_review_result_publication(result.id, &result.observed_head_sha, result.body_hash.as_deref())?
    {
        return Err("result comment is already posting or no longer pending".to_string());
    }
    let body = result
        .body
        .as_deref()
        .ok_or_else(|| "reserved review result has no comment body".to_string())?;
    let run_id = result
        .run_id
        .ok_or_else(|| "reserved review result has no run".to_string())?;
    // Rows reserved before run markers existed post with the marker stamped on,
    // so their retry can adopt the comment by run instead of posting again.
    let body = with_status_publication_marker(body, &result.marker_key, run_id);
    let posted = replace_clean_comment(
        &mut gitlab::GitLabStatusComments,
        full_name,
        number,
        &result.marker_key,
        run_id,
        &body,
    );
    match posted {
        Ok(comment_id) => {
            db.finish_review_result_publication(result.id, comment_id)?;
            let _ = db.retire_queue_acknowledgement(result.pr_id, &result.observed_head_sha, result.run_id);
            spawn_queue_acknowledgements(db.handle());
            verify_published_result_live_head(
                db,
                result,
                full_name,
                number,
                repo_path,
                "Review result superseded during publication",
            )?;
            if !finish_published_result_lane(db, result)? {
                let error = "Review result superseded by a new PR head".to_string();
                supersede_result_after_head_change(db, result, &error);
                return Err(error);
            }
            db.resolve_pending_for_pr("review_result_failed", result.pr_id)?;
            db.resolve_pending_for_pr("review_failed", result.pr_id)?;
            Ok(())
        }
        Err(error) => {
            record_review_result_failure(db, result, repo_id, &format!("Result comment failed: {error}"));
            Err(error)
        }
    }
}

pub(crate) fn record_review_result_failure(db: &Database, result: &ReviewResult, repo_id: i64, reason: &str) {
    let run_is_undecided = db.fail_review_result_publication(result.id, reason).unwrap_or(false);
    let _ = db.upsert_pending_action(
        "review_result_failed",
        repo_id,
        result.pr_id,
        "Review result failed",
        reason,
    );
    if result.effective_state == "clean" && run_is_undecided {
        let _ = db.set_pr_lane(result.pr_id, "failed", Some(reason));
    } else if result.observation == "no_new_changes" && result.effective_state == "findings_posted" {
        let _ = finalize_inherited_result_without_comment(db, result);
    }
}

pub(crate) fn publish_effective_review_result(
    db: &Database,
    result: &ReviewResult,
    full_name: &str,
    number: i64,
    repo_path: &Path,
) -> Result<(), String> {
    if result.publication_status == "disabled" {
        return Ok(());
    }
    publish_reserved_review_result(db, result, full_name, number, repo_path, false)
}

pub(crate) fn finalize_inherited_result_without_comment(db: &Database, result: &ReviewResult) -> Result<(), String> {
    let Some(run_id) = result.run_id else {
        return Ok(());
    };
    let finalized = match result.effective_state.as_str() {
        "clean" => db.finalize_review_decision_or_supersede_if_stale(
            run_id,
            result.pr_id,
            "mark_reviewed",
            "approved",
            "approved",
            "Reviewed — no new PR changes",
            None,
            &result.observed_head_sha,
            Some("you"),
            "Inherited result superseded by a new PR head",
        ),
        "findings_posted" => {
            let decided_by = db.pr_decided_by(result.pr_id)?;
            let lane = if decided_by.as_deref() == Some("auto") {
                "auto_posted"
            } else {
                "posted"
            };
            db.finalize_review_decision_or_supersede_if_stale(
                run_id,
                result.pr_id,
                "inherit_result",
                "posted",
                lane,
                "No new PR-owned changes — prior findings still apply",
                None,
                &result.observed_head_sha,
                decided_by.as_deref(),
                "Inherited result superseded by a new PR head",
            )
        }
        "findings_local" => db.finalize_review_decision_or_supersede_if_stale(
            run_id,
            result.pr_id,
            "mark_reviewed",
            "approved",
            "approved",
            "Prior findings were marked reviewed locally",
            None,
            &result.observed_head_sha,
            Some("you"),
            "Inherited result superseded by a new PR head",
        ),
        _ => db.finalize_review_decision_or_supersede_if_stale(
            run_id,
            result.pr_id,
            "skip",
            "skipped",
            "skipped",
            "No new PR-owned changes — prior non-clean result preserved",
            None,
            &result.observed_head_sha,
            Some("you"),
            "Inherited result superseded by a new PR head",
        ),
    }?;
    if !finalized {
        db.supersede_review_result(result.id, "Inherited result superseded by a new PR head")?;
    }
    Ok(())
}

pub(crate) fn mark_no_change_recovery_needed(db: &Database, inputs: &review::ReviewInputs, error: &str) {
    let reason = format!("No-change review needs manual recovery: {error}");
    let _ = db.set_pr_lane(inputs.pr_id, "manual_required", Some(&reason));
    let _ = db.upsert_pending_action(
        "manual_review_needed",
        inputs.repo_id,
        inputs.pr_id,
        "Manual review needed",
        &reason,
    );
}

/// One line per finished review saying what automatic publication did with it, so a review that
/// waits for a person when auto-post is on always says why.
fn log_auto_post(inputs: &review::ReviewInputs, outcome: &str) {
    eprintln!(
        "auto-post {}#{} run {}: {outcome}",
        inputs.full_name, inputs.number, inputs.run_id
    );
}

pub(crate) fn handle_completed_review(
    db: &Database,
    inputs: &review::ReviewInputs,
    no_new_changes: bool,
    has_findings: bool,
) {
    let Ok(Some(run)) = db.review_run_by_id(inputs.run_id) else {
        log_auto_post(inputs, "not attempted: the review run is no longer readable");
        return;
    };
    let prior = db.review_result(inputs.pr_id).ok().flatten();
    maybe_enqueue_pr_summary(db, inputs);
    let mode = db
        .settings()
        .map(|settings| settings.auto_post_mode)
        .unwrap_or_else(|_| "off".to_string());
    let auto_post_findings = has_findings && matches!(mode.as_str(), "comment" | "request_changes");
    if has_findings && db.review_requires_finding_selection(run.id).unwrap_or(true) && !auto_post_findings {
        log_auto_post(inputs, &format!("left for a decision: auto_post_mode is {mode}"));
        let _ = db.mark_review_ready_for_run(inputs.pr_id, run.id);
        let _ = db.set_pr_lane(
            inputs.pr_id,
            "ready",
            Some("Confirm finding selection before publishing"),
        );
        let _ = reserve_effective_review_result(
            db,
            &run,
            &inputs.full_name,
            inputs.number,
            "findings",
            "awaiting_action",
            &run.head_sha,
            prior.as_ref(),
            None,
            None,
            false,
            None,
        );
        return;
    }

    if no_new_changes {
        log_auto_post(inputs, "not attempted: no new changes since the previous result");
        let legacy = prior
            .is_none()
            .then(|| {
                db.latest_legacy_review_outcome(inputs.pr_id, run.id)
                    .ok()
                    .flatten()
                    .and_then(|outcome| legacy_effective_result(&outcome))
            })
            .flatten();
        let inherited_state = prior
            .as_ref()
            .map(|result| result.effective_state.as_str())
            .or_else(|| legacy.as_ref().map(|result| result.effective_state));
        let inherited_head = prior
            .as_ref()
            .map(|result| result.reviewed_head_sha.as_str())
            .or_else(|| legacy.as_ref().map(|result| result.reviewed_head_sha.as_str()))
            .unwrap_or(run.head_sha.as_str());
        let legacy_actionable_run_id = legacy.as_ref().and_then(|result| result.actionable_run_id);
        let (effective_state, reviewed_head_sha, publish_current_result) = match inherited_state {
            Some("clean") => ("clean", inherited_head, true),
            Some("awaiting_action") => ("awaiting_action", inherited_head, false),
            Some("findings_posted") => ("findings_posted", inherited_head, true),
            Some("findings_local") => ("findings_local", inherited_head, false),
            Some("skipped") => ("skipped", inherited_head, false),
            _ => ("skipped", run.head_sha.as_str(), false),
        };
        let result = reserve_effective_review_result(
            db,
            &run,
            &inputs.full_name,
            inputs.number,
            "no_new_changes",
            effective_state,
            reviewed_head_sha,
            prior.as_ref(),
            prior.as_ref().and_then(|result| result.github_review_id),
            prior.as_ref().and_then(|result| result.github_review_url.as_deref()),
            publish_current_result,
            legacy_actionable_run_id,
        );
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                mark_no_change_recovery_needed(db, inputs, &error);
                return;
            }
        };
        if effective_state == "awaiting_action" {
            let Some(actionable_run_id) = result.actionable_run_id else {
                mark_no_change_recovery_needed(db, inputs, "prior findings review is unavailable");
                return;
            };
            if let Err(error) = db.preserve_awaiting_action_after_no_changes(run.id, inputs.pr_id, actionable_run_id) {
                mark_no_change_recovery_needed(db, inputs, &error);
            }
        } else if result.publication_status == "pending" {
            let _ = publish_effective_review_result(db, &result, &inputs.full_name, inputs.number, &inputs.repo_path);
        } else if let Err(error) = finalize_inherited_result_without_comment(db, &result) {
            mark_no_change_recovery_needed(db, inputs, &error);
        }
        return;
    }

    if !has_findings {
        log_auto_post(inputs, "clean result: publishing the no-issues status");
        let result = reserve_effective_review_result(
            db,
            &run,
            &inputs.full_name,
            inputs.number,
            "clean",
            "clean",
            &run.head_sha,
            prior.as_ref(),
            None,
            None,
            true,
            None,
        );
        if let Ok(result) = result {
            if result.publication_status == "pending" {
                let _ =
                    publish_effective_review_result(db, &result, &inputs.full_name, inputs.number, &inputs.repo_path);
            } else {
                let _ = finish_published_result_lane(db, &result);
            }
        }
        return;
    }

    if matches!(mode.as_str(), "comment" | "request_changes") {
        let event = if mode == "comment" {
            "COMMENT"
        } else {
            "REQUEST_CHANGES"
        };
        match auto_post_review(
            db,
            &run,
            inputs.pr_id,
            &inputs.full_name,
            inputs.number,
            &inputs.repo_path,
            event,
        ) {
            Ok(AutoPostReview::Posted(review_id)) => {
                log_auto_post(inputs, "posted");
                let review_url = review_id.map(|id| {
                    format!(
                        "https://{}/{}/-/merge_requests/{}#note_{id}",
                        gitlab::gitlab_host(),
                        inputs.full_name,
                        inputs.number
                    )
                });
                if let Ok(result) = reserve_effective_review_result(
                    db,
                    &run,
                    &inputs.full_name,
                    inputs.number,
                    "findings",
                    "findings_posted",
                    &run.head_sha,
                    prior.as_ref(),
                    review_id,
                    review_url.as_deref(),
                    true,
                    None,
                ) {
                    if result.publication_status == "pending" {
                        let _ = publish_effective_review_result(
                            db,
                            &result,
                            &inputs.full_name,
                            inputs.number,
                            &inputs.repo_path,
                        );
                    }
                }
            }
            Ok(AutoPostReview::Superseded) => log_auto_post(inputs, "superseded by a new PR head"),
            Err(error) => {
                let superseded = gitlab::live_pr_state(&inputs.full_name, inputs.number)
                    .is_ok_and(|live| live.state != "OPEN" || live.head_sha != run.head_sha);
                if superseded {
                    log_auto_post(
                        inputs,
                        &format!("superseded by a new PR head after auto-post failed: {error}"),
                    );
                    let _ = db.mark_review_superseded_after_post(
                        run.id,
                        inputs.pr_id,
                        "Automatic review publication superseded by a new PR head",
                    );
                    return;
                }
                let reason = format!("Auto-post failed: {error}");
                log_auto_post(inputs, &reason);
                let _ = db.set_pr_lane(inputs.pr_id, "ready", Some(&reason));
                let _ = reserve_effective_review_result(
                    db,
                    &run,
                    &inputs.full_name,
                    inputs.number,
                    "findings",
                    "awaiting_action",
                    &run.head_sha,
                    prior.as_ref(),
                    None,
                    None,
                    false,
                    None,
                );
                let _ = db.note_auto_post_failure(run.id, &reason);
            }
        }
    } else {
        log_auto_post(inputs, &format!("left for a decision: auto_post_mode is {mode}"));
        let _ = reserve_effective_review_result(
            db,
            &run,
            &inputs.full_name,
            inputs.number,
            "findings",
            "awaiting_action",
            &run.head_sha,
            prior.as_ref(),
            None,
            None,
            false,
            None,
        );
    }
}

pub(crate) fn run_review_job(
    db_handle: Db,
    jobs: Arc<Mutex<HashMap<i64, ReviewJob>>>,
    inputs: review::ReviewInputs,
    cancel: Arc<AtomicBool>,
    superseded: Arc<AtomicBool>,
) {
    let pr_id = inputs.pr_id;
    let run_id = inputs.run_id;
    let progress_cancel = cancel.clone();
    // A connection per update: holding one for the whole review would pin it for minutes.
    let mut on_progress = |phase: &str, tail: &str| {
        if let Ok(db) = db_handle.open() {
            if matches!(db.update_review_run_progress(run_id, phase, tail), Ok(false)) {
                progress_cancel.store(true, Ordering::Relaxed);
            }
        }
    };
    let result = review::execute(&inputs, &cancel, &mut on_progress);
    if let Ok(db) = db_handle.open() {
        let done_phase = completed_review_phase(&db, run_id);
        let mut coverage_finalized = false;
        match &result {
            Ok(_) if superseded.load(Ordering::Relaxed) => {
                let _ = db.supersede_review_run_without_changing_pr(run_id, "Review superseded by a changed PR target");
            }
            Ok(output) => {
                let _ = db.set_review_run_risk(run_id, &output.risk);
                let u = &output.usage;
                let _ = db.set_review_run_usage(
                    run_id,
                    u.input_tokens,
                    u.output_tokens,
                    u.cache_read_tokens,
                    u.cache_creation_tokens,
                    u.model.as_deref(),
                );
                let model_rows: Vec<(String, i64, i64, i64, i64)> = u
                    .by_model
                    .iter()
                    .map(|m| {
                        (
                            m.model.clone(),
                            m.input_tokens,
                            m.output_tokens,
                            m.cache_read_tokens,
                            m.cache_creation_tokens,
                        )
                    })
                    .collect();
                let _ = db.set_run_model_usage(run_id, pr_id, &model_rows);
                let finished = if output.no_new_changes {
                    db.finish_review_run_no_new_changes(
                        run_id,
                        &output.body,
                        output.comments_json.as_deref(),
                        &done_phase,
                    )
                } else {
                    db.finish_review_run(run_id, &output.body, output.comments_json.as_deref(), &done_phase)
                }
                .unwrap_or(false);
                if finished {
                    let has_findings = review_has_findings(output.comments_json.as_deref(), Some(&output.body));
                    handle_completed_review(&db, &inputs, output.no_new_changes, has_findings);
                    coverage_finalized = true;
                } else {
                    log_auto_post(
                        &inputs,
                        "not attempted: the run was no longer running when its review finished",
                    );
                }
            }
            Err(_) if superseded.load(Ordering::Relaxed) => {
                let _ = db.supersede_review_run_without_changing_pr(run_id, "Review superseded by a changed PR target");
            }
            Err(error) if error.starts_with("review file limit exceeded:") => {
                let _ = db.skip_review_run_for_file_limit(run_id, error);
            }
            Err(error) if error.starts_with(review::NOTHING_REVIEWABLE) => {
                let _ = db.skip_review_run_without_reviewable_files(run_id, error);
            }
            Err(error) if is_review_target_changed_error(error) => {
                let _ = db.restore_review_intent(pr_id, inputs.force_full, inputs.reprocess);
                let _ = db.supersede_review_run_without_changing_pr(run_id, "Review superseded by a changed PR target");
                let _ = db.set_pr_lane_if_open(
                    pr_id,
                    "open",
                    Some("PR head or base changed during review; waiting for refresh"),
                );
            }
            Err(error) if error == "cancelled" => {
                let _ = db.cancel_review_run(run_id);
            }
            Err(error) => {
                let _ = db.fail_review_run(run_id, error);
            }
        }
        if coverage_finalized {
            let _ = db.finalize_review_coverage(run_id);
        } else {
            let _ = db.discard_review_coverage(run_id);
        }
    }
    if let Ok(mut jobs) = jobs.lock() {
        jobs.remove(&pr_id);
    }
}

/// Whether a review has anything to post: inline comments, a finding or question in its body, or
/// suggestions.
pub(crate) fn review_has_findings(comments_json: Option<&str>, review_body: Option<&str>) -> bool {
    comments_json_has_findings(comments_json)
        || review_body_has_findings(review_body)
        || !review_suggestions(review_body).is_empty()
}

/// The suggestions section of a review body, or "" when it has none.
pub(crate) fn review_suggestions(review_body: Option<&str>) -> &str {
    review_body.map_or("", |body| review::split_suggestions(body).1.trim())
}

pub(crate) fn comments_json_has_findings(comments_json: Option<&str>) -> bool {
    let Some(value) = comments_json.and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok()) else {
        return false;
    };
    let arr = value
        .get("comments")
        .and_then(|c| c.as_array())
        .or_else(|| value.as_array());
    arr.map(|a| !a.is_empty()).unwrap_or(false)
}

pub(crate) fn review_body_has_findings(review_body: Option<&str>) -> bool {
    review_body
        .map(|body| {
            body.lines().any(|line| {
                let normalized = line
                    .trim_start()
                    .trim_start_matches(['*', '_', '>', '-', '#'])
                    .trim_start()
                    .trim_start_matches(['*', '_', '>', '-', '#'])
                    .trim_start();
                review::has_point_marker_prefix(normalized)
            })
        })
        .unwrap_or(false)
}

pub(crate) enum AutoPostReview {
    Posted(Option<i64>),
    Superseded,
}

pub(crate) fn auto_post_review(
    db: &Database,
    run: &crate::db::ReviewRun,
    pr_id: i64,
    full_name: &str,
    number: i64,
    repo_path: &Path,
    event: &str,
) -> Result<AutoPostReview, String> {
    let manual_selection = db.review_requires_finding_selection(run.id)?;
    validate_live_review_scope(db, run.id, full_name, number, &run.head_sha, repo_path)?;
    let selection_revision = if manual_selection {
        Some(
            db.auto_confirm_generated_finding_selection(pr_id, run.id)?
                .ok_or("No safe generated finding selection is available; human confirmation is required.")?,
        )
    } else {
        None
    };
    let run = db
        .review_run_by_id(run.id)?
        .ok_or("Review run disappeared during automatic confirmation.")?;
    let mode = match event {
        "COMMENT" => "comment",
        "REQUEST_CHANGES" => "request_changes",
        _ => return Err("Unsupported automatic review event.".into()),
    };
    let event =
        crate::review::findings::auto_post_event(mode, run.comments_json.as_deref(), run.review_body.as_deref())
            .ok_or("Automatic publication is disabled.")?;
    validate_live_review_scope(db, run.id, full_name, number, &run.head_sha, repo_path)?;
    let draft = compose_review_post(full_name, number, &run, event, manual_selection);
    let valid_json = serde_json::json!({ "comments": draft.comments }).to_string();
    let evidence_warnings = db.review_evidence_warnings(run.id)?;
    let should_post = validate_review_post_draft(&run, event, &draft)? || !evidence_warnings.is_empty();
    let post_marker = if should_post {
        Some(review_post_marker(
            full_name,
            number,
            run.id,
            &run.head_sha,
            &db.installation_id()?,
        ))
    } else {
        None
    };
    let payload = if let Some(marker) = post_marker.as_deref() {
        Some(build_review_payload(
            full_name,
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
    let posting_decision = match event {
        "APPROVE" => "approve_post",
        "COMMENT" => "comment",
        _ => "request_changes",
    };
    if !db.try_begin_review_post(
        run.id,
        pr_id,
        &run.head_sha,
        event,
        posting_decision,
        "auto",
        selection_revision,
    )? {
        return Err("Review, finding selection, or automatic publishing settings changed, or publication is already in progress. Reload before retrying.".to_string());
    }
    db.set_review_run_comments_json(run.id, &draft.annotated_comments_json)?;
    let duplicate_only = !should_post && draft.suppressed_count > 0;
    let mut posted_review_id = None;
    if let Some(payload) = payload {
        if let Err(error) = validate_live_review_scope(db, run.id, full_name, number, &run.head_sha, repo_path) {
            let _ = db.reset_review_posting(run.id);
            return Err(error);
        }
        posted_review_id = publish_review_with_recovery(
            || {
                let snapshot = gitlab::snapshot_review_requests(full_name, number)?;
                db.prepare_review_request_restoration(run.id, &snapshot, post_marker.as_deref().unwrap_or_default())?;
                let prepared = gitlab::prepare_review(&payload)?;
                validate_live_review_scope(db, run.id, full_name, number, &run.head_sha, repo_path)?;
                Ok(prepared)
            },
            |prepared| gitlab::post_review(full_name, number, prepared),
            || gitlab::find_review_id_by_marker(full_name, number, post_marker.as_deref().unwrap_or_default()),
            || db.reset_review_posting(run.id),
        )?;
        if let Some(review_id) = posted_review_id {
            let _ = db.accept_review_request_restoration(run.id, review_id);
            let _ = db.retire_queue_acknowledgement(pr_id, &run.head_sha, Some(run.id));
            spawn_queue_acknowledgements(db.handle());
            if review_has_findings(run.comments_json.as_deref(), run.review_body.as_deref()) {
                retire_clean_comments_after_findings(db, full_name, number, run.id);
            }
        }
        if matches!(event, "APPROVE" | "REQUEST_CHANGES") {
            if let Some(review_id) = posted_review_id {
                let _ = db.set_ocr_review(pr_id, review_id, event);
            }
        }
    }
    // Nothing new reached GitLab when every finding was already posted; the review still finished,
    // so its "reviewing" comment has to go too.
    if posted_review_id.is_none() {
        let _ = db.retire_queue_acknowledgement(pr_id, &run.head_sha, Some(run.id));
        spawn_queue_acknowledgements(db.handle());
    }
    confirm_posted_review_live_head(
        db,
        run.id,
        pr_id,
        full_name,
        number,
        &run.head_sha,
        repo_path,
        "Review posted for a head superseded during publication",
    )?;
    let (decision, lane, reason) = match event {
        "APPROVE" => ("approve_post", "approved", "Approved"),
        "COMMENT" if duplicate_only => (
            "comment",
            "auto_posted",
            "Findings already posted by another OpenCodeReview run",
        ),
        "COMMENT" => ("comment", "auto_posted", "Commented"),
        _ if duplicate_only => (
            "request_changes",
            "auto_posted",
            "Findings already posted by another OpenCodeReview run",
        ),
        _ => ("request_changes", "auto_posted", "Requested changes"),
    };
    if !db.finalize_review_decision_or_supersede_if_stale(
        run.id,
        pr_id,
        decision,
        "posted",
        lane,
        reason,
        Some(event),
        &run.head_sha,
        Some("auto"),
        "Automatic review superseded by a new PR head",
    )? {
        return Ok(AutoPostReview::Superseded);
    }
    Ok(AutoPostReview::Posted(posted_review_id))
}

/// Bold `[Achado]`/`[Pergunta]` (or legacy English) title of an inline comment, ready for a table cell.
fn summary_row_title(body: &str) -> (bool, String) {
    let mut fence = false;
    let mut found = None;
    for line in body.lines().map(str::trim) {
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        let title = line.trim_start_matches(['*', '_', '>', '-', '#', ' ']);
        if let Some((kind, rest)) = review::strip_point_marker(title) {
            found = Some((kind == "question", rest));
            break;
        }
    }
    let (question, title) = found.unwrap_or_else(|| {
        (
            false,
            body.lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !review::is_review_attribution_line(line))
                .unwrap_or("Comentário de revisão"),
        )
    });
    let title = title.trim().trim_end_matches(['*', '_']).trim();
    let title = if title.is_empty() {
        "Comentário de revisão"
    } else {
        title
    };
    (question, table_cell(title, 120))
}

fn table_cell(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = if flat.chars().count() > max_chars {
        format!("{}…", flat.chars().take(max_chars - 1).collect::<String>().trim_end())
    } else {
        flat
    };
    flat.replace('|', "\\|")
}

fn plural(count: usize, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

/// Visible review body when findings go out as inline comments: a heading with
/// the counts and one table row per comment, so the PR page reads at a glance.
pub(crate) fn posted_findings_review_body(full_name: &str, head_sha: &str, comments: &[serde_json::Value]) -> String {
    let reviewed = format!(
        "Revisei [`{}`](https://{}/{full_name}/-/commit/{head_sha}).",
        short_sha(head_sha),
        gitlab::gitlab_host(),
    );
    if comments.is_empty() {
        return format!("### ⚠️ Publiquei achados\n\n{reviewed} Os detalhes estão nesta revisão.");
    }
    let mut rows = comments
        .iter()
        .map(|comment| {
            let (question, title) = summary_row_title(comment["body"].as_str().unwrap_or_default());
            let place = match (
                comment["path"].as_str().filter(|path| !path.is_empty()),
                comment["line"].as_i64(),
            ) {
                (Some(path), Some(line)) => format!("`{}:{line}`", table_cell(path, 200)),
                (Some(path), None) => format!("`{}`", table_cell(path, 200)),
                (None, _) => "Geral".to_string(),
            };
            // Most severe first, questions last; a comment without a severity reads as medium.
            let (rank, icon) = match (
                question,
                crate::review::evidence::comment_severity(comment["body"].as_str().unwrap_or_default()),
            ) {
                (true, _) => (3, "❓"),
                (false, Some("high")) => (0, "🔴"),
                (false, Some("low")) => (2, "🟡"),
                (false, _) => (1, "🟠"),
            };
            (rank, question, format!("| {icon} | {title} | {place} |"))
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|(rank, _, _)| *rank);
    let rows = rows
        .into_iter()
        .map(|(_, question, row)| (question, row))
        .collect::<Vec<_>>();
    let questions = rows.iter().filter(|(question, _)| *question).count();
    let issues = rows.len() - questions;
    let (heading, intro) = if issues == 0 {
        (
            format!("### ❓ Tenho {}", plural(questions, "pergunta")),
            "Cada pergunta abaixo explica o que confirmar.",
        )
    } else if questions == 0 {
        (
            format!("### ⚠️ Encontrei {}", plural(issues, "problema")),
            "Cada problema abaixo tem os detalhes e uma correção sugerida.",
        )
    } else {
        (
            format!(
                "### ⚠️ Encontrei {} e {}",
                plural(issues, "problema"),
                plural(questions, "pergunta")
            ),
            "Cada problema abaixo tem os detalhes e uma correção sugerida.",
        )
    };
    let table = rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>().join("\n");
    format!("{heading}\n\n{reviewed} {intro}\n\n| | Problema | Onde |\n|---|---|---|\n{table}")
}

pub(crate) fn build_review_payload(
    full_name: &str,
    head_sha: &str,
    event: &str,
    body: &str,
    comments_json: Option<&str>,
    marker: Option<&str>,
    evidence_warnings: &[crate::review::warnings::EvidenceWarning],
) -> Result<String, String> {
    let comments = comments_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("comments").cloned())
        .unwrap_or_else(|| serde_json::json!([]));
    let inline = comments.as_array().map(Vec::as_slice).unwrap_or_default();
    let body = if body.trim().is_empty() && !evidence_warnings.is_empty() && inline.is_empty() {
        format!(
            "### ⚠️ Terminei com avisos\n\nRevisei [`{}`](https://{}/{full_name}/-/commit/{head_sha}).",
            short_sha(head_sha),
            gitlab::gitlab_host()
        )
    } else if body.trim().is_empty() && matches!(event, "COMMENT" | "REQUEST_CHANGES") {
        posted_findings_review_body(full_name, head_sha, inline)
    } else {
        body.to_string()
    };
    let body = crate::review::warnings::append(&body, evidence_warnings);
    // Footer is the only brand mark on a posted review. Append it for both generated
    // empty drafts and precomposed bodies from `finish_review_post`.
    let body = if body.trim().is_empty() || body.contains("Revisado com [OpenCodeReview]") {
        body
    } else {
        format!("{body}\n\n{}", ocr_footer())
    };
    let body = marker.map(|marker| format!("{body}\n\n{marker}")).unwrap_or(body);
    serde_json::to_string(
        &serde_json::json!({ "commit_id": head_sha, "event": event, "body": body, "comments": comments }),
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn split_comments_for_post(
    full_name: &str,
    number: i64,
    comments_json: Option<&str>,
) -> (serde_json::Value, String) {
    let comments = comments_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("comments").cloned())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    if comments.is_empty() {
        return (serde_json::json!([]), String::new());
    }
    let anchors = gitlab::diff_anchors(full_name, number);
    if anchors.is_empty() {
        return (serde_json::Value::Array(comments), String::new());
    }
    let (mut valid, mut relocated) = (Vec::new(), String::new());
    for comment in comments {
        let path = comment.get("path").and_then(|value| value.as_str()).unwrap_or_default();
        let line = comment.get("line").and_then(|value| value.as_i64()).unwrap_or(-1);
        let side = comment.get("side").and_then(|value| value.as_str()).unwrap_or("RIGHT");
        let anchored = anchors.get(path).is_some_and(|(right, left)| {
            if side.eq_ignore_ascii_case("LEFT") {
                left.contains(&line)
            } else {
                right.contains(&line)
            }
        });
        if anchored {
            valid.push(comment);
        } else {
            let body = comment.get("body").and_then(|value| value.as_str()).unwrap_or_default();
            relocated.push_str(&format!(
                "\n\n---\n\n**`{path}:{line}`** (couldn't anchor to this PR's diff):\n\n{body}"
            ));
        }
    }
    (serde_json::Value::Array(valid), relocated)
}

pub(crate) fn generated_review_body(body: &str) -> String {
    // Brand is only in `ocr_footer` on the review; do not prepend it to comment bodies.
    body.trim().to_string()
}

pub(crate) fn review_post_body(relocated: &str) -> String {
    if relocated.trim().is_empty() {
        String::new()
    } else {
        generated_review_body(&format!(
            "Comment(s) that couldn't be anchored to the diff are inlined below:{relocated}"
        ))
    }
}

pub(crate) struct ReviewPostDraft {
    pub(crate) body: String,
    pub(crate) comments: serde_json::Value,
    pub(crate) annotated_comments_json: String,
    pub(crate) suppressed_count: usize,
}

pub(crate) fn compose_review_post(
    full_name: &str,
    number: i64,
    run: &crate::db::ReviewRun,
    event: &str,
    include_selected_body: bool,
) -> ReviewPostDraft {
    let (valid_comments, relocated) = split_comments_for_post(full_name, number, run.comments_json.as_deref());
    let dedupe = gitlab::dedupe_review_comments(full_name, number, valid_comments);
    finish_review_post(run, event, include_selected_body, dedupe, &relocated)
}

pub(crate) fn validate_review_post_draft(
    run: &crate::db::ReviewRun,
    event: &str,
    draft: &ReviewPostDraft,
) -> Result<bool, String> {
    let should_post = event == "APPROVE"
        || !draft.body.trim().is_empty()
        || draft.comments.as_array().is_some_and(|items| !items.is_empty());
    let selected = run
        .comments_json
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value["comments"].as_array().map(Vec::len))
        .unwrap_or(0);
    if !should_post
        && (selected > 0 || review_body_has_findings(run.review_body.as_deref()))
        && !(selected > 0
            && draft.suppressed_count == selected
            && !review_body_has_findings(run.review_body.as_deref()))
    {
        return Err("Selected review comments disappeared while preparing publication. Review remains unpublished; retry after checking the finding selection.".into());
    }
    Ok(should_post)
}

pub(crate) fn finish_review_post(
    run: &crate::db::ReviewRun,
    event: &str,
    include_selected_body: bool,
    dedupe: gitlab::DedupeResult,
    relocated: &str,
) -> ReviewPostDraft {
    let has_inline = dedupe
        .post_comments
        .as_array()
        .is_some_and(|comments| !comments.is_empty());
    let has_body_finding = review_body_has_findings(run.review_body.as_deref());
    let suggestions = review_suggestions(run.review_body.as_deref());
    // Suggestions repeat in every review of the pull request, so they never post on their own once
    // every inline comment was already posted.
    let only_duplicates =
        dedupe.suppressed_count > 0 && !has_inline && !has_body_finding && relocated.trim().is_empty();
    let body = if only_duplicates {
        String::new()
    } else if has_inline {
        if include_selected_body && has_body_finding {
            generated_review_body(&format!(
                "{}\n\n{}",
                run.review_body.as_deref().unwrap_or_default(),
                review_post_body(relocated)
            ))
        } else if suggestions.is_empty() {
            review_post_body(relocated)
        } else {
            generated_review_body(&format!("{suggestions}\n\n{}", review_post_body(relocated)))
        }
    } else if has_body_finding && matches!(event, "COMMENT" | "REQUEST_CHANGES") {
        let summary = run.review_body.clone().unwrap_or_default();
        let combined = if relocated.trim().is_empty() {
            summary
        } else {
            format!("{summary}\n{relocated}")
        };
        generated_review_body(&combined)
    } else if !suggestions.is_empty() {
        generated_review_body(&format!("{suggestions}\n\n{}", review_post_body(relocated)))
    } else {
        review_post_body(relocated)
    };
    ReviewPostDraft {
        body,
        comments: dedupe.post_comments,
        annotated_comments_json: dedupe.annotated_comments_json,
        suppressed_count: dedupe.suppressed_count,
    }
}

pub(crate) fn completed_review_phase(db: &Database, run_id: i64) -> String {
    let fallback = review_done_phase();
    let Some(raw) = db
        .review_run_by_id(run_id)
        .ok()
        .flatten()
        .and_then(|run| run.phase_json)
    else {
        return fallback;
    };
    complete_phase_json(&raw, &fallback)
}

pub(crate) fn complete_phase_json(raw: &str, fallback: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return fallback.to_string();
    };
    let Some(phases) = value.as_array_mut() else {
        return fallback.to_string();
    };
    if phases.is_empty() {
        return fallback.to_string();
    }
    for phase in phases {
        if phase["status"] == "skipped" {
            continue;
        }
        if let Some(object) = phase.as_object_mut() {
            object.insert("status".to_string(), serde_json::Value::String("ok".to_string()));
        }
    }
    serde_json::to_string(&value).unwrap_or_else(|_| fallback.to_string())
}

pub(crate) fn review_done_phase() -> String {
    "[{\"id\":\"context\",\"label\":\"Context ready\",\"status\":\"ok\"},{\"id\":\"review\",\"label\":\"Reviewed\",\"status\":\"ok\"},{\"id\":\"ready\",\"label\":\"Ready\",\"status\":\"ok\"}]".to_string()
}

pub(crate) fn active_review_count(state: &SharedState) -> usize {
    state.review_jobs.lock().map(|jobs| jobs.len()).unwrap_or(0)
}
