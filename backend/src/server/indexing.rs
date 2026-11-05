use super::*;

#[cfg(test)]
pub(crate) fn index_matches_remote(status: &crate::db::RepoIndexStatus, branch: &str, remote_sha: &str) -> bool {
    status.status == "ready"
        && status.spec_version == crate::db::INDEX_SPEC_VERSION
        && status.base_sha.as_deref() == Some(remote_sha)
        && status.base_ref.as_deref() == Some(format!("origin/{branch}").as_str())
}

#[cfg(test)]
pub(crate) fn prepare_review_index(
    state: &SharedState,
    repo: &Repo,
    run_id: i64,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    let path = state.config.repos_dir.join(&repo.owner).join(&repo.name);
    let started = Instant::now();
    let mut joined = false;
    let mut requested = false;
    let mut remote_sha = None;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        if started.elapsed() > Duration::from_secs(1800) {
            return Err("Timed out waiting for the repository index. Check Indexing and retry.".to_string());
        }
        let indexing = state
            .index_jobs
            .lock()
            .map_err(|_| "index job lock poisoned")?
            .contains_key(&repo.id);
        let status = with_db_raw(state, |db| db.repo_index_status(repo.id))?;
        if indexing {
            joined = true;
        }
        let detail = if indexing {
            state
                .index_progress
                .lock()
                .ok()
                .and_then(|progress| progress.get(&repo.id).cloned())
                .map(|progress| {
                    format!(
                        "{} · {}/{} · {}%{}",
                        progress.stage,
                        progress.processed,
                        progress.total,
                        progress.overall_percent,
                        progress
                            .current_path
                            .map(|path| format!(" · {path}"))
                            .unwrap_or_default()
                    )
                })
                .unwrap_or_else(|| "Preparing repository index…".to_string())
        } else {
            "Checking repository index freshness…".to_string()
        };
        if !with_db_raw(state, |db| {
            db.update_review_run_progress(run_id, &review::index_phase_json("running"), &detail)
        })? {
            return Err("cancelled".to_string());
        }
        if !indexing {
            if joined && matches!(status.status.as_str(), "failed" | "cancelled") {
                return Err(status
                    .error_message
                    .unwrap_or_else(|| "Repository indexing failed. Retry the review.".to_string()));
            }
            let current_repo = with_db_raw(state, |db| db.repo_by_id(repo.id))?;
            if path.join(".git").is_dir() {
                if let Some(branch) = current_repo.default_branch.as_deref() {
                    if remote_sha.is_none() {
                        let bytes = indexer::git(
                            &path,
                            &["ls-remote", "origin", &format!("refs/heads/{branch}")],
                            Some(cancel),
                        )?;
                        remote_sha = String::from_utf8_lossy(&bytes)
                            .split_whitespace()
                            .next()
                            .map(str::to_string);
                        if remote_sha.is_none() {
                            return Err("Remote default branch could not be resolved".to_string());
                        }
                    }
                    if index_matches_remote(&status, branch, remote_sha.as_deref().unwrap_or_default()) {
                        let detail = format!(
                            "{} · {} · {} files · {} symbols",
                            if joined { "Index updated" } else { "Already up to date" },
                            status.base_sha.as_deref().unwrap_or_default(),
                            status.file_count,
                            status.symbol_count
                        );
                        if !with_db_raw(state, |db| {
                            db.update_review_run_progress(run_id, &review::index_phase_json("ok"), &detail)
                        })? {
                            return Err("cancelled".to_string());
                        }
                        return Ok(path);
                    }
                }
            }
            if requested && joined {
                return Err("Repository changed while indexing. Retry to use the latest index.".to_string());
            }
            let next = start_index(state, repo.id)?;
            requested = true;
            if next.get("status").and_then(|value| value.as_str()) == Some("indexing") {
                joined = true;
                remote_sha = None;
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Not a failure: the repository has no commits yet, so there is nothing to index.
const EMPTY_REPOSITORY: &str = "empty repository";

pub(crate) fn start_index(state: &SharedState, repo_id: i64) -> Result<serde_json::Value, String> {
    let mut jobs = state
        .index_jobs
        .lock()
        .map_err(|_| "index job lock poisoned".to_string())?;
    if jobs.contains_key(&repo_id) {
        return with_db_read(state, |db| db.repo_index_status(repo_id));
    }
    if jobs.len() >= 2 {
        return serde_json::to_value(with_db_raw(state, |db| db.enqueue_repo_index(repo_id))?)
            .map_err(|error| error.to_string());
    }
    let repo = with_db_raw(state, |db| db.repo_by_id(repo_id))?;
    let Some(status) = with_db_raw(state, |db| db.claim_repo_index(repo_id, repo.default_branch.as_deref()))? else {
        // Another instance is indexing it.
        return with_db_read(state, |db| db.repo_index_status(repo_id));
    };
    let cancel = Arc::new(AtomicBool::new(false));
    jobs.insert(repo_id, cancel.clone());
    drop(jobs);
    let state = state.clone();
    thread::spawn(move || {
        let prepared = (|| {
            let repo_path = ensure_repo_checkout_cancellable(&state.config.repos_dir, &repo, &cancel)?;
            // An empty GitHub repository reports no default branch, or an empty one. Fetch first,
            // in case the first push landed after the clone.
            let repo = if repo.default_branch.as_deref().is_none_or(str::is_empty) {
                indexer::git(&repo_path, &["fetch", "origin", "--quiet"], Some(&cancel))?;
                let _ = indexer::git(&repo_path, &["remote", "set-head", "origin", "--auto"], Some(&cancel));
                let branch = local::default_branch(&repo_path).ok_or(EMPTY_REPOSITORY)?;
                with_db_raw(&state, |db| db.save_repo_default_branch(repo_id, &branch))?
            } else {
                repo
            };
            Ok::<_, String>((repo, repo_path))
        })();
        match prepared {
            Ok((repo, path)) => run_index_job(state, repo_id, repo, path, cancel),
            Err(error) => {
                let _ = with_db_raw(&state, |db| {
                    if cancel.load(Ordering::Relaxed) {
                        db.cancel_repo_index(repo_id)
                    } else if error == EMPTY_REPOSITORY {
                        db.mark_repo_index_empty(repo_id)
                    } else {
                        db.fail_repo_index(repo_id, &error)
                    }
                });
                if let Ok(mut jobs) = state.index_jobs.lock() {
                    jobs.remove(&repo_id);
                }
            }
        }
    });
    serde_json::to_value(status).map_err(|error| error.to_string())
}

pub(crate) fn run_index_job(state: SharedState, repo_id: i64, repo: Repo, repo_path: PathBuf, cancel: Arc<AtomicBool>) {
    let started = std::time::Instant::now();
    let mut last_emit = std::time::Instant::now() - Duration::from_secs(1);
    let mut last_stage = String::new();
    let mut stage_started = std::time::Instant::now();
    let db_handle = state.config.db.clone();
    let progress_map = state.index_progress.clone();
    let mut last_recorded = std::time::Instant::now() - Duration::from_secs(1);
    let mut on_progress = |progress: indexer::IndexProgress| {
        let now = std::time::Instant::now();
        let stage_changed = progress.stage != last_stage;
        if stage_changed {
            last_stage = progress.stage.clone();
            stage_started = now;
            if let Ok(db) = db_handle.open() {
                let _ = db.update_repo_index_stage(
                    repo_id,
                    &progress.stage,
                    progress.file_count,
                    progress.total,
                    progress.package_count,
                    progress.edge_count,
                    progress.skipped_count,
                );
            }
        }
        if !stage_changed && now.duration_since(last_emit) < Duration::from_millis(250) {
            return;
        }
        last_emit = now;
        let stage_elapsed = now.duration_since(stage_started).as_secs_f64().max(0.001);
        let files_per_sec = if progress.processed > 0 {
            progress.processed as f64 / stage_elapsed
        } else {
            0.0
        };
        let eta_ms =
            if progress.stage == "parsing_imports" && files_per_sec > 0.0 && progress.total > progress.processed {
                Some((((progress.total - progress.processed) as f64 / files_per_sec) * 1000.0) as i64)
            } else {
                None
            };
        let event = IndexProgressEvent {
            repo_id,
            stage: progress.stage,
            processed: progress.processed,
            total: progress.total,
            overall_percent: overall_percent(&last_stage, progress.processed, progress.total),
            file_count: progress.file_count,
            package_count: progress.package_count,
            edge_count: progress.edge_count,
            skipped_count: progress.skipped_count,
            files_per_sec,
            eta_ms,
            current_path: progress.current_path,
            updated_at: crate::db::now_millis(),
        };
        // Other instances answer progress requests from the database; a write a second is enough.
        if stage_changed || now.duration_since(last_recorded) >= Duration::from_secs(1) {
            last_recorded = now;
            if let (Ok(json), Ok(db)) = (serde_json::to_string(&event), db_handle.open()) {
                let _ = db.record_repo_index_progress(repo_id, &json);
            }
        }
        if let Ok(mut map) = progress_map.lock() {
            map.insert(repo_id, event);
        }
    };

    let result = match indexer::run_index(&repo_path, repo.default_branch.as_deref(), &cancel, &mut on_progress) {
        Ok(mut write) => {
            write.duration_ms = started.elapsed().as_millis() as i64;
            state
                .config
                .db
                .open()
                .and_then(|db| db.finish_repo_index(repo_id, &write))
        }
        Err(error) if error == "cancelled" => state.config.db.open().and_then(|db| db.cancel_repo_index(repo_id)),
        Err(error) => state
            .config
            .db
            .open()
            .and_then(|db| db.fail_repo_index(repo_id, &error)),
    };
    if let Err(error) = result {
        let _ = with_db_raw(&state, |db| db.fail_repo_index(repo_id, &error));
        eprintln!("index failed for {}: {error}", repo.full_name);
    }
    if let Ok(mut jobs) = state.index_jobs.lock() {
        jobs.remove(&repo_id);
    }
    if let Ok(mut progress) = state.index_progress.lock() {
        progress.remove(&repo_id);
    }
}

pub(crate) fn index_progress(state: &SharedState, repo_id: i64) -> Result<serde_json::Value, String> {
    let value = state
        .index_progress
        .lock()
        .map_err(|_| "index progress lock poisoned".to_string())?
        .get(&repo_id)
        .cloned();
    if value.is_some() {
        return serde_json::to_value(value).map_err(|error| error.to_string());
    }
    // An index another instance runs.
    match with_db_read_raw(state, |db| db.repo_index_progress(repo_id))? {
        Some(json) => serde_json::from_str(&json).map_err(|error| error.to_string()),
        None => Ok(serde_json::Value::Null),
    }
}

pub(crate) fn check_freshness(state: &SharedState, repo_id: i64) -> Result<serde_json::Value, String> {
    let repo = with_db_raw(state, |db| db.repo_by_id(repo_id))?;
    let status = with_db_raw(state, |db| db.repo_index_status(repo_id))?;
    if status.base_sha.is_none() || matches!(status.status.as_str(), "not_indexed" | "failed") {
        return serde_json::to_value(status).map_err(|error| error.to_string());
    }
    let repo_path = ensure_repo_checkout(&state.config.repos_dir, &repo)?;
    let remote_sha = indexer::remote_default_sha(&repo_path, repo.default_branch.as_deref())?;
    let next = if status.base_sha.as_deref() == Some(remote_sha.as_str()) {
        status
    } else {
        with_db_raw(state, |db| db.mark_repo_index_stale(repo_id, &remote_sha))?
    };
    serde_json::to_value(next).map_err(|error| error.to_string())
}

/// Whether `repo_id`'s default branch is due another look on GitHub; records the look when it is.
fn due_for_remote_check(repo_id: i64) -> bool {
    static CHECKED: Mutex<Vec<(i64, Instant)>> = Mutex::new(Vec::new());
    let Ok(mut checked) = CHECKED.lock() else {
        return true;
    };
    match checked.iter_mut().find(|(id, _)| *id == repo_id) {
        Some((_, at)) if at.elapsed() < Duration::from_secs(60) => false,
        Some((_, at)) => {
            *at = Instant::now();
            true
        }
        None => {
            checked.push((repo_id, Instant::now()));
            true
        }
    }
}

pub(crate) fn start_reindex_worker(state: SharedState) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(2));
        // One instance decides what to index at a time; the index job it starts records that
        // instance, and the others see the repository as indexing.
        let _ = state
            .config
            .db
            .run_exclusive(exclusive::REINDEX, || reindex_pass(&state));
    });
}

