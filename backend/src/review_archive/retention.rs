use crate::db::{Database, Db};
use crate::review_archive::{ArchiveStore, STORE_QUOTA};
use crate::server::SharedState;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const MS_PER_DAY: i64 = 86_400_000;
/// Where the last maintenance pass leaves the storage status, for whichever instance is asked.
pub const STORAGE_STATUS_SETTING: &str = "archive_storage_status";
pub const MAX_RETENTION_DAYS: i64 = 365;
/// Zero purges a merged pull request's frozen code as soon as its merge is confirmed.
pub const MIN_RETENTION_DAYS: i64 = 0;
pub const DEFAULT_RETENTION_DAYS: i64 = 0;
pub const STORE_QUOTA_BYTES: u64 = STORE_QUOTA;

#[derive(Clone, Default)]
pub struct ArchiveLeaseTracker {
    active_runs: Arc<Mutex<HashSet<i64>>>,
}

pub struct ArchiveRunLease {
    tracker: Arc<Mutex<HashSet<i64>>>,
    run_id: i64,
}

impl Drop for ArchiveRunLease {
    fn drop(&mut self) {
        if let Ok(mut set) = self.tracker.lock() {
            set.remove(&self.run_id);
        }
    }
}

impl ArchiveLeaseTracker {
    pub fn new() -> Self {
        Self {
            active_runs: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn acquire(&self, run_id: i64) -> ArchiveRunLease {
        if let Ok(mut set) = self.active_runs.lock() {
            set.insert(run_id);
        }
        ArchiveRunLease {
            tracker: self.active_runs.clone(),
            run_id,
        }
    }

    pub fn is_active(&self, run_id: i64) -> bool {
        self.active_runs
            .lock()
            .map(|set| set.contains(&run_id))
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveStorageStatus {
    pub quota_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub last_cleanup_at: Option<i64>,
    pub last_packages_removed: usize,
    pub last_bytes_removed: u64,
    pub pending_deferrals: usize,
    pub last_error: Option<String>,
}

impl Default for ArchiveStorageStatus {
    fn default() -> Self {
        Self {
            quota_bytes: STORE_QUOTA_BYTES,
            used_bytes: 0,
            available_bytes: STORE_QUOTA_BYTES,
            last_cleanup_at: None,
            last_packages_removed: 0,
            last_bytes_removed: 0,
            pending_deferrals: 0,
            last_error: None,
        }
    }
}

pub struct RetentionCoordinator {
    pub db: Db,
    pub archive_root: PathBuf,
    pub leases: ArchiveLeaseTracker,
    pub status: Arc<Mutex<ArchiveStorageStatus>>,
    pub notify: Arc<(Mutex<bool>, Condvar)>,
    backoff: Mutex<HashMap<i64, Instant>>,
}

impl RetentionCoordinator {
    pub fn new(
        db: Db,
        archive_root: PathBuf,
        leases: ArchiveLeaseTracker,
        status: Arc<Mutex<ArchiveStorageStatus>>,
        notify: Arc<(Mutex<bool>, Condvar)>,
    ) -> Self {
        Self {
            db,
            archive_root,
            leases,
            status,
            notify,
            backoff: Mutex::new(HashMap::new()),
        }
    }

    pub fn perform_maintenance(&self, cancel: &AtomicBool) -> Result<MaintenanceResult, String> {
        self.perform_maintenance_with_backfill(|store, db| self.backfill_merges(store, db, cancel))
    }

    fn perform_maintenance_with_backfill(
        &self,
        backfill: impl FnOnce(&ArchiveStore, &Database) -> Result<(), String>,
    ) -> Result<MaintenanceResult, String> {
        let result = self.maintenance_pass(backfill);
        match &result {
            Ok(_) => self.publish_status(),
            Err(error) => {
                eprintln!("Archive retention maintenance error: {error}");
                self.publish_failure();
            }
        }
        result
    }

    /// Records the status in the database: the pass runs on one instance, and every instance
    /// answers the status endpoint.
    fn publish_status(&self) {
        let Ok(status) = self.status.lock().map(|status| status.clone()) else {
            return;
        };
        if let (Ok(json), Ok(db)) = (serde_json::to_string(&status), self.db.open()) {
            let _ = db.save_setting_value(STORAGE_STATUS_SETTING, &json);
        }
    }

    /// Marks the recorded status failed and keeps its metrics: another instance's last successful
    /// pass may have recorded them, and this instance's own may be older or empty.
    fn publish_failure(&self) {
        const FAILED: &str = "Archive maintenance failed. Check server logs; previous successful metrics are retained.";
        // Marked locally first, so the failure shows even when the database cannot be reached.
        let Ok(mut status) = self.status.lock().map(|mut local| {
            local.last_error = Some(FAILED.into());
            local.clone()
        }) else {
            return;
        };
        let Ok(db) = self.db.open() else {
            return;
        };
        if let Some(mut recorded) = db
            .setting(STORAGE_STATUS_SETTING)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<ArchiveStorageStatus>(&json).ok())
        {
            recorded.last_error = Some(FAILED.into());
            if let Ok(mut local) = self.status.lock() {
                *local = recorded.clone();
            }
            status = recorded;
        }
        if let Ok(json) = serde_json::to_string(&status) {
            let _ = db.save_setting_value(STORAGE_STATUS_SETTING, &json);
        }
    }

    fn maintenance_pass(
        &self,
        backfill: impl FnOnce(&ArchiveStore, &Database) -> Result<(), String>,
    ) -> Result<MaintenanceResult, String> {
        let mut warnings = Vec::new();
        let now_ms = crate::sandbox::now();
        let db = self.db.open().map_err(|e| format!("Could not open database: {e}"))?;
        let mut store = ArchiveStore::open(&self.archive_root, &self.db)
            .map_err(|e| format!("Could not open archive store: {e:?}"))?;

        // 1. Validate retention setting.
        let raw_setting = db
            .setting("review_archive_retention_days")
            .map_err(|e| format!("Could not read retention setting: {e}"))?;

        let retention_days = match raw_setting {
            Some(value) => match value.parse::<i64>() {
                Ok(days) if (MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days) => days,
                _ => {
                    let err = "Malformed retention days setting; deletion suspended.".to_string();
                    if let Ok(mut st) = self.status.lock() {
                        st.last_error = Some(err.clone());
                    }
                    return Err(err);
                }
            },
            None => DEFAULT_RETENTION_DAYS,
        };

        // 2. Local cleanup pass for already-confirmed merges
        let (removed_packages, removed_bytes, _deferrals) =
            self.cleanup_pass(&mut store, &db, retention_days, now_ms, &mut warnings)?;

        // 3. Backfill merge timestamps from GitHub for PRs with operational archives
        if let Err(backfill_err) = backfill(&store, &db) {
            eprintln!("Merge backfill warning: {backfill_err}");
            warnings.push("Merge timestamps could not all be verified; affected archives were deferred.");
        }

        // 4. Second quick cleanup pass if backfill confirmed new merges
        let (additional_packages, additional_bytes, final_deferrals) =
            self.cleanup_pass(&mut store, &db, retention_days, now_ms, &mut warnings)?;

        let total_packages = removed_packages + additional_packages;
        let total_bytes = removed_bytes + additional_bytes;

        // 5. Compute usage and update storage status
        let (recorded, actual) = store
            .storage_usage()
            .map_err(|e| format!("Could not measure storage usage: {e:?}"))?;
        let used_bytes = recorded.max(actual);
        let available_bytes = STORE_QUOTA_BYTES.saturating_sub(used_bytes);

        let mut st = self.status.lock().map_err(|_| "Status lock poisoned")?;
        st.quota_bytes = STORE_QUOTA_BYTES;
        st.used_bytes = used_bytes;
        st.available_bytes = available_bytes;
        st.last_cleanup_at = Some(now_ms);
        st.last_packages_removed = total_packages;
        st.last_bytes_removed = total_bytes;
        st.pending_deferrals = final_deferrals;
        warnings.sort_unstable();
        warnings.dedup();
        st.last_error = (!warnings.is_empty()).then(|| warnings.join(" "));

        Ok(MaintenanceResult {
            packages_removed: total_packages,
            bytes_removed: total_bytes,
            deferrals: final_deferrals,
            used_bytes,
            available_bytes,
        })
    }

    fn cleanup_pass(
        &self,
        store: &mut ArchiveStore,
        db: &Database,
        retention_days: i64,
        now_ms: i64,
        warnings: &mut Vec<&'static str>,
    ) -> Result<(usize, u64, usize), String> {
        let retention_ms = retention_days
            .checked_mul(MS_PER_DAY)
            .ok_or_else(|| "Retention duration overflow".to_string())?;
        let cutoff_ms = now_ms.saturating_sub(retention_ms);

        let artifacts = store
            .list_artifacts()
            .map_err(|e| format!("Could not list artifacts: {e:?}"))?;

        let mut removed_packages = 0;
        let mut removed_bytes = 0;
        let mut deferrals = 0;

        for (id, _bytes) in artifacts {
            // Check active upload
            if store
                .has_active_upload_for_artifact(&id)
                .map_err(|e| format!("Upload check failed: {e:?}"))?
            {
                deferrals += 1;
                continue;
            }

            let receipts = store
                .receipts_for_artifact(&id)
                .map_err(|e| format!("Receipts check failed: {e:?}"))?;

            // Unresolved ownership
            if receipts.is_empty() {
                deferrals += 1;
                continue;
            }

            let active_runs = self.leases.active_runs.lock().map_err(|_| "Lease lock poisoned")?;
            let mut all_owners_eligible = true;
            for owner in &receipts {
                let Some(run_id_str) = owner.strip_prefix("review:") else {
                    all_owners_eligible = false;
                    break;
                };
                let Ok(run_id) = run_id_str.parse::<i64>() else {
                    all_owners_eligible = false;
                    break;
                };

                // Check in-flight lease
                if active_runs.contains(&run_id) {
                    all_owners_eligible = false;
                    break;
                }

                // Check DB for run and PR
                match self.check_run_and_pr_eligibility(db, run_id, cutoff_ms) {
                    Ok(true) => {}
                    Err(error) => {
                        eprintln!("Archive eligibility check failed: {error}");
                        warnings.push("Some archive owners could not be checked; affected archives were deferred.");
                        all_owners_eligible = false;
                        break;
                    }
                    Ok(false) => {
                        all_owners_eligible = false;
                        break;
                    }
                }
            }

            if all_owners_eligible {
                match store.delete_artifact(&id, &receipts) {
                    Ok(None) => {
                        deferrals += 1;
                    }
                    Ok(Some(freed)) => {
                        removed_packages += 1;
                        removed_bytes += freed;
                    }
                    Err(e) => {
                        eprintln!("Failed to delete artifact {id}: {e:?}");
                        warnings.push("Some archives could not be deleted. Check server logs.");
                        deferrals += 1;
                    }
                }
            } else {
                deferrals += 1;
            }
        }

        Ok((removed_packages, removed_bytes, deferrals))
    }

    fn check_run_and_pr_eligibility(&self, db: &Database, run_id: i64, cutoff_ms: i64) -> Result<bool, String> {
        let Some((status, pr_id)) = db.review_run_retention_info(run_id)? else {
            return Ok(false); // Absent run
        };

        // If run is still in-flight / non-terminal, preserve
        if matches!(
            status.to_ascii_lowercase().as_str(),
            "pending" | "running" | "posting" | "preview"
        ) {
            return Ok(false);
        }

        let Some((state, merged_at)) = db.pr_retention_info(pr_id)? else {
            return Ok(false);
        };

        let Some(merged_at_ms) = merged_at else {
            return Ok(false); // Merge not confirmed
        };

        // Must be merged and merged_at <= cutoff_ms (inclusive comparison)
        if state.eq_ignore_ascii_case("merged") && merged_at_ms <= cutoff_ms {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn backfill_merges(&self, store: &ArchiveStore, db: &Database, cancel: &AtomicBool) -> Result<(), String> {
        let run_ids = store
            .distinct_review_receipt_run_ids()
            .map_err(|e| format!("Could not get distinct receipt run IDs: {e:?}"))?;

        if run_ids.is_empty() {
            return Ok(());
        }

        let active_candidates = self.next_backfill_candidates(db, &run_ids)?;

        let mut failed = !self.backoff.lock().map_err(|_| "Backoff lock poisoned")?.is_empty();
        // Process in batches of up to 25
        for batch in active_candidates.chunks(25) {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("Merge backfill cancelled".into());
            }
            if let Err(e) = self.query_gitlab_batch(batch, db, cancel) {
                failed = true;
                eprintln!("Batch GitLab merge query error: {e}");
                // Apply backoff to this batch
                if let Ok(mut b) = self.backoff.lock() {
                    let backoff_until = Instant::now() + Duration::from_secs(300);
                    for (pr_id, _, _) in batch {
                        b.insert(*pr_id, backoff_until);
                    }
                }
            }
        }

        if failed {
            Err("Some merge checks failed or remain on backoff".into())
        } else {
            Ok(())
        }
    }

    fn next_backfill_candidates(&self, db: &Database, run_ids: &[i64]) -> Result<Vec<(i64, String, i64)>, String> {
        let mut candidates = db.find_candidate_prs_for_merge_backfill(run_ids)?;
        candidates.sort_by_key(|candidate| candidate.0);
        candidates.dedup_by_key(|candidate| candidate.0);
        let cursor = db
            .setting("review_archive_backfill_cursor")?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        let split = candidates.partition_point(|candidate| candidate.0 <= cursor);
        candidates.rotate_left(split);
        let now = Instant::now();
        let mut backoff = self.backoff.lock().map_err(|_| "Backoff lock poisoned")?;
        backoff.retain(|_, until| *until > now);
        let selected: Vec<_> = candidates
            .into_iter()
            .filter(|candidate| !backoff.contains_key(&candidate.0))
            .take(200)
            .collect();
        drop(backoff);
        if let Some(last) = selected.last() {
            db.save_setting_value("review_archive_backfill_cursor", &last.0.to_string())?;
        }
        Ok(selected)
    }

    fn query_gitlab_batch(
        &self,
        batch: &[(i64, String, i64)],
        db: &Database,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let mut incomplete = false;
        for (pr_id, full_name, number) in batch {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("Merge backfill cancelled".into());
            }
            let project = crate::gitlab::project_api_id(full_name);
            let route = format!("projects/{project}/merge_requests/{number}");
            let response_bytes = match crate::gitlab::glab_capture_cancellable(&["api", &route], cancel) {
                Ok(bytes) => bytes,
                Err(_) => {
                    incomplete = true;
                    if let Ok(mut b) = self.backoff.lock() {
                        b.insert(*pr_id, Instant::now() + Duration::from_secs(600));
                    }
                    continue;
                }
            };
            let mr: serde_json::Value =
                serde_json::from_slice(&response_bytes).map_err(|e| format!("Invalid JSON from GitLab API: {e}"))?;
            let state = mr.get("state").and_then(|v| v.as_str()).unwrap_or("");
            match state {
                "merged" => {
                    let merged_at = mr
                        .get("merged_at")
                        .and_then(|v| v.as_str())
                        .and_then(crate::gitlab::iso8601_to_epoch_ms)
                        .ok_or("GitLab returned a merged MR without a valid merge timestamp")?;
                    db.set_pr_merged_at(*pr_id, merged_at)?;
                }
                "closed" => {
                    db.mark_pr_terminal(*pr_id, "closed", "MR closed on GitLab")?;
                }
                "opened" | "locked" => {}
                _ => {
                    incomplete = true;
                }
            }
        }

        if incomplete {
            Err("GitLab returned incomplete merge information".into())
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone)]
pub struct MaintenanceResult {
    pub packages_removed: usize,
    pub bytes_removed: u64,
    pub deferrals: usize,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

fn wait_for_maintenance(notify: &(Mutex<bool>, Condvar), interval: Duration) -> Result<(), String> {
    let (lock, cvar) = notify;
    let wake = lock.lock().map_err(|_| "Retention notification lock poisoned")?;
    let (mut wake, _) = cvar
        .wait_timeout_while(wake, interval, |wake| !*wake)
        .map_err(|_| "Retention notification lock poisoned")?;
    *wake = false;
    Ok(())
}

pub(crate) fn start_archive_retention_worker(state: SharedState) {
    let coordinator = Arc::new(RetentionCoordinator::new(
        state.config.db.clone(),
        state.config.archive_dir(),
        state.archive_leases.clone(),
        state.archive_storage_status.clone(),
        state.retention_worker_notify.clone(),
    ));

    std::thread::Builder::new()
        .name("archive-retention".into())
        .spawn(move || {
            let cancel = AtomicBool::new(false);
            loop {
                // Several instances may share the archive; one maintains it at a time. When another
                // instance is mid-pass (or the lock is unreachable), try again soon rather than in an
                // hour, so a requested pass (a retention change) still happens.
                let ran = match coordinator.db.run_exclusive(crate::server::exclusive::RETENTION, || {
                    coordinator.perform_maintenance(&cancel)
                }) {
                    Ok(ran) => ran.is_some(),
                    Err(error) => {
                        eprintln!("Archive retention could not take its turn: {error}");
                        false
                    }
                };
                let interval = Duration::from_secs(if ran { 3600 } else { 60 });
                if wait_for_maintenance(&coordinator.notify, interval).is_err() {
                    break;
                }
            }
        })
        .expect("start archive retention thread");
}

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;
