use super::*;

impl Database {
    pub fn list_review_board(&self) -> Result<Vec<ReviewBoardGroup>, String> {
        let mut stmt = self.conn.prepare(
      "SELECT pr.id, r.id, r.full_name, pr.number, pr.title, pr.author, pr.author_avatar_url, pr.state,
              CASE
                WHEN pr.state = 'merged' THEN 'merged'
                WHEN pr.state = 'closed' THEN 'closed'
                WHEN pr.review_lane IN ('closed', 'merged', 'queued') THEN pr.review_lane
                WHEN res.effective_state = 'awaiting_action'
                  AND res.observed_head_sha = pr.head_sha
                  AND lr.head_sha = pr.head_sha
                  AND res.observation = 'findings'
                  AND json_member_is_true(rfa.audit_json, 'complete')
                  AND json_member_type(rfa.audit_json, 'candidates') = 'array'
                  AND json_member_array_length(rfa.audit_json, 'candidates') = 0 THEN 'reviewed_cleanly'
                WHEN lr.status = 'preview' THEN 'ready'
                WHEN lr.status IN ('pending', 'running', 'posting') THEN 'reviewing'
                ELSE pr.review_lane
              END AS display_lane,
              CASE
                WHEN pr.review_lane IN ('closed', 'merged') OR pr.state IN ('closed', 'merged') THEN COALESCE(pr.status_reason, 'PR closed or merged')
                WHEN pr.review_lane = 'queued' THEN COALESCE(pr.status_reason, 'Waiting for a free review slot')
                WHEN res.effective_state = 'awaiting_action'
                  AND res.observed_head_sha = pr.head_sha
                  AND lr.head_sha = pr.head_sha
                  AND res.observation = 'findings'
                  AND json_member_is_true(rfa.audit_json, 'complete')
                  AND json_member_type(rfa.audit_json, 'candidates') = 'array'
                  AND json_member_array_length(rfa.audit_json, 'candidates') = 0 THEN 'Nada a sinalizar'
                WHEN lr.status = 'preview' THEN 'AI review ready'
                WHEN lr.status IN ('pending', 'running', 'posting') THEN 'AI review running'
                ELSE pr.status_reason
              END,
              pr.url, COALESCE(pr.github_updated_at, lr.created_at, pr.created_at), pr.decided_by, pr.base_ref, pr.head_ref, pr.base_sha,
              pr.head_sha, lr.head_sha, pr.is_stacked,
              CASE
                WHEN res.effective_state = 'awaiting_action'
                  AND res.observed_head_sha = pr.head_sha
                  AND lr.head_sha = pr.head_sha
                  AND res.observation = 'findings'
                  AND json_member_is_true(rfa.audit_json, 'complete')
                  AND json_member_type(rfa.audit_json, 'candidates') = 'array'
                  AND json_member_array_length(rfa.audit_json, 'candidates') = 0 THEN 'clean'
                ELSE res.effective_state
              END,
              res.observation, res.observed_head_sha, res.reviewed_head_sha,
              res.prior_result_head_sha, res.publication_status, res.error_message,
              lr.review_body, lr.comments_json
       FROM pull_requests pr
       JOIN repos r ON r.id = pr.repo_id
       JOIN subscriptions s ON s.repo_id = r.id AND s.enabled = 1
       LEFT JOIN review_runs lr ON lr.id = (SELECT id FROM review_runs WHERE pr_id = pr.id ORDER BY created_at DESC LIMIT 1)
       LEFT JOIN review_finding_audits rfa ON rfa.run_id = lr.id
       LEFT JOIN review_results res ON res.pr_id = pr.id
         AND res.installation_id = (SELECT value FROM settings WHERE key = 'installation_id')
       ORDER BY pr.number DESC",
    ).map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ReviewCard {
                    id: row.get(0)?,
                    repo_id: row.get(1)?,
                    repo_full_name: row.get(2)?,
                    number: row.get(3)?,
                    title: row.get(4)?,
                    author: row.get(5)?,
                    author_avatar_url: row.get(6)?,
                    state: row.get(7)?,
                    lane: row.get(8)?,
                    status_reason: row.get(9)?,
                    url: row.get(10)?,
                    updated_at: row.get(11)?,
                    decided_by: row.get(12)?,
                    base_ref: row.get(13)?,
                    head_ref: row.get(14)?,
                    base_sha: row.get(15)?,
                    head_sha: row.get(16)?,
                    latest_run_head_sha: row.get(17)?,
                    is_stacked: row.get::<_, i64>(18)? != 0,
                    result_state: row.get(19)?,
                    result_observation: row.get(20)?,
                    result_observed_head_sha: row.get(21)?,
                    result_reviewed_head_sha: row.get(22)?,
                    result_prior_head_sha: row.get(23)?,
                    result_publication_status: row.get(24)?,
                    result_error: row.get(25)?,
                    suggestions_only: crate::review::findings::suggestions_only(
                        row.get::<_, Option<String>>(27)?.as_deref(),
                        row.get::<_, Option<String>>(26)?.as_deref(),
                    ),
                    questions_only: crate::review::findings::questions_only(
                        row.get::<_, Option<String>>(27)?.as_deref(),
                        row.get::<_, Option<String>>(26)?.as_deref(),
                    ),
                    result_inherited: row.get::<_, Option<String>>(20)?.as_deref() == Some("no_new_changes"),
                })
            })
            .map_err(|error| error.to_string())?;
        let cards = rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
        Ok(REVIEW_LANES
            .iter()
            .map(|(lane, title)| ReviewBoardGroup {
                lane: (*lane).to_string(),
                title: (*title).to_string(),
                items: cards.iter().filter(|card| card.lane == *lane).cloned().collect(),
            })
            .collect())
    }

    pub fn sync_review_attention_pendings(&self) -> Result<(), String> {
        let rows = {
            let mut stmt = self.conn.prepare(
                "SELECT pr.id, pr.repo_id, pr.number, pr.review_lane, COALESCE(pr.status_reason, ''), LOWER(pr.state)
                 FROM pull_requests pr JOIN repos r ON r.id = pr.repo_id AND r.removed_at IS NULL",
            ).map_err(|error| error.to_string())?;
            let mapped = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })
                .map_err(|error| error.to_string())?;
            mapped
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        for (pr_id, repo_id, number, lane, reason, state) in rows {
            if matches!(state.as_str(), "closed" | "merged") {
                self.resolve_all_pending_for_pr(pr_id)?;
                continue;
            }
            match lane.as_str() {
                "manual_required" => self.upsert_pending_action(
                    "manual_review_needed",
                    repo_id,
                    pr_id,
                    "Manual review needed",
                    if reason.is_empty() {
                        "Manual AI review required"
                    } else {
                        &reason
                    },
                )?,
                "ready" => self.upsert_pending_action(
                    "review_ready",
                    repo_id,
                    pr_id,
                    "Review ready",
                    if reason.is_empty() { "AI review ready" } else { &reason },
                )?,
                "failed" => {
                    self.upsert_pending_action(
                        "review_failed",
                        repo_id,
                        pr_id,
                        "Review failed",
                        if reason.is_empty() { "AI review failed" } else { &reason },
                    )?;
                }
                _ => {
                    self.resolve_pending_for_pr("manual_review_needed", pr_id)?;
                    self.resolve_pending_for_pr("review_ready", pr_id)?;
                    self.resolve_pending_for_pr("review_failed", pr_id)?;
                }
            }
            let _ = number;
        }
        Ok(())
    }

    pub fn list_pending_actions(&self) -> Result<Vec<PendingAction>, String> {
        let mut stmt = self.conn.prepare(
      "SELECT p.id, p.type, p.status, p.title, p.message, r.full_name, p.pr_id, pr.number, pr.title, pr.author, pr.author_avatar_url, p.created_at, p.seen_at, p.resolved_at
       FROM pending_actions p
       LEFT JOIN repos r ON r.id = p.repo_id
       LEFT JOIN pull_requests pr ON pr.id = p.pr_id
       WHERE p.status != 'resolved'
         AND (p.pr_id IS NULL OR LOWER(pr.state) NOT IN ('closed', 'merged'))
       ORDER BY p.created_at DESC",
    ).map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PendingAction {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    status: row.get(2)?,
                    title: row.get(3)?,
                    message: row.get(4)?,
                    repo_full_name: row.get(5)?,
                    pr_id: row.get(6)?,
                    pr_number: row.get(7)?,
                    pr_title: row.get(8)?,
                    author: row.get(9)?,
                    author_avatar_url: row.get(10)?,
                    created_at: row.get(11)?,
                    seen_at: row.get(12)?,
                    resolved_at: row.get(13)?,
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
    }

    /// Mark a PR's unseen pendings of given kinds as seen (e.g. when the user opens
    /// the review drawer — they've now seen the ready review, so the badge clears).
    pub fn mark_pending_seen_for_pr(&self, pr_id: i64, kinds: &[&str]) -> Result<(), String> {
        let now = now_millis();
        for kind in kinds {
            self.conn.execute(
        "UPDATE pending_actions SET status = 'seen', seen_at = COALESCE(seen_at, $1) WHERE pr_id = $2 AND type = $3 AND status = 'unseen'",
        params![now, pr_id, kind],
      ).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn upsert_pending_action(
        &self,
        kind: &str,
        repo_id: i64,
        pr_id: i64,
        title: &str,
        message: &str,
    ) -> Result<(), String> {
        let now = now_millis();
        // The NOT EXISTS below dedupes only when one writer at a time checks it; the PR's lock
        // makes concurrent calls (overlapping requests) take turns.
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        self.lock_pull_request(pr_id)?;
        self.conn
            .execute(
                "INSERT INTO pending_actions (type, repo_id, pr_id, status, title, message, created_at)
       SELECT $1::TEXT, $2::BIGINT, $3::BIGINT, 'unseen', $4::TEXT, $5::TEXT, $6::BIGINT
       WHERE NOT EXISTS (
         SELECT 1 FROM pending_actions WHERE type = $1 AND pr_id = $3 AND status != 'resolved'
       )",
                params![kind, repo_id, pr_id, title, message, now],
            )
            .map_err(|error| error.to_string())?;
        // Keep title/message current for an existing unresolved action (no new notification).
        self.conn
            .execute(
                "UPDATE pending_actions SET title = $3, message = $4
       WHERE type = $1 AND pr_id = $2 AND status != 'resolved'",
                params![kind, pr_id, title, message],
            )
            .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }

    /// Whether an unresolved pending of this kind already exists for the PR.
    pub fn has_pending_action(&self, kind: &str, pr_id: i64) -> Result<bool, String> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_actions WHERE type = $1 AND pr_id = $2 AND status != 'resolved')",
                params![kind, pr_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value == 1)
            .map_err(|error| error.to_string())
    }

    pub fn resolve_pending_for_pr(&self, kind: &str, pr_id: i64) -> Result<(), String> {
        let now = now_millis();
        self.conn.execute(
      "UPDATE pending_actions SET status = 'resolved', resolved_at = $1 WHERE type = $2 AND pr_id = $3 AND status != 'resolved'",
      params![now, kind, pr_id],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Repo-level pending action (no PR), e.g. "index outdated". Deduped by type+repo.
    pub fn upsert_repo_pending_action(
        &self,
        kind: &str,
        repo_id: i64,
        title: &str,
        message: &str,
    ) -> Result<(), String> {
        let now = now_millis();
        // Repository-level actions have no PR row to lock; a lock keyed by kind and repository
        // makes concurrent calls take turns over the NOT EXISTS check.
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtextextended('pending:' || $1 || ':' || $2, 0))",
            params![kind, repo_id.to_string()],
        )
        .map_err(|error| error.to_string())?;
        self.conn
            .execute(
                "INSERT INTO pending_actions (type, repo_id, pr_id, status, title, message, created_at)
       SELECT $1::TEXT, $2::BIGINT, NULL, 'unseen', $3::TEXT, $4::TEXT, $5::BIGINT
       WHERE NOT EXISTS (
         SELECT 1 FROM pending_actions WHERE type = $1 AND repo_id = $2 AND pr_id IS NULL AND status != 'resolved'
       )",
                params![kind, repo_id, title, message, now],
            )
            .map_err(|error| error.to_string())?;
        self.conn
            .execute(
                "UPDATE pending_actions SET title = $3, message = $4
       WHERE type = $1 AND repo_id = $2 AND pr_id IS NULL AND status != 'resolved'",
                params![kind, repo_id, title, message],
            )
            .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }

    pub fn resolve_repo_pending(&self, kind: &str, repo_id: i64) -> Result<(), String> {
        let now = now_millis();
        self.conn.execute(
      "UPDATE pending_actions SET status = 'resolved', resolved_at = $1 WHERE type = $2 AND repo_id = $3 AND pr_id IS NULL AND status != 'resolved'",
      params![now, kind, repo_id],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// PRs waiting for a free review slot, oldest first — the queue drained by the
    /// poller as running reviews finish.
    pub fn restore_decided_pr_lane(&self, pr_id: i64) -> Result<(), String> {
        self.conn.execute(
            "UPDATE pull_requests
             SET review_lane = COALESCE((
               SELECT CASE
                 WHEN rr.decision = 'clean_result' THEN 'reviewed_cleanly'
                 WHEN rr.decision = 'mark_reviewed' THEN 'approved'
                 WHEN rr.decision = 'skip' THEN 'skipped'
                 WHEN rr.decision IN ('approve_post', 'request_changes', 'comment') AND rr.outcome ILIKE '%auto%' THEN 'auto_posted'
                 WHEN rr.decision IN ('approve_post', 'request_changes', 'comment') THEN 'posted'
                 ELSE NULL
               END
               FROM review_runs rr
               WHERE rr.pr_id = pull_requests.id AND rr.head_sha = pull_requests.head_sha AND rr.decision IS NOT NULL
               ORDER BY rr.decided_at DESC, rr.id DESC
               LIMIT 1
             ), 'approved'),
             status_reason = 'Already reviewed current head',
             updated_at = $1
             WHERE id = $2 AND last_decision_sha = head_sha AND review_lane IN ('open', 'queued', 'reviewing')",
            params![now_millis(), pr_id],
        ).map_err(|error| error.to_string())?;
        self.conn.execute(
            "UPDATE review_runs SET status = 'superseded', outcome = 'Skipped: current head already reviewed', updated_at = $1
             WHERE pr_id = $2 AND status IN ('pending', 'running')",
            params![now_millis(), pr_id],
        ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn queued_pr_ids(&self) -> Result<Vec<i64>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT p.id FROM pull_requests p JOIN repos r ON r.id = p.repo_id AND r.removed_at IS NULL WHERE p.review_lane = 'queued' AND (p.last_decision_sha IS NULL OR p.last_decision_sha != p.head_sha) AND (NOT EXISTS(SELECT 1 FROM pr_comment_activations a WHERE a.pr_id = p.id) OR (p.state = 'open' AND EXISTS(SELECT 1 FROM subscriptions s WHERE s.repo_id = p.repo_id AND s.enabled = 1))) ORDER BY p.updated_at ASC",
            )
            .map_err(|error| error.to_string())?;
        let ids = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        Ok(ids)
    }

    pub fn lifecycle_candidates(
        &self,
        repo_id: i64,
        after: Option<(i64, i64)>,
        limit: i64,
    ) -> Result<Vec<(i64, i64, String, String, i64)>, String> {
        let (after_updated_at, after_id) = after
            .map(|(updated_at, id)| (Some(updated_at), Some(id)))
            .unwrap_or((None, None));
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, number, LOWER(state), review_lane, updated_at
             FROM pull_requests
             WHERE repo_id = $1
               AND (LOWER(state) NOT IN ('closed', 'merged') OR review_lane NOT IN ('closed', 'merged'))
               AND ($2::BIGINT IS NULL OR updated_at > $2 OR (updated_at = $2 AND id > $3))
             ORDER BY updated_at ASC, id ASC LIMIT $4",
            )
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(params![repo_id, after_updated_at, after_id, limit], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
    }

    pub fn mark_pr_terminal(&self, pr_id: i64, state: &str, reason: &str) -> Result<(), String> {
        self.mark_pr_terminal_with_merged_at(pr_id, state, reason, None)
    }

    pub fn mark_pr_terminal_with_merged_at(
        &self,
        pr_id: i64,
        state: &str,
        reason: &str,
        merged_at: Option<i64>,
    ) -> Result<(), String> {
        let lane = if state.eq_ignore_ascii_case("merged") {
            "merged"
        } else {
            "closed"
        };
        let now = now_millis();
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        self.lock_pull_request(pr_id)?;
        self.conn.execute(
            "UPDATE pull_requests SET state = $1, review_lane = $2, status_reason = $3, last_decision_sha = NULL, updated_at = $4, merged_at = COALESCE($5, merged_at) WHERE id = $6",
            params![state.to_ascii_lowercase(), lane, reason, now, merged_at, pr_id],
        ).map_err(|error| error.to_string())?;
        self.conn.execute(
            "UPDATE review_runs SET status = 'superseded', error_message = $1, finished_at = COALESCE(finished_at, $2), updated_at = $2 WHERE pr_id = $3 AND status IN ('pending', 'running', 'posting', 'preview')",
            params![reason, now, pr_id],
        ).map_err(|error| error.to_string())?;
        self.conn.execute(
            "UPDATE review_results SET publication_status = 'superseded', error_message = $1, updated_at = $2 WHERE pr_id = $3 AND publication_status IN ('pending', 'posting')",
            params![reason, now, pr_id],
        ).map_err(|error| error.to_string())?;
        self.resolve_all_pending_for_pr(pr_id)?;
        tx.commit().map_err(|error| error.to_string())
    }

    pub fn set_pr_merged_at(&self, pr_id: i64, merged_at: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE pull_requests SET merged_at = $1 WHERE id = $2",
                params![merged_at, pr_id],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn review_run_retention_info(&self, run_id: i64) -> Result<Option<(String, i64)>, String> {
        self.conn
            .query_row(
                "SELECT status, pr_id FROM review_runs WHERE id = $1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())
    }

    pub fn pr_retention_info(&self, pr_id: i64) -> Result<Option<(String, Option<i64>)>, String> {
        self.conn
            .query_row(
                "SELECT state, merged_at FROM pull_requests WHERE id = $1",
                params![pr_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())
    }

    pub fn find_candidate_prs_for_merge_backfill(&self, run_ids: &[i64]) -> Result<Vec<(i64, String, i64)>, String> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT p.id, r.full_name, p.number
                 FROM review_runs rr
                 JOIN pull_requests p ON rr.pr_id = p.id
                 JOIN repos r ON p.repo_id = r.id
                 WHERE rr.id = ANY($1) AND p.merged_at IS NULL",
            )
            .map_err(|e| e.to_string())?;
        let candidates = stmt
            .query_map(params![run_ids], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
            })
            .map_err(|e| e.to_string())?
            .flatten()
            .collect();
        Ok(candidates)
    }

    pub fn mark_pr_size_skipped(&self, pr_id: i64, limit: i64) -> Result<(), String> {
        let count: Option<i64> = self
            .conn
            .query_row(
                "SELECT changed_file_count FROM pr_review_file_counts WHERE pr_id = $1",
                params![pr_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let reason = format!(
            "Skipped: {} reviewable changed files exceeds the {}-file review limit. Change Maximum PR files in Settings.",
            count.unwrap_or_default(),
            limit
        );
        self.set_pr_lane_if_open(pr_id, "size_skipped", Some(&reason))?;
        self.resolve_pending_for_pr("manual_review_needed", pr_id)
    }

    pub fn set_pr_lane_if_open(&self, pr_id: i64, lane: &str, status_reason: Option<&str>) -> Result<bool, String> {
        let changed = self.conn.execute(
            "UPDATE pull_requests SET review_lane = $1, status_reason = $2, updated_at = $3 WHERE id = $4 AND LOWER(state) NOT IN ('closed', 'merged')",
            params![lane, status_reason, now_millis(), pr_id],
        ).map_err(|error| error.to_string())?;
        Ok(changed == 1)
    }

    pub fn set_pr_lane(&self, pr_id: i64, lane: &str, status_reason: Option<&str>) -> Result<(), String> {
        let now = now_millis();
        self.conn
            .execute(
                "UPDATE pull_requests SET review_lane = $1, status_reason = $2, updated_at = $3 WHERE id = $4",
                params![lane, status_reason, now, pr_id],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn resolve_all_pending_for_pr(&self, pr_id: i64) -> Result<(), String> {
        let now = now_millis();
        self.conn.execute(
      "UPDATE pending_actions SET status = 'resolved', resolved_at = $1 WHERE pr_id = $2 AND status != 'resolved'",
      params![now, pr_id],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) fn upsert_pending_action_for_run(
        &self,
        kind: &str,
        pr_id: i64,
        run_id: i64,
        title: &str,
        message: &str,
    ) -> Result<(), String> {
        let now = now_millis();
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        self.lock_pull_request(pr_id)?;
        let repo_id: i64 = self
            .conn
            .query_row(
                "SELECT repo_id FROM pull_requests WHERE id = $1",
                params![pr_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        self.conn
            .execute(
                "INSERT INTO pending_actions (type, repo_id, pr_id, review_run_id, status, title, message, created_at)
       SELECT $1::TEXT, $2::BIGINT, $3::BIGINT, $4::BIGINT, 'unseen', $5::TEXT, $6::TEXT, $7::BIGINT
       WHERE NOT EXISTS (
         SELECT 1 FROM pending_actions WHERE type = $1 AND pr_id = $3 AND status != 'resolved'
       )",
                params![kind, repo_id, pr_id, run_id, title, message, now],
            )
            .map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())
    }
}
