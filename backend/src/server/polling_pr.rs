use super::*;

pub(crate) struct PolledPr {
    pub pr_id: i64,
    pub head: String,
    pub activation_eligible: bool,
    pub queue_eligible: bool,
}

#[cfg(not(test))]
fn list_pr_files(full_name: &str, number: i64) -> Result<Vec<String>, String> {
    gitlab::pr_file_paths(full_name, number)
}

#[cfg(test)]
fn list_pr_files(_: &str, _: i64) -> Result<Vec<String>, String> {
    Err("tests never list pull request files from GitHub".into())
}

/// How long a failed file listing is trusted before the next poll lists again.
pub(crate) const FILE_LISTING_RETRY: std::time::Duration = std::time::Duration::from_secs(600);

/// A pull request's file count for the review limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileCount {
    pub(crate) count: i64,
    /// Whether `count` holds only the files a review reads, or GitHub's full count.
    pub(crate) reviewable: bool,
}

/// What a file listing gave for one head and base.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FileListing {
    Reviewable(i64),
    /// GitHub stops listing at 3,000 files, so listing again cannot help.
    Incomplete,
    Failed(std::time::Instant),
}

/// The file count the review limit applies to. Above the limit, only the files a review reads
/// count (`counts_toward_file_limit`), from GitHub's file listing, remembered per head and base.
/// A listing that stops short (over 3,000 files) keeps GitHub's full count for that revision, and
/// a failed one keeps it until the listing is retried.
pub(crate) fn file_count_for_limit(
    state: &SharedState,
    repo: &Repo,
    pr: &gitlab::GhPullRequest,
    limit: i64,
    list_files: impl FnOnce(&str, i64) -> Result<Vec<String>, String>,
) -> Option<FileCount> {
    let count = pr.changed_files?;
    let full = FileCount {
        count,
        reviewable: false,
    };
    if count <= limit {
        return Some(full);
    }
    let key = (repo.id, pr.number);
    let revision = (pr.head_ref_oid.clone(), pr.base_ref_oid.clone());
    let cached = state.reviewable_file_counts.lock().ok().and_then(|counts| {
        counts
            .get(&key)
            .filter(|(counted_at, _)| *counted_at == revision)
            .map(|(_, listing)| *listing)
    });
    let listing = match cached {
        Some(FileListing::Failed(at)) if at.elapsed() >= FILE_LISTING_RETRY => None,
        cached => cached,
    }
    .unwrap_or_else(|| {
        let listing = match list_files(&repo.full_name, pr.number) {
            Ok(paths) if (paths.len() as i64) < count => FileListing::Incomplete,
            Ok(paths) => FileListing::Reviewable(
                paths
                    .iter()
                    .filter(|path| crate::review::counts_toward_file_limit(path))
                    .count() as i64,
            ),
            Err(_) => FileListing::Failed(std::time::Instant::now()),
        };
        if let Ok(mut counts) = state.reviewable_file_counts.lock() {
            counts.insert(key, (revision, listing));
        }
        listing
    });
    Some(match listing {
        FileListing::Reviewable(count) => FileCount {
            count,
            reviewable: true,
        },
        FileListing::Incomplete | FileListing::Failed(_) => full,
    })
}

