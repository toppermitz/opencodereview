use super::*;

#[path = "polling_pr.rs"]
mod pr_state;
pub(crate) use pr_state::*;

pub(crate) fn start_poll_scheduler(state: SharedState) {
    thread::spawn(move || loop {
        let sleep_secs = with_db_read_raw(&state, |db| db.poll_status())
            .ok()
            .and_then(|status| {
                status
                    .next_poll_at
                    .map(|next| ((next - crate::db::now_millis()).max(0) as u64 / 1000).max(1))
            })
            .unwrap_or(state.config.poll_interval_seconds.max(1));
        thread::sleep(Duration::from_secs(sleep_secs));
        let due = with_db_read_raw(&state, |db| db.poll_status())
            .ok()
            .and_then(|status| status.next_poll_at)
            .is_none_or(|next| next <= crate::db::now_millis());
        if due {
            let _ = poll_reviews(&state);
        }
    });
}

pub(crate) fn list_index_jobs(state: &SharedState) -> Result<serde_json::Value, String> {
    // Every instance's index runs, not only this one's.
    let active = with_db_read_raw(state, |db| db.active_index_count())?;
    let jobs = with_db_read_raw(state, |db| db.list_index_queue())?
        .into_iter()
        .map(|(repo, status)| {
            serde_json::json!({
              "repo": repo,
              "status": status,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "active": active, "jobs": jobs }))
}

pub(crate) fn list_review_board(state: &SharedState) -> Result<serde_json::Value, String> {
    let mut groups = with_db_read_raw(state, |db| db.list_review_board())?;
    for group in &mut groups {
        group
            .items
            .retain(|item| !matches!(item.lane.as_str(), "auto_skipped" | "draft"));
    }
    serde_json::to_value(groups).map_err(|error| error.to_string())
}

pub(crate) fn skill_version_content_http(state: &SharedState, id: i64) -> Result<serde_json::Value, String> {
    with_db_read(state, |db| {
        Ok(serde_json::json!({ "content": db.skill_version_content(id) }))
    })
}

pub(crate) fn create_skill_version_http(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    require_auth(request, state)?;
    let payload: CreateSkillVersionRequest =
        serde_json::from_slice(&request.body).map_err(|error| error.to_string())?;
    if payload.content.trim().is_empty() {
        return Err("Skill content is empty.".to_string());
    }
    with_db(state, |db| {
        db.create_skill_version(&payload.content, payload.note.as_deref())
    })
}

pub(crate) fn poll_reviews(state: &SharedState) -> Result<serde_json::Value, String> {
    if state.poll_running.swap(true, Ordering::Relaxed) {
        return with_db_read(state, |db| db.poll_status());
    }
    // Another instance may be polling; one poll at a time across them all.
    let polled = state.config.db.run_exclusive(exclusive::POLL, || {
        let _ = with_db_raw(state, |db| db.record_poll_start());
        run_poll_reviews(state).inspect_err(|error| {
            let _ = with_db_raw(state, |db| db.record_poll_result(crate::db::now_millis(), Some(error)));
        })
    });
    state.poll_running.store(false, Ordering::Relaxed);
    let result = match polled? {
        Some(result) => result,
        None => return with_db_read(state, |db| db.poll_status()),
    };
    spawn_queue_acknowledgements(state.config.db.clone());
    result
}

pub(crate) fn manual_review_pause_applies(
    old_head: &str,
    old_lane: &str,
    old_status_reason: Option<&str>,
    incoming_head: &str,
    computed_lane: &str,
) -> bool {
    old_head == incoming_head
        && old_lane == "manual_required"
        && old_status_reason == Some("Review cancelled")
        && matches!(computed_lane, "open" | "manual_required")
}

fn sync_monitor_all_workspaces(state: &SharedState) {
    let Ok(logins) = with_db_read_raw(state, |db| db.monitor_all_logins()) else {
        return;
    };
    for login in logins {
        if let Ok(repos) = gitlab::list_repos(&login) {
            let _ = with_db_raw(state, |db| db.upsert_repos(&repos));
        }
    }
    let _ = with_db_raw(state, |db| db.enable_monitor_all_repos());
}

pub(crate) fn run_poll_reviews(state: &SharedState) -> Result<serde_json::Value, String> {
    spawn_failed_clean_result_retry(state.config.db.clone(), state.config.repos_dir.clone());
    sync_monitor_all_workspaces(state);
    let (repos, settings) = with_db_raw(state, |db| Ok((db.monitored_repos()?, db.settings()?)))?;
    let mut target_logins = std::collections::HashSet::new();
    if let Some(login) = settings.review_assigned_only.then(gitlab::current_login).flatten() {
        target_logins.insert(login.to_ascii_lowercase());
    }
    target_logins.extend(
        settings
            .review_target_logins
            .iter()
            .map(|login| login.trim_start_matches('@').to_ascii_lowercase()),
    );
    let mut current_teams = std::collections::HashSet::new();
    current_teams.extend(
        settings
            .review_target_teams
            .iter()
            .flat_map(|team| normalize_team_targets(team)),
    );
    let mut errors = Vec::new();
    let mut eligible_queue = std::collections::HashSet::new();
    for repo in repos {
        let settings = with_db_read_raw(state, |db| db.settings()).unwrap_or_else(|_| settings.clone());
        let partial_listing = settings.review_assigned_only && current_teams.is_empty() && !target_logins.is_empty();
        let listed = if partial_listing {
            list_open_prs_for_targets(&repo.full_name, &target_logins)
        } else {
            gitlab::list_open_prs(&repo.full_name)
        };
        match listed {
            Ok(mut prs) => {
                let tracked = with_db_read_raw(state, |db| db.activated_open_pr_numbers(repo.id))?;
                for number in tracked {
                    if prs.iter().any(|pr| pr.number == number) {
                        continue;
                    }
                    match gitlab::activation_pr(&repo.full_name, number) {
                        Ok(Some(pr)) => prs.push(pr),
                        Ok(None) => {}
                        Err(error) => errors.push(format!("{} #{number}: {error}", repo.full_name)),
                    }
                }
                let open_numbers: Vec<i64> = prs.iter().map(|pr| pr.number).collect();
                let mut activation_candidates = HashMap::new();
                for pr in prs {
                    let polled = poll_one_pr(state, &repo, &settings, &target_logins, &current_teams, &pr, true)?;
                    if polled.activation_eligible {
                        activation_candidates.insert(pr.number, (polled.pr_id, polled.head));
                    }
                    if polled.queue_eligible {
                        eligible_queue.insert(polled.pr_id);
                    }
                }
                if let Err(error) = reconcile_pr_lifecycle(state, &repo, &open_numbers) {
                    errors.push(format!("{} lifecycle: {error}", repo.full_name));
                }
                let mut resolved_candidates = HashMap::new();
                if let Err(error) = poll_author_commands(state, &repo, &activation_candidates, |number| {
                    let Some(pr) = gitlab::activation_pr(&repo.full_name, number)? else {
                        return Ok(None);
                    };
                    if pr.changed_files.is_none() {
                        return Err(format!("Missing file count for PR #{number}"));
                    }
                    let polled = poll_one_pr(state, &repo, &settings, &target_logins, &current_teams, &pr, false)?;
                    if !polled.activation_eligible {
                        return Ok(None);
                    }
                    let candidate = (polled.pr_id, polled.head);
                    resolved_candidates.insert(number, candidate.clone());
                    Ok(Some(candidate))
                }) {
                    errors.push(format!("{} author commands: {error}", repo.full_name));
                }
                activation_candidates.extend(resolved_candidates);
                for (number, (pr_id, _)) in &activation_candidates {
                    if with_db_read_raw(state, |db| db.pr_comment_activated(repo.id, *number))? {
                        eligible_queue.insert(*pr_id);
                    }
                }
            }
            Err(error) => errors.push(format!("{}: {error}", repo.full_name)),
        }
    }
    let queued = with_db_raw(state, |db| db.queued_pr_ids()).unwrap_or_default();
    for pr_id in queued {
        if !eligible_queue.contains(&pr_id)
            && with_db_read_raw(state, |db| db.comment_queue_requires_eligibility(pr_id))?
        {
            continue;
        }
        if active_review_count(state) >= settings.review_concurrency as usize {
            break;
        }
        let _ = start_review_job(state, pr_id, false);
    }
    let status = with_db_raw(state, |db| {
        db.record_poll_result(
            crate::db::now_millis(),
            (!errors.is_empty()).then(|| errors.join("; ")).as_deref(),
        )
    })?;
    serde_json::to_value(status).map_err(|error| error.to_string())
}

fn reconcile_pr_lifecycle(state: &SharedState, repo: &Repo, open_numbers: &[i64]) -> Result<(), String> {
    let open_numbers = open_numbers.iter().copied().collect::<std::collections::HashSet<_>>();
    let mut cursor = None;
    loop {
        let candidates = with_db_raw(state, |db| db.lifecycle_candidates(repo.id, cursor, 200))?;
        let next_cursor = candidates.last().map(|candidate| (candidate.4, candidate.0));
        let has_more = candidates.len() == 200;
        for (pr_id, number, local_state, lane, _) in candidates {
            if open_numbers.contains(&number) && local_state == "open" && !matches!(lane.as_str(), "closed" | "merged")
            {
                continue;
            }
            let live = match gitlab::live_pr_state(&repo.full_name, number) {
                Ok(live) => live,
                Err(_) => continue,
            };
            if live.state.eq_ignore_ascii_case("OPEN") {
                continue;
            }
            if let Ok(mut jobs) = state.review_jobs.lock() {
                if let Some(job) = jobs.get_mut(&pr_id) {
                    job.superseded.store(true, Ordering::Relaxed);
                    for cancel in job.cancel_by_run.values() {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            }
            let reason = if live.state.eq_ignore_ascii_case("MERGED") {
                "MR merged on GitLab"
            } else {
                "MR closed on GitLab"
            };
            with_db_raw(state, |db| {
                db.mark_pr_terminal_with_merged_at(pr_id, &live.state, reason, live.merged_at)
            })?;
            if live.merged_at.is_some() {
                // Merged code is purged once the merge is confirmed; free it now, not at the next hourly pass.
                super::wake_retention_worker(state);
            }
        }
        if !has_more {
            break;
        }
        cursor = next_cursor;
    }
    Ok(())
}

pub(crate) fn pr_assigned_to(
    pr: &gitlab::GhPullRequest,
    logins: &std::collections::HashSet<String>,
    teams: &std::collections::HashSet<String>,
) -> bool {
    let login_match = |value: &str| logins.contains(&value.trim_start_matches('@').to_ascii_lowercase());
    let team_match = |value: &str| {
        let value = value.trim_start_matches('@').to_ascii_lowercase();
        teams.contains(&value) || teams.iter().any(|team| team.ends_with(&format!("/{value}")))
    };
    pr.assignees
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .any(|item| item.login.as_deref().is_some_and(login_match))
        || pr.review_requests.as_deref().unwrap_or(&[]).iter().any(|item| {
            item.login.as_deref().is_some_and(login_match)
                || item.slug.as_deref().is_some_and(team_match)
                || item.name.as_deref().is_some_and(team_match)
        })
}

pub(crate) fn list_open_prs_for_targets(
    full_name: &str,
    logins: &std::collections::HashSet<String>,
) -> Result<Vec<gitlab::GhPullRequest>, String> {
    let mut prs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for login in logins {
        for pr in gitlab::list_open_prs_for_login(full_name, login)? {
            if seen.insert(pr.number) {
                prs.push(pr);
            }
        }
    }
    Ok(prs)
}

pub(crate) fn normalize_team_targets(team: &str) -> Vec<String> {
    let value = team.trim().trim_start_matches('@').to_ascii_lowercase();
    if value.is_empty() {
        return Vec::new();
    }
    let short = value.rsplit('/').next().unwrap_or(&value).to_string();
    if short == value {
        vec![value]
    } else {
        vec![value, short]
    }
}

pub(crate) fn poll_scope_needs_reset(
    old_head: &str,
    head: &str,
    old_lane: &str,
    lane: &str,
    base_changed: bool,
    activated: bool,
) -> bool {
    old_head != head
        || base_changed
        || old_lane == "size_skipped"
        || lane == "size_skipped"
        || (activated && matches!(lane, "auto_skipped" | "draft"))
        || matches!(old_lane, "closed" | "merged")
}

pub(crate) fn reset_polled_review(
    db: &Database,
    pr_id: i64,
    base_changed: bool,
    lane: &str,
    reason: Option<&str>,
) -> Result<(), String> {
    if base_changed {
        db.invalidate_review_scope(pr_id, None, "PR base/scope changed; verifying reusable review coverage")?;
    } else {
        db.supersede_review_runs(pr_id)?;
    }
    db.set_pr_lane(pr_id, lane, reason)
}
