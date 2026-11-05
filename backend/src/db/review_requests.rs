use super::*;

pub(crate) struct ReviewRequestRestoration {
    pub run_id: i64,
    pub full_name: String,
    pub number: i64,
    pub snapshot: crate::gitlab::ReviewRequestSnapshot,
    pub marker: String,
    pub review_id: Option<i64>,
    pub attempts: i64,
}

impl Database {
    pub(crate) fn prepare_review_request_restoration(
        &self,
        run_id: i64,
        snapshot: &crate::gitlab::ReviewRequestSnapshot,
        marker: &str,
    ) -> Result<(), String> {
        let json = serde_json::to_string(snapshot).map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "INSERT INTO review_request_restorations(run_id,snapshot_json,marker,next_attempt_at)
             VALUES ($1,$2,$3,$4)
             ON CONFLICT(run_id) DO UPDATE SET snapshot_json=excluded.snapshot_json,marker=excluded.marker,
                status='pending',attempts=0,next_attempt_at=excluded.next_attempt_at,last_error=NULL
             WHERE review_request_restorations.review_id IS NULL AND review_request_restorations.lease_until=0",
                params![run_id, json, marker, now_millis() + 300_000],
            )
            .map_err(|e| e.to_string())?;
        if self.conn.changes() != 1 {
            return Err("A previous review publication is still being reconciled".into());
        }
        Ok(())
    }

    pub(crate) fn accept_review_request_restoration(&self, run_id: i64, review_id: i64) -> Result<(), String> {
        self.conn.execute(
            "UPDATE review_request_restorations SET review_id=$2,next_attempt_at=$3,status=CASE WHEN review_id IS NULL THEN 'pending' ELSE status END
             WHERE run_id=$1 AND (review_id IS NULL OR review_id=$2)",
            params![run_id,review_id,now_millis()],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// One background worker owns a job across its bounded GitHub calls. Expired
    /// leases recover crashes without coupling retries to review publication.
    pub(crate) fn claim_review_request_restoration(&self) -> Result<Option<ReviewRequestRestoration>, String> {
        let now = now_millis();
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let job = self
            .conn
            .query_row(
                "SELECT r.run_id,repos.full_name,pr.number,r.snapshot_json,r.marker,r.review_id,r.attempts
             FROM review_request_restorations r JOIN review_runs rr ON rr.id=r.run_id
             JOIN pull_requests pr ON pr.id=rr.pr_id JOIN repos ON repos.id=pr.repo_id
             WHERE r.status='pending' AND r.next_attempt_at<=$1 AND r.lease_until<=$1
             ORDER BY r.next_attempt_at,r.run_id LIMIT 1
             FOR UPDATE OF r SKIP LOCKED",
                [now],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((run_id, full_name, number, json, marker, review_id, attempts)) = job else {
            return Ok(None);
        };
        self.conn
            .execute(
                "UPDATE review_request_restorations SET lease_until=$2,attempts=attempts+1 WHERE run_id=$1 AND lease_until<=$3",
                params![run_id, now + 3_600_000, now],
            )
            .map_err(|e| e.to_string())?;
        let snapshot = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(Some(ReviewRequestRestoration {
            run_id,
            full_name,
            number,
            snapshot,
            marker,
            review_id,
            attempts: attempts + 1,
        }))
    }

    pub(crate) fn finish_review_request_restoration(
        &self,
        run_id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        self.conn.execute(
            "UPDATE review_request_restorations SET status=$2,last_error=$3,lease_until=0,next_attempt_at=$4 WHERE run_id=$1",
            params![run_id,status,error,now_millis()+60_000],
        ).map_err(|e| e.to_string())?;
        if matches!(status, "failed" | "changed") {
            let pr_id = self
                .conn
                .query_row("SELECT pr_id FROM review_runs WHERE id=$1", [run_id], |row| row.get(0))
                .map_err(|e| e.to_string())?;
            self.upsert_pending_action_for_run(
                "review_requests_restore_failed",
                pr_id,
                run_id,
                "Check human review requests",
                error.unwrap_or("Review posted; check the requested reviewers on GitHub."),
            )?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Database, i64, crate::gitlab::ReviewRequestSnapshot) {
        let db = Db::for_test().open().unwrap();
        db.ensure_temp_repo_for_index(1, "acme/api").unwrap();
        db.conn.execute("INSERT INTO pull_requests (id,repo_id,number,title,author,head_sha,last_synced_at,created_at,updated_at) VALUES (1,1,1,'PR','author','abc',1,1,1)", []).unwrap();
        let run_id = db.start_review_skeleton_with(1, None).unwrap();
        let snapshot = serde_json::from_value(serde_json::json!({
            "login":"reviewer","requested":{"reviewers":["reviewer"],"team_reviewers":["backend"]},"events":["request"]
        }))
        .unwrap();
        (db, run_id, snapshot)
    }

    #[test]
    fn review_requests_recovery_is_durable_exclusive_and_does_not_change_review_status() {
        let (db, run, snapshot) = fixture();
        db.prepare_review_request_restoration(run, &snapshot, "marker").unwrap();
        assert!(db.claim_review_request_restoration().unwrap().is_none());
        db.accept_review_request_restoration(run, 42).unwrap();
        db.conn
            .execute("UPDATE review_runs SET status='posted' WHERE id=$1", [run])
            .unwrap();
        let job = db.claim_review_request_restoration().unwrap().unwrap();
        assert_eq!(job.review_id, Some(42));
        assert_eq!(job.attempts, 1);
        assert_eq!(job.snapshot.login, "reviewer");
        assert!(db.claim_review_request_restoration().unwrap().is_none());
        assert!(db
            .prepare_review_request_restoration(run, &snapshot, "another")
            .is_err());
        db.finish_review_request_restoration(run, "pending", Some("offline"))
            .unwrap();
        assert!(db.claim_review_request_restoration().unwrap().is_none());
        db.conn
            .execute("UPDATE review_request_restorations SET next_attempt_at=0", [])
            .unwrap();
        assert_eq!(db.claim_review_request_restoration().unwrap().unwrap().attempts, 2);
        db.finish_review_request_restoration(run, "done", None).unwrap();
        assert!(db.claim_review_request_restoration().unwrap().is_none());
        assert_eq!(db.review_run_by_id(run).unwrap().unwrap().status, "posted");
    }

    #[test]
    fn review_requests_expired_lease_recovers_and_failure_creates_attention() {
        let (db, run, snapshot) = fixture();
        db.prepare_review_request_restoration(run, &snapshot, "marker").unwrap();
        db.accept_review_request_restoration(run, 42).unwrap();
        db.claim_review_request_restoration().unwrap().unwrap();
        db.conn
            .execute("UPDATE review_request_restorations SET lease_until=1", [])
            .unwrap();
        assert_eq!(
            db.claim_review_request_restoration().unwrap().unwrap().review_id,
            Some(42)
        );
        db.finish_review_request_restoration(run, "failed", Some("Check GitHub requests"))
            .unwrap();
        assert!(db.claim_review_request_restoration().unwrap().is_none());
        let pending: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pending_actions WHERE type='review_requests_restore_failed' AND review_run_id=$1",
                [run],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
    }

    #[test]
    fn review_requests_ambiguous_publication_retains_marker_for_recovery() {
        let (db, run, snapshot) = fixture();
        db.prepare_review_request_restoration(run, &snapshot, "publication-marker")
            .unwrap();
        db.conn
            .execute("UPDATE review_request_restorations SET next_attempt_at=0", [])
            .unwrap();
        let job = db.claim_review_request_restoration().unwrap().unwrap();
        assert_eq!(job.marker, "publication-marker");
        assert_eq!(job.review_id, None);
        db.accept_review_request_restoration(run, 42).unwrap();
        db.finish_review_request_restoration(run, "done", None).unwrap();
        // Startup publication reconciliation cannot reopen completed restoration.
        db.accept_review_request_restoration(run, 42).unwrap();
        assert!(db.claim_review_request_restoration().unwrap().is_none());
    }
}