pub(crate) fn poll_one_pr(
    state: &SharedState,
    repo: &Repo,
    settings: &crate::db::Settings,
    target_logins: &std::collections::HashSet<String>,
    current_teams: &std::collections::HashSet<String>,
    pr: &gitlab::GhPullRequest,
    allow_auto_start: bool,
) -> Result<PolledPr, String> {
    let matched_keyword = settings
        .auto_skip_keywords
        .iter()
        .find(|keyword| pr.title.to_ascii_lowercase().contains(&keyword.to_ascii_lowercase()));
    let activated = with_db_read_raw(state, |db| db.pr_comment_activated(repo.id, pr.number))?;
    let target_mismatch =
        !activated && settings.review_assigned_only && !pr_assigned_to(pr, target_logins, current_teams);
    let off_main = repo.default_branch.as_deref().is_some_and(|default| {
        pr.base_ref_name
            .as_deref()
            .is_some_and(|base| !base.is_empty() && base != default)
    });
    let file_count = file_count_for_limit(state, repo, pr, settings.review_max_files, list_pr_files);
    let too_many_files = file_count.is_some_and(|files| files.count > settings.review_max_files);
    let computed_lane = author_activation_lane(
        pr.is_draft && repo.skip_draft_prs,
        activated,
        matched_keyword.is_some() || target_mismatch,
        too_many_files,
        settings.review_auto_enabled,
    );
    let status_reason = if too_many_files {
        Some(format!(
            "Ignorado: {} {} excedem o limite de {} arquivos. Altere Máximo de arquivos do MR em Configurações.",
            file_count.map(|files| files.count).unwrap_or_default(),
            if file_count.is_some_and(|files| files.reviewable) {
                "arquivos alterados revisáveis"
            } else {
                "arquivos alterados"
            },
            settings.review_max_files
        ))
    } else if off_main {
        Some(format!(
            "MR empilhado: revisando `{}` → `{}` com contexto da base imediata",
            pr.head_ref_name.as_deref().unwrap_or("head"),
            pr.base_ref_name.as_deref().unwrap_or_default()
        ))
    } else if target_mismatch {
        Some("Só MRs atribuídos a você ou às suas equipes estão habilitados".to_string())
    } else {
        matched_keyword
            .map(|keyword| format!("Palavra-chave de auto-pulo correspondente: {keyword}"))
            .or_else(|| (!settings.review_auto_enabled).then(|| "Revisão por IA manual configurada".to_string()))
    };
    let previous_scope = with_db_raw(state, |db| db.pr_scope(repo.id, pr.number))?;
    let incoming_scope = pr
        .base_ref_name
        .as_ref()
        .zip(pr.base_ref_oid.as_ref())
        .map(|(base_ref, base_sha)| crate::review::scope::ReviewScope {
            head_sha: pr.head_ref_oid.clone(),
            base_ref: base_ref.clone(),
            base_sha: base_sha.clone(),
        });
    let base_scope_changed = previous_scope
        .as_ref()
        .zip(incoming_scope.as_ref())
        .is_some_and(|(before, after)| {
            (before.base_ref != after.base_ref || before.base_sha != after.base_sha)
                && !crate::review::scope::equivalent(
                    &state.config.repos_dir.join(&repo.owner).join(&repo.name),
                    before,
                    after,
                    &AtomicBool::new(false),
                )
        });
    let github_updated_at = pr.updated_at.as_deref().and_then(gitlab::iso8601_to_epoch_ms);
    let (pr_id, effective_lane, decided) = with_db_raw(state, |db| {
        let prior = db.pr_poll_state(repo.id, pr.number)?;
        let pr_id = db.upsert_pull_request_with_github_updated_at(
            &crate::db::PullRequestInput {
                repo_id: repo.id,
                number: pr.number,
                title: pr.title.clone(),
                author: pr.author.login.clone(),
                author_avatar_url: pr
                    .author
                    .avatar_url
                    .clone()
                    .or_else(|| Some(gitlab::avatar_url(&pr.author.login))),
                is_draft: pr.is_draft,
                head_sha: pr.head_ref_oid.clone(),
                base_sha: pr.base_ref_oid.clone(),
                base_ref: pr.base_ref_name.clone(),
                head_ref: pr.head_ref_name.clone(),
                default_branch: repo.default_branch.clone(),
                url: pr.url.clone(),
                lane: computed_lane.to_string(),
                status_reason: status_reason.clone(),
            },
            github_updated_at,
        )?;
        // Only a reviewable count is stored for the job-start check; GitHub's full count would
        // skip a pull request whose reviewable files fit a lowered limit.
        db.set_pr_changed_file_count(
            pr_id,
            file_count.filter(|files| files.reviewable).map(|files| files.count),
        )?;
        let has_active_review_job = state
            .review_jobs
            .lock()
            .map(|jobs| jobs.contains_key(&pr_id))
            .unwrap_or(false);
        if let Some((old_head, old_lane, _, old_status_reason)) = prior {
            let manually_paused = manual_review_pause_applies(
                &old_head,
                &old_lane,
                old_status_reason.as_deref(),
                &pr.head_ref_oid,
                computed_lane,
            );
            let managed = matches!(
                old_lane.as_str(),
                "reviewing"
                    | "queued"
                    | "ready"
                    | "failed"
                    | "posted"
                    | "auto_posted"
                    | "reviewed_cleanly"
                    | "approved"
                    | "skipped"
                    | "size_skipped"
                    | "dismissed"
                    | "closed"
            );
            if poll_scope_needs_reset(
                &old_head,
                &pr.head_ref_oid,
                &old_lane,
                computed_lane,
                base_scope_changed,
                activated,
            ) {
                if let Ok(jobs) = state.review_jobs.lock() {
                    if let Some(job) = jobs.get(&pr_id) {
                        supersede_active_review(db, pr_id, job)?;
                    }
                }
                reset_polled_review(db, pr_id, base_scope_changed, computed_lane, status_reason.as_deref())?;
            } else if (!managed
                || matches!(old_lane.as_str(), "queued" | "failed")
                    && matches!(computed_lane, "auto_skipped" | "size_skipped"))
                && old_lane != computed_lane
                && !manually_paused
            {
                db.set_pr_lane(pr_id, computed_lane, status_reason.as_deref())?;
            }
        }
        let (_, lane, last_decision_sha) = db
            .pr_state(repo.id, pr.number)?
            .unwrap_or_else(|| (pr.head_ref_oid.clone(), computed_lane.to_string(), None));
        let decided = last_decision_sha.as_deref() == Some(pr.head_ref_oid.as_str());
        if decided && !has_active_review_job && matches!(lane.as_str(), "open" | "queued" | "reviewing") {
            db.restore_decided_pr_lane(pr_id)?;
            let (_, restored_lane, _) = db
                .pr_state(repo.id, pr.number)?
                .unwrap_or_else(|| (pr.head_ref_oid.clone(), lane.clone(), last_decision_sha.clone()));
            return Ok((pr_id, restored_lane, decided));
        }
        Ok((pr_id, lane, decided))
    })?;
    let _ = with_db_raw(state, |db| {
        if effective_lane == "manual_required" && !decided {
            db.upsert_pending_action(
                "manual_review_needed",
                repo.id,
                pr_id,
                "Manual review needed",
                status_reason.as_deref().unwrap_or("Manual AI review required"),
            )
        } else if matches!(computed_lane, "auto_skipped" | "size_skipped") {
            db.resolve_pending_for_pr("manual_review_needed", pr_id)
        } else {
            Ok(())
        }
    });
    if allow_auto_start
        && settings.review_auto_enabled
        && effective_lane == "open"
        && !decided
        && !state
            .review_jobs
            .lock()
            .map(|jobs| jobs.contains_key(&pr_id))
            .unwrap_or(true)
    {
        let _ = start_review_job(state, pr_id, false);
    }
    Ok(PolledPr {
        pr_id,
        head: pr.head_ref_oid.clone(),
        // Only verified author commands consume this eligibility. Assignment
        // still filters ordinary polling until activation has been persisted.
        activation_eligible: matched_keyword.is_none() && !too_many_files && pr.changed_files.is_some(),
        queue_eligible: computed_lane != "draft"
            && !matches!(computed_lane, "auto_skipped" | "size_skipped")
            && pr.changed_files.is_some(),
    })
}