fn reindex_pass(state: &SharedState) {
    let repos = match with_db_raw(state, |db| db.monitored_repos()) {
        Ok(repos) => repos,
        Err(error) => {
            eprintln!("worker could not load repos: {error}");
            return;
        }
    };
    for repo in repos {
        let should_skip = state
            .index_jobs
            .lock()
            .map(|jobs| jobs.len() >= 2 || jobs.contains_key(&repo.id))
            .unwrap_or(true);
        if should_skip {
            continue;
        }
        let status = match with_db_raw(state, |db| db.repo_index_status(repo.id)) {
            Ok(status) => status,
            Err(error) => {
                eprintln!("worker could not load index status for {}: {error}", repo.full_name);
                continue;
            }
        };
        if matches!(status.status.as_str(), "queued" | "not_indexed" | "stale")
            || (status.status == "ready" && status.spec_version != crate::db::INDEX_SPEC_VERSION)
        {
            let _ = start_index(state, repo.id);
            continue;
        }
        // An empty repository indexes itself after its first push; check for one each minute.
        if status.status == "empty" && crate::db::now_millis() - status.updated_at > 60_000 {
            let pushed = ensure_repo_checkout(&state.config.repos_dir, &repo)
                .and_then(|path| indexer::git(&path, &["ls-remote", "--heads", "origin"], None))
                .is_ok_and(|heads| !heads.is_empty());
            let _ = if pushed {
                start_index(state, repo.id).map(drop)
            } else {
                with_db_raw(state, |db| db.mark_repo_index_empty(repo.id)).map(drop)
            };
            continue;
        }
        // Ask GitHub whether the default branch moved at most once a minute per repository.
        if status.status == "ready" && status.base_sha.is_some() && due_for_remote_check(repo.id) {
            if let Ok(repo_path) = ensure_repo_checkout(&state.config.repos_dir, &repo) {
                if let Ok(remote_sha) = indexer::remote_default_sha(&repo_path, repo.default_branch.as_deref()) {
                    if status.base_sha.as_deref() != Some(remote_sha.as_str()) {
                        let _ = with_db_raw(state, |db| db.mark_repo_index_stale(repo.id, &remote_sha));
                        let _ = start_index(state, repo.id);
                    }
                }
            }
        }
    }
}

