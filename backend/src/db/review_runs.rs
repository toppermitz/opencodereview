use super::*;

impl Database {
    /// Serializes changes to one pull request's review state. SQLite let one writer in at a time;
    /// Postgres lets concurrent transactions interleave their checks and writes, which could post
    /// a review twice. A transaction that reads a PR's state to decide a write takes this first,
    /// so row locks are always taken pull request first.
    pub(crate) fn lock_pull_request(&self, pr_id: i64) -> Result<(), String> {
        debug_assert!(
            !self.conn.is_autocommit(),
            "a row lock outside a transaction ends at once"
        );
        self.conn
            .execute("SELECT 1 FROM pull_requests WHERE id = $1 FOR UPDATE", [pr_id])
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// `lock_pull_request` for the pull request a run belongs to.
    pub(crate) fn lock_run_pull_request(&self, run_id: i64) -> Result<(), String> {
        debug_assert!(
            !self.conn.is_autocommit(),
            "a row lock outside a transaction ends at once"
        );
        self.conn
            .execute(
                "SELECT 1 FROM pull_requests WHERE id = (SELECT pr_id FROM review_runs WHERE id = $1) FOR UPDATE",
                [run_id],
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Records the engine a run is reviewing with, such as "Claude · Opus 5.5 · medium".
    pub(crate) fn set_review_run_model(&self, run_id: i64, model: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE review_runs SET model = $1 WHERE id = $2",
                params![model, run_id],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Validate the index and open a review run for a PR. Returns the run id.
    pub fn start_review_skeleton_with(&self, pr_id: i64, model: Option<&str>) -> Result<i64, String> {
        let now = now_millis();
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        // Archive retention holds the gate exclusively while it decides what to delete.
        tx.execute("SELECT pg_advisory_xact_lock_shared($1)", [ARCHIVE_GATE_KEY])
            .map_err(|error| error.to_string())?;
        let (repo_id, head_sha): (i64, String) = self
            .conn
            .query_row(
                "SELECT repo_id, head_sha FROM pull_requests WHERE id = $1 FOR UPDATE",
                params![pr_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| error.to_string())?;
        // Another instance may run a review of this PR; the PR lock above makes this check stick.
        let running: bool = self
            .conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM review_runs WHERE pr_id = $1 AND status IN ('pending', 'running'))",
                params![pr_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if running {
            return Err("A review is already running for this PR.".to_string());
        }
        self.repo_index_status(repo_id)?;
        let run_id: i64 = self.conn.query_row(
      "INSERT INTO review_runs (pr_id, head_sha, status, created_at, updated_at, started_at, model, human_selection_required, review_scope_revision, owner_instance)
       VALUES ($1, $2, 'pending', $3, $3, $3, $4, 1, (SELECT review_scope_revision FROM pull_requests WHERE id = $1), $5) RETURNING id",
      params![pr_id, head_sha, now, model, self.db.instance_id()],
      |row| row.get(0),
    ).map_err(|error| error.to_string())?;
        self.conn.execute("UPDATE pull_requests SET review_lane = 'reviewing', status_reason = 'AI review queued', updated_at = $1 WHERE id = $2", params![now, pr_id])
      .map_err(|error| error.to_string())?;
        // A new run supersedes any prior pending state for this PR.
        self.resolve_pending_for_pr("manual_review_needed", pr_id)?;
        self.resolve_pending_for_pr("review_ready", pr_id)?;
        self.resolve_pending_for_pr("review_failed", pr_id)?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(run_id)
    }

    pub fn latest_pr_summary_for_head(&self, pr_id: i64, head_sha: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row(
                "SELECT status FROM pr_summaries WHERE pr_id = $1 AND head_sha = $2 ORDER BY id DESC LIMIT 1",
                params![pr_id, head_sha],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                pg::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other.to_string()),
            })
    }

    pub fn start_pr_summary(&self, pr_id: i64, head_sha: &str) -> Result<i64, String> {
        let now = now_millis();
        self.conn.query_row(
            "INSERT INTO pr_summaries (pr_id, head_sha, status, created_at, updated_at) VALUES ($1, $2, 'running', $3, $3) RETURNING id",
            params![pr_id, head_sha, now],
            |row| row.get(0),
        ).map_err(|error| error.to_string())
    }

    pub fn finish_pr_summary(
        &self,
        id: i64,
        status: &str,
        body: Option<&str>,
        summary_json: Option<&str>,
        github_comment_id: Option<i64>,
        error_message: Option<&str>,
    ) -> Result<(), String> {
        let now = now_millis();
        let posted_at = (status == "posted").then_some(now);
        self.conn.execute(
            "UPDATE pr_summaries SET status = $2, body = $3, summary_json = $4, github_comment_id = $5, error_message = $6, updated_at = $7, posted_at = COALESCE($8, posted_at) WHERE id = $1",
            params![id, status, body, summary_json, github_comment_id, error_message, now, posted_at],
        ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn set_review_run_context_metadata(
        &self,
        run_id: i64,
        reviewed_base_sha: Option<&str>,
        reviewed_head_sha: &str,
        context_index_sha: Option<&str>,
        stacked_base_ref: Option<&str>,
        stacked_base_sha: Option<&str>,
    ) -> Result<(), String> {
        self.conn.execute(
      "UPDATE review_runs SET reviewed_base_sha = $2, reviewed_head_sha = $3, context_index_sha = $4, stacked_base_ref = $5, stacked_base_sha = $6 WHERE id = $1",
      params![run_id, reviewed_base_sha, reviewed_head_sha, context_index_sha, stacked_base_ref, stacked_base_sha],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// On startup, no review/index jobs are alive in memory. Any run still marked
    /// in-flight in the DB was orphaned by a restart/crash — reconcile it so the UI
    /// never shows a stuck "reviewing"/"indexing" state with no live work.
    pub fn reconcile_interrupted_jobs(&self) -> Result<(), String> {
        self.reclaim_orphaned_work()?;
        let now = now_millis();
        // Heal pending actions that contradict the PR's current lane (stale ready/failed).
        self.conn
            .execute(
                "UPDATE pending_actions SET status = 'resolved', resolved_at = $1
       WHERE type = 'review_failed' AND status != 'resolved'
         AND pr_id IN (SELECT id FROM pull_requests WHERE review_lane = 'ready')",
                params![now],
            )
            .map_err(|error| error.to_string())?;
        self.conn
            .execute(
                "UPDATE pending_actions SET status = 'resolved', resolved_at = $1
       WHERE type = 'review_ready' AND status != 'resolved'
         AND pr_id IN (SELECT id FROM pull_requests WHERE review_lane != 'ready')",
                params![now],
            )
            .map_err(|error| error.to_string())?;
        // Undo old "external review" reconciliation: a GitHub approval does not replace
        // a bot review. Put those PRs back into the normal reviewable flow.
        self.conn.execute(
      "UPDATE pull_requests SET review_lane = 'open', status_reason = NULL, last_decision_sha = NULL, updated_at = $1
       WHERE status_reason ILIKE 'Reviewed on GitHub%'",
      params![now],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn latest_review_run(&self, pr_id: i64) -> Result<Option<ReviewRun>, String> {
        let mut stmt = self.conn.prepare(
      "SELECT id, pr_id, status, head_sha, review_body, comments_json, risk_score, risk_level, risk_json, error_message, phase_json, tail, started_at, finished_at, created_at, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, usage_model, model, context_index_sha, reviewed_head_sha
       FROM review_runs WHERE pr_id = $1
       ORDER BY CASE
         WHEN (SELECT review_lane FROM pull_requests WHERE id = $1) = 'ready'
          AND status = 'preview' THEN 0
         ELSE 1
       END, created_at DESC LIMIT 1",
    ).map_err(|error| error.to_string())?;
        let mut rows = stmt.query(params![pr_id]).map_err(|error| error.to_string())?;
        if let Some(row) = rows.next().map_err(|error| error.to_string())? {
            Ok(Some(ReviewRun {
                id: row.get(0).map_err(|error| error.to_string())?,
                pr_id: row.get(1).map_err(|error| error.to_string())?,
                status: row.get(2).map_err(|error| error.to_string())?,
                head_sha: row.get(3).map_err(|error| error.to_string())?,
                review_body: row.get(4).map_err(|error| error.to_string())?,
                comments_json: row.get(5).map_err(|error| error.to_string())?,
                risk_score: row.get(6).map_err(|error| error.to_string())?,
                risk_level: row.get(7).map_err(|error| error.to_string())?,
                risk_json: row.get(8).map_err(|error| error.to_string())?,
                error_message: row.get(9).map_err(|error| error.to_string())?,
                phase_json: row.get(10).map_err(|error| error.to_string())?,
                tail: row.get(11).map_err(|error| error.to_string())?,
                started_at: row.get(12).map_err(|error| error.to_string())?,
                finished_at: row.get(13).map_err(|error| error.to_string())?,
                created_at: row.get(14).map_err(|error| error.to_string())?,
                input_tokens: row.get(15).map_err(|error| error.to_string())?,
                output_tokens: row.get(16).map_err(|error| error.to_string())?,
                cache_read_tokens: row.get(17).map_err(|error| error.to_string())?,
                cache_creation_tokens: row.get(18).map_err(|error| error.to_string())?,
                usage_model: row.get(19).map_err(|error| error.to_string())?,
                model: row.get(20).map_err(|error| error.to_string())?,
                context_index_sha: row.get(21).map_err(|error| error.to_string())?,
                reviewed_head_sha: row.get(22).map_err(|error| error.to_string())?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn review_progress_snapshot(&self, pr_id: i64) -> Result<Option<ReviewProgressSnapshot>, String> {
        let mut stmt = self.conn.prepare(
            "SELECT id, pr_id, status, head_sha, error_message, phase_json, tail, started_at, finished_at, updated_at
             FROM review_runs WHERE pr_id = $1
             ORDER BY CASE
               WHEN (SELECT review_lane FROM pull_requests WHERE id = $1) = 'ready'
                AND status = 'preview' THEN 0
               ELSE 1
             END, created_at DESC LIMIT 1",
        ).map_err(|error| error.to_string())?;
        let mut rows = stmt.query(params![pr_id]).map_err(|error| error.to_string())?;
        if let Some(row) = rows.next().map_err(|error| error.to_string())? {
            Ok(Some(ReviewProgressSnapshot {
                run_id: row.get(0).map_err(|error| error.to_string())?,
                pr_id: row.get(1).map_err(|error| error.to_string())?,
                status: row.get(2).map_err(|error| error.to_string())?,
                head_sha: row.get(3).map_err(|error| error.to_string())?,
                error_message: row.get(4).map_err(|error| error.to_string())?,
                phase_json: row.get(5).map_err(|error| error.to_string())?,
                tail: row.get(6).map_err(|error| error.to_string())?,
                started_at: row.get(7).map_err(|error| error.to_string())?,
                finished_at: row.get(8).map_err(|error| error.to_string())?,
                updated_at: row.get(9).map_err(|error| error.to_string())?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn review_run_by_id(&self, run_id: i64) -> Result<Option<ReviewRun>, String> {
        let mut stmt = self.conn.prepare(
      "SELECT id, pr_id, status, head_sha, review_body, comments_json, risk_score, risk_level, risk_json, error_message, phase_json, tail, started_at, finished_at, created_at, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, usage_model, model, context_index_sha, reviewed_head_sha
       FROM review_runs WHERE id = $1",
    ).map_err(|error| error.to_string())?;
        let mut rows = stmt.query(params![run_id]).map_err(|error| error.to_string())?;
        if let Some(row) = rows.next().map_err(|error| error.to_string())? {
            Ok(Some(ReviewRun {
                id: row.get(0).map_err(|error| error.to_string())?,
                pr_id: row.get(1).map_err(|error| error.to_string())?,
                status: row.get(2).map_err(|error| error.to_string())?,
                head_sha: row.get(3).map_err(|error| error.to_string())?,
                review_body: row.get(4).map_err(|error| error.to_string())?,
                comments_json: row.get(5).map_err(|error| error.to_string())?,
                risk_score: row.get(6).map_err(|error| error.to_string())?,
                risk_level: row.get(7).map_err(|error| error.to_string())?,
                risk_json: row.get(8).map_err(|error| error.to_string())?,
                error_message: row.get(9).map_err(|error| error.to_string())?,
                phase_json: row.get(10).map_err(|error| error.to_string())?,
                tail: row.get(11).map_err(|error| error.to_string())?,
                started_at: row.get(12).map_err(|error| error.to_string())?,
                finished_at: row.get(13).map_err(|error| error.to_string())?,
                created_at: row.get(14).map_err(|error| error.to_string())?,
                input_tokens: row.get(15).map_err(|error| error.to_string())?,
                output_tokens: row.get(16).map_err(|error| error.to_string())?,
                cache_read_tokens: row.get(17).map_err(|error| error.to_string())?,
                cache_creation_tokens: row.get(18).map_err(|error| error.to_string())?,
                usage_model: row.get(19).map_err(|error| error.to_string())?,
                model: row.get(20).map_err(|error| error.to_string())?,
                context_index_sha: row.get(21).map_err(|error| error.to_string())?,
                reviewed_head_sha: row.get(22).map_err(|error| error.to_string())?,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn review_runs_for_pr(&self, pr_id: i64) -> Result<Vec<ReviewRun>, String> {
        let mut stmt = self.conn.prepare(
      "SELECT id, pr_id, status, head_sha, review_body, comments_json, risk_score, risk_level, risk_json, error_message, phase_json, tail, started_at, finished_at, created_at, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, usage_model, model, context_index_sha, reviewed_head_sha
       FROM review_runs WHERE pr_id = $1 ORDER BY created_at DESC",
    ).map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(params![pr_id], |row| {
                Ok(ReviewRun {
                    id: row.get(0)?,
                    pr_id: row.get(1)?,
                    status: row.get(2)?,
                    head_sha: row.get(3)?,
                    review_body: row.get(4)?,
                    comments_json: row.get(5)?,
                    risk_score: row.get(6)?,
                    risk_level: row.get(7)?,
                    risk_json: row.get(8)?,
                    error_message: row.get(9)?,
                    phase_json: row.get(10)?,
                    tail: row.get(11)?,
                    started_at: row.get(12)?,
                    finished_at: row.get(13)?,
                    created_at: row.get(14)?,
                    input_tokens: row.get(15)?,
                    output_tokens: row.get(16)?,
                    cache_read_tokens: row.get(17)?,
                    cache_creation_tokens: row.get(18)?,
                    usage_model: row.get(19)?,
                    model: row.get(20)?,
                    context_index_sha: row.get(21)?,
                    reviewed_head_sha: row.get(22)?,
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
    }

    /// Persist token usage parsed from a run's export transcript.
    pub fn set_review_run_usage(
        &self,
        run_id: i64,
        input: i64,
        output: i64,
        cache_read: i64,
        cache_creation: i64,
        model: Option<&str>,
    ) -> Result<(), String> {
        let now = now_millis();
        self.conn.execute(
      "UPDATE review_runs SET input_tokens = $1, output_tokens = $2, cache_read_tokens = $3, cache_creation_tokens = $4, usage_model = $5, updated_at = $6 WHERE id = $7",
      params![input, output, cache_read, cache_creation, model, now, run_id],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Per-run usage detail for a PR (newest first), each with its per-model split.
    pub fn insert_review_run_stage(&self, stage: &ReviewRunStageInput) -> Result<(), String> {
        self.conn.execute(
            "INSERT INTO review_run_stages (run_id, pr_id, stage, chunk_id, model, status, cache_key, cache_hit, potential_cache_hit, potential_savings_tokens, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, prompt_bytes, diff_bytes, context_bytes, file_count, related_file_count, created_at, finished_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)",
            params![
                stage.run_id,
                stage.pr_id,
                stage.stage,
                stage.chunk_id,
                stage.model,
                stage.status,
                stage.cache_key,
                stage.cache_hit as i64,
                stage.potential_cache_hit as i64,
                stage.potential_savings_tokens,
                stage.input_tokens,
                stage.output_tokens,
                stage.cache_read_tokens,
                stage.cache_creation_tokens,
                stage.prompt_bytes,
                stage.diff_bytes,
                stage.context_bytes,
                stage.file_count,
                stage.related_file_count,
                stage.started_at,
                stage.finished_at,
            ],
        ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn sync_review_stage_usage(&self, run_id: i64) -> Result<(), String> {
        self.conn.execute(
            "UPDATE review_runs SET (input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens) = (SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), COALESCE(SUM(cache_read_tokens),0), COALESCE(SUM(cache_creation_tokens),0) FROM review_run_stages WHERE run_id = $1) WHERE id = $1 AND EXISTS (SELECT 1 FROM review_run_stages WHERE run_id = $1)",
            params![run_id],
        ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn review_stage_breakdown(&self, run_id: i64) -> Result<ReviewStageBreakdown, String> {
        let pr_id = self
            .conn
            .query_row("SELECT pr_id FROM review_runs WHERE id = $1", params![run_id], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, pr_id, stage, chunk_id, model, status, cache_key, cache_hit, potential_cache_hit, potential_savings_tokens, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, prompt_bytes, diff_bytes, context_bytes, file_count, related_file_count, created_at, finished_at
             FROM review_run_stages WHERE run_id = $1 ORDER BY CASE stage WHEN 'single' THEN 0 WHEN 'chunk' THEN 1 WHEN 'final' THEN 2 ELSE 3 END, COALESCE(chunk_id, 0), id",
        ).map_err(|error| error.to_string())?;
        let stages: Vec<ReviewRunStage> = stmt
            .query_map(params![run_id], |row| {
                Ok(ReviewRunStage {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    pr_id: row.get(2)?,
                    stage: row.get(3)?,
                    chunk_id: row.get(4)?,
                    model: row.get(5)?,
                    status: row.get(6)?,
                    cache_key: row.get(7)?,
                    cache_hit: row.get::<_, i64>(8)? != 0,
                    potential_cache_hit: row.get::<_, i64>(9)? != 0,
                    potential_savings_tokens: row.get(10)?,
                    input_tokens: row.get(11)?,
                    output_tokens: row.get(12)?,
                    cache_read_tokens: row.get(13)?,
                    cache_creation_tokens: row.get(14)?,
                    prompt_bytes: row.get(15)?,
                    diff_bytes: row.get(16)?,
                    context_bytes: row.get(17)?,
                    file_count: row.get(18)?,
                    related_file_count: row.get(19)?,
                    created_at: row.get(20)?,
                    finished_at: row.get(21)?,
                })
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let total_tokens = stages
            .iter()
            .map(|stage| stage.input_tokens + stage.output_tokens)
            .sum();
        let potential_cache_hits = stages.iter().filter(|stage| stage.potential_cache_hit).count() as i64;
        let potential_cache_savings_tokens = stages.iter().map(|stage| stage.potential_savings_tokens).sum();
        Ok(ReviewStageBreakdown {
            run_id,
            pr_id,
            stages,
            total_tokens,
            potential_cache_hits,
            potential_cache_savings_tokens,
        })
    }

    pub fn set_review_run_running(&self, run_id: i64, phase_json: &str) -> Result<(), String> {
        let now = now_millis();
        self.conn
            .execute(
                "UPDATE review_runs SET status = 'running', phase_json = $1, updated_at = $2 WHERE id = $3",
                params![phase_json, now, run_id],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn update_review_run_progress(&self, run_id: i64, phase_json: &str, tail: &str) -> Result<bool, String> {
        let now = now_millis();
        let previous: Option<String> = self
            .conn
            .query_row(
                "SELECT phase_json FROM review_runs WHERE id = $1",
                params![run_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let phase_json = merge_review_phase_progress(previous.as_deref(), phase_json, tail, now);
        let tail: String = tail
            .chars()
            .rev()
            .take(32_768)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        let changed = self.conn.execute(
      "UPDATE review_runs SET phase_json = $1, tail = $2, updated_at = $3 WHERE id = $4 AND status = 'running'",
      params![phase_json, tail, now, run_id],
    ).map_err(|error| error.to_string())?;
        Ok(changed > 0)
    }

    pub fn finish_review_run_only(
        &self,
        run_id: i64,
        body: &str,
        comments_json: Option<&str>,
        phase_json: &str,
    ) -> Result<bool, String> {
        let now = now_millis();
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        self.lock_run_pull_request(run_id)?;
        let stale_head: bool = self
            .conn
            .query_row(
                "SELECT rr.head_sha != pr.head_sha OR LOWER(pr.state) IN ('closed', 'merged') FROM review_runs rr JOIN pull_requests pr ON pr.id = rr.pr_id WHERE rr.id = $1",
                params![run_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        if stale_head {
            self.conn.execute(
                "UPDATE review_runs SET status = 'superseded', review_body = $1, comments_json = $2, phase_json = $3, outcome = 'Superseded by new commits before completion', finished_at = $4, updated_at = $4 WHERE id = $5 AND status = 'running'",
                params![body, comments_json, phase_json, now, run_id],
            ).map_err(|error| error.to_string())?;
            tx.commit().map_err(|error| error.to_string())?;
            return Ok(false);
        }
        let changed = self.conn.execute(
      "UPDATE review_runs SET status = 'preview', review_body = $1, comments_json = $2, phase_json = $3, finished_at = $4, updated_at = $4 WHERE id = $5 AND status = 'running'",
      params![body, comments_json, phase_json, now, run_id],
    ).map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(changed > 0)
    }

    pub fn set_review_run_comments_json(&self, run_id: i64, comments_json: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE review_runs SET comments_json = $1, updated_at = $2 WHERE id = $3",
                params![comments_json, now_millis(), run_id],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn set_review_run_risk(&self, run_id: i64, risk: &ReviewRisk) -> Result<(), String> {
        let risk_json = serde_json::to_string(risk).map_err(|error| error.to_string())?;
        let now = now_millis();
        self.conn.execute(
      "UPDATE review_runs SET risk_score = $1, risk_level = $2, risk_json = $3, updated_at = $4 WHERE id = $5",
      params![risk.score, risk.level, risk_json, now, run_id],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn mark_review_ready_for_run(&self, pr_id: i64, run_id: i64) -> Result<(), String> {
        let now = now_millis();
        self.conn.execute("UPDATE pull_requests SET review_lane = 'ready', status_reason = 'AI review ready', updated_at = $1 WHERE id = $2", params![now, pr_id])
      .map_err(|error| error.to_string())?;
        self.resolve_pending_for_pr("review_failed", pr_id)?;
        self.upsert_pending_action_for_run(
            "review_ready",
            pr_id,
            run_id,
            "AI review ready",
            "A generated review is ready for your approval.",
        )
    }

    pub fn finish_review_run_no_new_changes(
        &self,
        run_id: i64,
        body: &str,
        comments_json: Option<&str>,
        phase_json: &str,
    ) -> Result<bool, String> {
        self.finish_review_run_only(run_id, body, comments_json, phase_json)
    }

    pub fn finish_review_run(
        &self,
        run_id: i64,
        body: &str,
        comments_json: Option<&str>,
        phase_json: &str,
    ) -> Result<bool, String> {
        let pr_id: i64 = self
            .conn
            .query_row("SELECT pr_id FROM review_runs WHERE id = $1", params![run_id], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        if !self.finish_review_run_only(run_id, body, comments_json, phase_json)? {
            return Ok(false);
        }
        self.mark_review_ready_for_run(pr_id, run_id)?;
        Ok(true)
    }

    pub fn skip_review_run_for_file_limit(&self, run_id: i64, reason: &str) -> Result<(), String> {
        let now = now_millis();
        let pr_id: i64 = self
            .conn
            .query_row("SELECT pr_id FROM review_runs WHERE id = $1", params![run_id], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        let run_changed = self.conn.execute(
            "UPDATE review_runs SET status = 'skipped', error_message = $1, outcome = $1, finished_at = $2, updated_at = $2 WHERE id = $3 AND status IN ('pending','running')",
            params![reason, now, run_id],
        ).map_err(|error| error.to_string())? > 0;
        if run_changed {
            self.mark_pr_size_skipped(pr_id, self.settings()?.review_max_files)?;
        }
        self.resolve_all_pending_for_pr(pr_id)
    }

    /// Skips a run whose pull request changed only files excluded from review evidence. The PR is
    /// left `skipped` until its head changes; nothing is reviewed or marked clean.
    pub fn skip_review_run_without_reviewable_files(&self, run_id: i64, reason: &str) -> Result<(), String> {
        let now = now_millis();
        let pr_id: i64 = self
            .conn
            .query_row("SELECT pr_id FROM review_runs WHERE id = $1", params![run_id], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        let run_changed = self.conn.execute(
            "UPDATE review_runs SET status = 'skipped', error_message = $1, outcome = $1, finished_at = $2, updated_at = $2 WHERE id = $3 AND status IN ('pending','running')",
            params![reason, now, run_id],
        ).map_err(|error| error.to_string())? > 0;
        if run_changed {
            self.set_pr_lane_if_open(
                pr_id,
                "skipped",
                Some("Not reviewed: only generated files, lockfiles or other excluded paths changed"),
            )?;
        }
        self.resolve_all_pending_for_pr(pr_id)
    }

    pub fn fail_review_run(&self, run_id: i64, error_message: &str) -> Result<(), String> {
        let now = now_millis();
        let pr_id: i64 = self
            .conn
            .query_row("SELECT pr_id FROM review_runs WHERE id = $1", params![run_id], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        self.conn.execute(
      "UPDATE review_runs SET status = 'failed', error_message = $1, finished_at = $2, updated_at = $2 WHERE id = $3 AND status IN ('pending','running')",
      params![error_message, now, run_id],
    ).map_err(|error| error.to_string())?;
        let lane_changed = self.conn.execute("UPDATE pull_requests SET review_lane = 'failed', status_reason = 'AI review failed', updated_at = $1 WHERE id = $2 AND LOWER(state) NOT IN ('closed', 'merged')", params![now, pr_id])
      .map_err(|error| error.to_string())?;
        self.resolve_pending_for_pr("review_ready", pr_id)?;
        if lane_changed == 0 {
            return self.resolve_all_pending_for_pr(pr_id);
        }
        self.upsert_pending_action_for_run("review_failed", pr_id, run_id, "AI review failed", error_message)
    }

    pub fn cancel_review_run_only(&self, run_id: i64) -> Result<(), String> {
        let now = now_millis();
        self.conn.execute(
      "UPDATE review_runs SET status = 'cancelled', error_message = 'Review cancelled by user.', finished_at = $1, updated_at = $1,
         phase_json = REPLACE(COALESCE(phase_json, ''), '\"status\":\"running\"', '\"status\":\"cancelled\"')
       WHERE id = $2 AND status IN ('pending','running')",
      params![now, run_id],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn cancel_review_run(&self, run_id: i64) -> Result<(), String> {
        let now = now_millis();
        let pr_id: i64 = self
            .conn
            .query_row("SELECT pr_id FROM review_runs WHERE id = $1", params![run_id], |row| {
                row.get(0)
            })
            .map_err(|error| error.to_string())?;
        self.cancel_review_run_only(run_id)?;
        self.conn.execute("UPDATE pull_requests SET review_lane = 'manual_required', status_reason = 'Review cancelled', updated_at = $1 WHERE id = $2 AND LOWER(state) NOT IN ('closed', 'merged')", params![now, pr_id])
      .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn cancel_review_for_pr(&self, pr_id: i64) -> Result<(), String> {
        self.set_reprocess_next_review(pr_id, false)?;
        let now = now_millis();
        self.conn.execute(
      "UPDATE review_runs SET status = 'cancelled', error_message = 'Review cancelled by user.', finished_at = $1, updated_at = $1,
         phase_json = REPLACE(COALESCE(phase_json, ''), '\"status\":\"running\"', '\"status\":\"cancelled\"')
       WHERE pr_id = $2 AND status IN ('pending','running','posting')",
      params![now, pr_id],
    ).map_err(|error| error.to_string())?;
        self.conn.execute("UPDATE pull_requests SET review_lane = 'manual_required', status_reason = 'Review cancelled', updated_at = $1 WHERE id = $2 AND LOWER(state) NOT IN ('closed', 'merged')", params![now, pr_id])
      .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn try_begin_review_post(
        &self,
        run_id: i64,
        pr_id: i64,
        head_sha: &str,
        event: &str,
        decision: &str,
        actor: &str,
        selection_revision: Option<i64>,
    ) -> Result<bool, String> {
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        self.lock_pull_request(pr_id)?;
        if actor == "auto" {
            let settings = self.settings()?;
            let run = self.review_run_by_id(run_id)?.ok_or("Review run not found.")?;
            let allowed = match (event, decision) {
                ("APPROVE", "approve_post") => {
                    settings.auto_approve_enabled
                        && run
                            .risk_score
                            .is_some_and(|score| score <= settings.auto_approve_max_risk_score)
                        && !crate::review::findings::actionable(
                            run.comments_json.as_deref(),
                            run.review_body.as_deref(),
                        )
                }
                ("COMMENT", "comment") | ("REQUEST_CHANGES", "request_changes") => {
                    crate::review::findings::auto_post_event(
                        &settings.auto_post_mode,
                        run.comments_json.as_deref(),
                        run.review_body.as_deref(),
                    ) == Some(event)
                }
                _ => false,
            };
            if !allowed {
                return Ok(false);
            }
            if !matches!((event, decision), ("APPROVE", "approve_post")) {
                let view = self.finding_audit(pr_id, run_id)?;
                if view.requires_confirmation && (!view.can_auto_post() || selection_revision != Some(view.revision)) {
                    return Ok(false);
                }
            }
        }
        let changed = self.conn.execute(
            "UPDATE review_runs
             SET status = 'posting', event = $5, decision = $6, outcome = $7, updated_at = $1, owner_instance = $9
             WHERE id = $2 AND pr_id = $3 AND head_sha = $4 AND status = 'preview'
               AND EXISTS (SELECT 1 FROM pull_requests pr WHERE pr.id = $3 AND pr.force_full_next_review = 0 AND pr.reprocess_next_review = 0 AND pr.review_scope_revision = review_runs.review_scope_revision AND (review_runs.stacked_base_ref IS NULL OR review_runs.stacked_base_ref IS NOT DISTINCT FROM pr.base_ref))
               AND (human_selection_required = 0 OR (
                 $2 = (SELECT id FROM review_runs WHERE pr_id = $3 ORDER BY created_at DESC, id DESC LIMIT 1)
                 AND EXISTS (SELECT 1 FROM pull_requests WHERE id = $3 AND head_sha = $4 AND LOWER(state) = 'open')
                 AND EXISTS (SELECT 1 FROM review_finding_audits WHERE run_id = $2 AND revision = $8 AND selection_saved = 1)
               ))
               AND NOT EXISTS (
                 SELECT 1 FROM pull_requests
                 WHERE id = $3 AND last_decision_sha = $4 AND review_lane IN ('posted', 'auto_posted', 'reviewed_cleanly', 'approved')
               )
               AND NOT EXISTS (
                 SELECT 1 FROM review_runs other
                 WHERE other.pr_id = $3 AND other.head_sha = $4 AND other.id != $2
                   AND (other.status = 'posting' OR (other.status = 'posted' AND other.stacked_base_ref IS NOT DISTINCT FROM review_runs.stacked_base_ref AND other.reviewed_base_sha IS NOT DISTINCT FROM review_runs.reviewed_base_sha))
               )",
            params![
                now_millis(),
                run_id,
                pr_id,
                head_sha,
                event,
                decision,
                format!("Posting ({actor})"),
                selection_revision,
                self.db.instance_id(),
            ],
        ).map_err(|error| error.to_string())?;
        tx.commit().map_err(|error| error.to_string())?;
        Ok(changed == 1)
    }

    pub fn reset_review_posting(&self, run_id: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE review_runs SET status = 'preview', event = NULL, decision = NULL,
                    outcome = NULL, updated_at = $1
             WHERE id = $2 AND status = 'posting'",
                params![now_millis(), run_id],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// All four writes happen together or not at all, under the pull request's lock.
    pub fn preserve_awaiting_action_after_no_changes(
        &self,
        run_id: i64,
        pr_id: i64,
        actionable_run_id: i64,
    ) -> Result<(), String> {
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        self.lock_pull_request(pr_id)?;
        self.preserve_awaiting_action_locked(run_id, pr_id, actionable_run_id)?;
        tx.commit().map_err(|error| error.to_string())
    }

    fn preserve_awaiting_action_locked(&self, run_id: i64, pr_id: i64, actionable_run_id: i64) -> Result<(), String> {
        let now = now_millis();
        let current_head: String = self
            .conn
            .query_row(
                "SELECT head_sha FROM review_runs WHERE id = $1",
                params![run_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        self.conn
            .execute(
                "UPDATE review_runs
             SET status = 'preview',
                 reviewed_head_sha = COALESCE(reviewed_head_sha, head_sha),
                 head_sha = $1,
                 updated_at = $2
             WHERE id = $3 AND pr_id = $4 AND id != $5
               AND status IN ('preview', 'superseded')",
                params![current_head, now, actionable_run_id, pr_id, run_id],
            )
            .map_err(|error| error.to_string())?;
        if self.conn.changes() != 1 {
            return Err("prior findings review is no longer available".to_string());
        }
        self.conn
            .execute(
                "UPDATE review_runs SET status = 'superseded',
                    outcome = 'No new PR-owned changes; prior findings still await action',
                    updated_at = $1 WHERE id = $2",
                params![now, run_id],
            )
            .map_err(|error| error.to_string())?;
        self.conn
            .execute(
                "UPDATE pull_requests SET review_lane = 'ready',
                    status_reason = 'Prior findings still await action', updated_at = $1
             WHERE id = $2",
                params![now, pr_id],
            )
            .map_err(|error| error.to_string())?;
        self.upsert_pending_action_for_run(
            "review_ready",
            pr_id,
            actionable_run_id,
            "Review ready",
            "Prior findings still await action.",
        )?;
        Ok(())
    }

    pub fn mark_review_superseded_after_post(&self, run_id: i64, pr_id: i64, reason: &str) -> Result<(), String> {
        let now = now_millis();
        self.conn
            .execute(
                "UPDATE review_runs SET status = 'superseded', outcome = $1, updated_at = $2
             WHERE id = $3",
                params![reason, now, run_id],
            )
            .map_err(|error| error.to_string())?;
        self.conn
            .execute(
                "UPDATE pull_requests SET review_lane = 'open', status_reason = $1,
                    last_decision_sha = NULL, updated_at = $2 WHERE id = $3",
                params![reason, now, pr_id],
            )
            .map_err(|error| error.to_string())?;
        self.resolve_pending_for_pr("review_ready", pr_id)?;
        Ok(())
    }

    pub fn supersede_review_run_without_changing_pr(&self, run_id: i64, reason: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE review_runs SET status = 'superseded', outcome = $1, updated_at = $2
                 WHERE id = $3",
                params![reason, now_millis(), run_id],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Record a human decision on a review run. The caller decides the resulting
    /// run status / PR lane / reason and whether anything was posted. Bumps the PR's
    /// `last_decision_sha` so the poller won't re-review the same head.
    pub fn finalize_review_decision(
        &self,
        run_id: i64,
        pr_id: i64,
        decision: &str,
        run_status: &str,
        lane: &str,
        reason: &str,
        event: Option<&str>,
        head_sha: &str,
        decided_by: Option<&str>,
    ) -> Result<(), String> {
        let now = now_millis();
        self.conn.execute(
      "UPDATE review_runs SET status = $1, decision = $2, event = $3, outcome = $4, decided_at = $5, updated_at = $5 WHERE id = $6",
      params![run_status, decision, event, reason, now, run_id],
    ).map_err(|error| error.to_string())?;
        self.conn.execute(
      "UPDATE pull_requests SET review_lane = $1, status_reason = $2, last_decision_sha = $3, decided_by = $4, updated_at = $5 WHERE id = $6",
      params![lane, reason, head_sha, decided_by, now, pr_id],
    ).map_err(|error| error.to_string())?;
        self.resolve_pending_for_pr("review_ready", pr_id)?;
        self.resolve_pending_for_pr("manual_review_needed", pr_id)?;
        Ok(())
    }

    pub fn finalize_review_decision_if_current_head(
        &self,
        run_id: i64,
        pr_id: i64,
        decision: &str,
        run_status: &str,
        lane: &str,
        reason: &str,
        event: Option<&str>,
        head_sha: &str,
        decided_by: Option<&str>,
    ) -> Result<bool, String> {
        let now = now_millis();
        let tx = if self.conn.is_autocommit() {
            Some(self.conn.unchecked_transaction().map_err(|error| error.to_string())?)
        } else {
            None
        };
        self.lock_pull_request(pr_id)?;
        let changed = self.conn.execute(
      "UPDATE pull_requests SET review_lane = $1, status_reason = $2, last_decision_sha = $3, decided_by = $4, updated_at = $5
       WHERE id = $6 AND head_sha = $3 AND force_full_next_review = 0 AND reprocess_next_review = 0
         AND EXISTS (SELECT 1 FROM review_runs rr WHERE rr.id = $7 AND rr.pr_id = $6 AND rr.status != 'superseded' AND rr.review_scope_revision = pull_requests.review_scope_revision AND (rr.stacked_base_ref IS NULL OR rr.stacked_base_ref IS NOT DISTINCT FROM pull_requests.base_ref))",
      params![lane, reason, head_sha, decided_by, now, pr_id, run_id],
    ).map_err(|error| error.to_string())?;
        if changed != 1 {
            return Ok(false);
        }
        self.conn.execute(
      "UPDATE review_runs SET status = $1, decision = $2, event = $3, outcome = $4, decided_at = $5, updated_at = $5 WHERE id = $6",
      params![run_status, decision, event, reason, now, run_id],
    ).map_err(|error| error.to_string())?;
        self.resolve_pending_for_pr("review_ready", pr_id)?;
        self.resolve_pending_for_pr("manual_review_needed", pr_id)?;
        if let Some(tx) = tx {
            tx.commit().map_err(|error| error.to_string())?;
        }
        Ok(true)
    }

    pub fn finalize_review_decision_or_supersede_if_stale(
        &self,
        run_id: i64,
        pr_id: i64,
        decision: &str,
        run_status: &str,
        lane: &str,
        reason: &str,
        event: Option<&str>,
        head_sha: &str,
        decided_by: Option<&str>,
        stale_reason: &str,
    ) -> Result<bool, String> {
        if self.finalize_review_decision_if_current_head(
            run_id, pr_id, decision, run_status, lane, reason, event, head_sha, decided_by,
        )? {
            return Ok(true);
        }
        self.supersede_review_run_without_changing_pr(run_id, stale_reason)?;
        Ok(false)
    }

    /// New commits arrived → any still-pending preview review is stale.
    pub fn supersede_review_runs(&self, pr_id: i64) -> Result<(), String> {
        let now = now_millis();
        self.conn
            .execute(
                "UPDATE review_runs SET status = 'superseded', outcome = 'Superseded by new commits', updated_at = $1
       WHERE pr_id = $2 AND status = 'preview'",
                params![now, pr_id],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}