pub(crate) fn ensure_repo_checkout(repos_dir: &Path, repo: &Repo) -> Result<PathBuf, String> {
    ensure_repo_checkout_cancellable(repos_dir, repo, &AtomicBool::new(false))
}

pub(crate) fn ensure_repo_checkout_cancellable(
    repos_dir: &Path,
    repo: &Repo,
    cancel: &AtomicBool,
) -> Result<PathBuf, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_string());
    }
    let path = repos_dir.join(&repo.owner).join(&repo.name);
    if path.join(".git").is_dir() {
        return Ok(path);
    }
    // Indexing and reviews can ask for the same new repository at once: one clones, the others
    // wait for it and reuse the checkout.
    static CLONES: Mutex<Vec<(PathBuf, Arc<Mutex<()>>)>> = Mutex::new(Vec::new());
    let lock = {
        let mut clones = CLONES.lock().map_err(|_| "Clone lock poisoned")?;
        clones.retain(|(_, lock)| Arc::strong_count(lock) > 1);
        match clones.iter().find(|(cloning, _)| *cloning == path) {
            Some((_, lock)) => lock.clone(),
            None => {
                let lock = Arc::new(Mutex::new(()));
                clones.push((path.clone(), lock.clone()));
                lock
            }
        }
    };
    let _cloning = lock.lock().map_err(|_| "Clone lock poisoned")?;
    if path.join(".git").is_dir() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    // Clone over HTTPS with the gh token rather than `gh repo clone`, which
    // honours the host's `git_protocol` (often ssh) and would need SSH keys
    // that aren't mounted into the container.
    let url = format!("https://{}/{}.git", crate::gitlab::gitlab_host(), repo.full_name);
    let parent = path.parent().ok_or("Repository parent is missing")?;
    let stamp = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let staging = parent.join(format!(".{}-clone-{}-{stamp}", repo.name, std::process::id()));
    std::fs::create_dir(&staging).map_err(|error| error.to_string())?;
    let result = indexer::git(
        parent,
        &["clone", "--filter=blob:none", &url, &staging.to_string_lossy()],
        Some(cancel),
    );
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&staging, &path) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error.to_string());
    }
    Ok(path)
}
