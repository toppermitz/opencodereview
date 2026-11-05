use super::*;
use crate::review_archive::{Archive, ArchiveStore, Scope};
use crate::sandbox::runner;
use std::sync::{Arc, Mutex, OnceLock};

pub struct FrozenContext {
    pub archive: Archive,
    targets: OnceLock<Vec<(PathBuf, String)>>,
    root: OnceLock<PathBuf>,
}

impl FrozenContext {
    pub fn new(archive: Archive) -> Self {
        Self {
            archive,
            targets: OnceLock::new(),
            root: OnceLock::new(),
        }
    }

    #[cfg(test)]
    pub(super) fn diagnostic(archive: Archive, root: PathBuf, targets: Vec<(PathBuf, String)>) -> Self {
        let context = Self::new(archive);
        context.root.set(root).unwrap();
        context.targets.set(targets).unwrap();
        context
    }
}

/// Host recorded on a frozen archive. Live reviews require `OCR_GITLAB_HOST`.
/// Offline public-benchmark archives keep a stable `github.com` source host when
/// no GitLab connection is configured, matching the datasets they mount.
fn archive_source_host(offline: bool) -> Result<String, String> {
    let configured = gitlab::gitlab_host();
    if !configured.is_empty() {
        return Ok(configured);
    }
    if offline {
        return Ok("github.com".into());
    }
    Err("OCR_GITLAB_HOST is required (bare host or host:port for your GitLab Self-Hosted instance)".into())
}

pub fn capture(
    inputs: &ReviewInputs,
    plan: &ReviewContextPlan,
    head: &str,
    cancel: &AtomicBool,
) -> Result<Arc<FrozenContext>, String> {
    let host = archive_source_host(inputs.offline)?;
    let scope = Scope {
        host,
        repository: inputs.full_name.clone(),
        head_sha: head.into(),
        base_sha: plan.reviewed_base_sha.clone().ok_or("Pinned base unavailable")?,
        merge_base_sha: plan.merge_base_sha.clone().ok_or("Pinned merge-base unavailable")?,
        prior_sha: plan.prior_sha.clone().filter(|_| plan.incremental),
    };
    crate::review_archive::hydrate(&inputs.repo_path, &scope, cancel).map_err(|e| e.to_string())?;
    // A repository larger than the archive's budgets keeps what the review is about first.
    let priority = plan
        .changed_files
        .iter()
        .chain(plan.related_files.iter().map(|file| &file.path))
        .chain(&plan.test_files)
        .chain(&plan.convention_files)
        .cloned()
        .collect();
    let archive = crate::review_archive::capture_prioritized(&inputs.repo_path, scope, &priority, cancel)
        .map_err(|e| e.to_string())?;
    // An offline review has the checkout and nothing else: no submodules and no linked
    // repositories to pin, so its archive is the checkout's own.
    let archive = if inputs.offline {
        archive
    } else {
        crate::review::dependencies::capture(
            archive,
            &inputs.repo_path,
            &plan.changed_files,
            &inputs.pr_body,
            &std::collections::BTreeSet::new(),
            true,
            cancel,
        )?
    };
    let db = inputs.db_handle.as_ref().ok_or("Review storage unavailable")?;
    let mut store = ArchiveStore::open(&inputs.archive_dir, db).map_err(|e| e.to_string())?;
    store
        .deposit_and_link(&archive, &format!("review:{}", inputs.run_id))
        .map_err(|e| e.to_string())?;
    Ok(Arc::new(FrozenContext::new(archive)))
}

pub fn prompt(inputs: &ReviewInputs, context: &Path, files: &[String], output: &Path) -> String {
    let mut prompt = crate::review::evidence::prompt(context, files, output);
    // Budget runtime instructions before the planners validate or split the prompt.
    prompt.push_str(runner::SNAPSHOT_READ_INSTRUCTIONS);
    let Some(frozen) = &inputs.frozen else {
        return prompt;
    };
    let unavailable = ["PENDING_REVIEW.md"]
        .into_iter()
        .filter(|path| !context.join(path).is_file())
        .collect::<Vec<_>>();
    let availability = if unavailable.is_empty() {
        String::new()
    } else {
        format!(
            "\nUnavailable optional context files in this stage: {}. Never cite an unavailable file.\n",
            unavailable.join(", ")
        )
    };
    let suffix = format!("{}{availability}\nConvention evidence may cite only repository-relative paths present under {}/conventions; do not prefix them with `context/` or `conventions/`, and do not cite other archive documents as conventions.\n", frozen.archive.protocol(), context.display());
    let completion = if inputs.db_handle.is_none() {
        ""
    } else {
        "\nOperational review completion: review every assigned primary file using the frozen evidence and report supported findings. If supporting evidence remains unavailable or insufficient, record it in missingEvidence with the affected assigned path and the exact supporting reference when known. Complete the remaining checks and return the report. OpenCodeReview preserves these limitations as warnings, separate from findings; such a review is not fully evidenced. Missing primary coverage, invalid citations and invalid anchors still fail validation. Never invent a supporting reference or a finding to explain a limitation.\n"
    };
    format!("{prompt}{suffix}{completion}")
}

pub(super) fn decision_example(inputs: &ReviewInputs) -> String {
    let mut reference = serde_json::json!({"path":"repository/path","line":1,"quote":"exact inspected source text"});
    if let Some(frozen) = &inputs.frozen {
        reference["revision"] = frozen.archive.scope().head_sha.clone().into();
    }
    let mut example = serde_json::json!({"decisions":[
        {"candidateId":"candidate-1","decision":"keep","unchanged":true,"reason":"Verified; the chunk comment stands as written"},
        {"candidateId":"candidate-2","decision":"discard","reason":"Inspected guard refutes this candidate","counterEvidence":[reference]},
        {"candidateId":"candidate-3","decision":"keep","commentIndex":0,"reason":"Verified; the line and impact are corrected in report.json findings"}
    ]});
    if let Some(frozen) = &inputs.frozen {
        example["contractVersion"] = 2.into();
        example["snapshotId"] = frozen.archive.descriptor().id.into();
    }
    serde_json::to_string_pretty(&example).unwrap()
}

fn assigned_diff(archive: &Archive, files: &[String], diff: &str) -> String {
    let inventory = archive
        .entries()
        .iter()
        .map(|entry| entry.source.path.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    integration::scoped_diff(diff, files, &inventory)
}

pub fn prepare_stage(
    inputs: &ReviewInputs,
    context: &Path,
    files: &[String],
    diff: &str,
    before: &str,
) -> Result<(), String> {
    if let Some(frozen) = &inputs.frozen {
        let diff = assigned_diff(&frozen.archive, files, diff);
        let publication = context.join("PUBLICATION_DIFF.patch");
        if publication.exists() {
            let original = fs::read_to_string(&publication).map_err(|e| e.to_string())?;
            fs::write(&publication, assigned_diff(&frozen.archive, files, &original)).map_err(|e| e.to_string())?;
        }
        frozen
            .archive
            .prepare_stage(context, files, &diff, before)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn materialize(
    inputs: &ReviewInputs,
    report: &Path,
    context: &Path,
    files: &[String],
    head: &str,
    diff: &str,
    summary: &Path,
    comments: &Path,
) -> Result<Vec<crate::review::warnings::EvidenceWarning>, String> {
    let mut warnings = match &inputs.frozen {
        Some(frozen) => {
            let resolver = crate::review::evidence::EvidenceResolver::frozen(context, &frozen.archive);
            let diff = assigned_diff(&frozen.archive, files, diff);
            if inputs.db_handle.is_none() {
                resolver
                    .materialize(report, files, head, &diff, summary, comments)
                    .map(|_| Vec::new())
            } else {
                resolver.materialize_with_warnings(report, files, head, &diff, summary, comments)
            }
        }
        None => crate::review::evidence::materialize(report, context, files, head, diff, summary, comments)
            .map(|_| Vec::new()),
    }?;
    // Findings output repair dropped for failing validation are reported, never silently lost.
    let dropped = report
        .parent()
        .map(|stage| stage.join(crate::review::output_repair::DROPPED))
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
        .unwrap_or_default();
    warnings.extend(dropped.iter().map(|finding| crate::review::warnings::EvidenceWarning {
        reason: "dropped_finding".into(),
        path: finding["path"].as_str().map(str::to_string),
        reference: None,
    }));
    Ok(crate::review::warnings::merge(warnings))
}

pub fn decisions(inputs: &ReviewInputs, raw: &str, context: &Path) -> Result<(), String> {
    match &inputs.frozen {
        Some(frozen) => {
            crate::review::evidence::EvidenceResolver::frozen(context, &frozen.archive).verify_decisions(raw)
        }
        None => crate::review::evidence::verify_decision_evidence(raw, context),
    }
}

static BUSY: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
struct Slot(PathBuf);
impl Drop for Slot {
    fn drop(&mut self) {
        if let Ok(mut busy) = BUSY.lock() {
            busy.retain(|path| path != &self.0);
        }
    }
}
fn slot(targets: &[(PathBuf, String)], cancel: &AtomicBool) -> Result<Slot, String> {
    loop {
        check_cancel(cancel)?;
        {
            let mut busy = BUSY.lock().map_err(|_| "Runner queue unavailable")?;
            if let Some((path, _)) = targets.iter().find(|(path, _)| !busy.contains(path)) {
                busy.push(path.clone());
                return Ok(Slot(path.clone()));
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A free runner among `targets`, held until the returned slot drops.
fn free_target<'a>(
    targets: &'a [(PathBuf, String)],
    cancel: &AtomicBool,
) -> Result<(Slot, &'a (PathBuf, String)), String> {
    let slot = slot(targets, cancel)?;
    let target = targets
        .iter()
        .find(|(path, _)| path == &slot.0)
        .ok_or("Runner unavailable")?;
    Ok((slot, target))
}

/// The runner sockets: `OCR_REVIEW_RUNNER_SOCKETS` when set, otherwise every socket in
/// `OCR_REVIEW_RUNNER_DIR`, where each runner replica listens on its own.
fn runner_sockets() -> Result<Vec<PathBuf>, String> {
    let paths = match std::env::var("OCR_REVIEW_RUNNER_SOCKETS") {
        Ok(sockets) => sockets
            .split(',')
            .map(|path| PathBuf::from(path.trim()))
            .collect::<Vec<_>>(),
        Err(_) => {
            let dir = std::env::var("OCR_REVIEW_RUNNER_DIR").unwrap_or_else(|_| "/review-ipc".into());
            let mut paths = fs::read_dir(&dir)
                .map_err(|_| "Isolation runtime unavailable: no review runners are running")?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|extension| extension == "sock"))
                .collect::<Vec<_>>();
            paths.sort();
            paths
        }
    };
    if paths.len() > crate::review::policy::MAX_RUNNERS
        || paths.iter().any(|path| !path.is_absolute())
        || paths.iter().collect::<std::collections::BTreeSet<_>>().len() != paths.len()
    {
        return Err("Invalid operational runner configuration".into());
    }
    Ok(paths)
}

/// Each runner's CLI version, read once per runner. Replicas come from one image, so a runner busy
/// with another review before it was ever read is skipped rather than waited for.
fn runner_targets(model: &str, cancel: &AtomicBool) -> Result<Vec<(PathBuf, String)>, String> {
    static CATALOGUES: Mutex<Vec<(PathBuf, Vec<String>, String)>> = Mutex::new(Vec::new());
    loop {
        let mut targets = Vec::new();
        for path in runner_sockets()? {
            check_cancel(cancel)?;
            let known = CATALOGUES
                .lock()
                .map_err(|_| "Runner catalogue unavailable")?
                .iter()
                .find(|(known, _, _)| known == &path)
                .map(|(_, models, version)| (models.clone(), version.clone()));
            let catalogue = match known {
                // An idle runner read before may have been replaced since: drop its socket if it is dead.
                Some(_) if try_slot(&path)?.is_some_and(|_slot| runner::is_stale(&path)) => {
                    if let Ok(mut catalogues) = CATALOGUES.lock() {
                        catalogues.retain(|(known, _, _)| known != &path);
                    }
                    None
                }
                Some(catalogue) => Some(catalogue),
                None => {
                    let Some(_slot) = try_slot(&path)? else { continue };
                    match runner::catalog_or_stale(&path) {
                        Ok(Some((models, version))) => {
                            if let Ok(mut catalogues) = CATALOGUES.lock() {
                                catalogues.push((path.clone(), models.clone(), version.clone()));
                            }
                            Some((models, version))
                        }
                        Ok(None) | Err(_) => None,
                    }
                }
            };
            if let Some((models, version)) = catalogue {
                if !models.iter().any(|known| known == model) {
                    return Err("Review model is unavailable in the isolated runtime".into());
                }
                targets.push((path, version));
            }
        }
        if targets.iter().any(|(_, version)| version != &targets[0].1) {
            return Err("Operational runner CLI versions differ".into());
        }
        if !targets.is_empty() {
            return Ok(targets);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Claims `path` only if no review is using it.
fn try_slot(path: &Path) -> Result<Option<Slot>, String> {
    let mut busy = BUSY.lock().map_err(|_| "Runner queue unavailable")?;
    if busy.iter().any(|known| known == path) {
        return Ok(None);
    }
    busy.push(path.to_path_buf());
    Ok(Some(Slot(path.to_path_buf())))
}

pub fn for_each_prepared_request(
    inputs: &ReviewInputs,
    root: &Path,
    stages: &[(PathBuf, PathBuf, PathBuf)],
    version: &str,
    account: i64,
    provider: &str,
    cancel: &AtomicBool,
    mut visit: impl FnMut(usize, runner::Request) -> Result<(), String>,
) -> Result<(), String> {
    let frozen = inputs.frozen.as_ref().ok_or("Frozen snapshot unavailable")?;
    if stages.is_empty() {
        return Err("No review stages prepared".into());
    }
    for (index, (stage, context, prompt)) in stages.iter().enumerate() {
        check_cancel(cancel)?;
        let mut request =
            runner::stage_request(&inputs.model, version, account, provider, root, stage, context, prompt)?;
        let (model, effort) = engine_for(inputs, provider);
        runner::set_engine(&mut request, &model, &effort)?;
        if let runner::Request::Run { files, .. } = &request {
            frozen
                .archive
                .validate_stage_files(files)
                .map_err(|error| error.to_string())?;
        }
        visit(index, request)?;
    }
    Ok(())
}

fn preparation_error(error: String, prefix: String) -> String {
    if error == "cancelled" {
        error
    } else {
        format!("{prefix}: {error}")
    }
}

pub fn preflight(
    inputs: &ReviewInputs,
    root: &Path,
    stages: &[(PathBuf, PathBuf, PathBuf)],
    cancel: &AtomicBool,
) -> Result<(), String> {
    let Some(frozen) = &inputs.frozen else {
        return Ok(());
    };
    crate::accounts::sandbox_sync::sync_app_auth()?;
    let account = next_account(inputs, &[])?;
    let targets = runner_targets(&inputs.model, cancel)?;
    for_each_prepared_request(
        inputs,
        root,
        stages,
        &targets[0].1,
        account.id,
        &account.provider,
        cancel,
        |_, _| Ok(()),
    )?;
    // The runners are replicas of one image, so one free runner stands for all of them.
    let (_slot, target) = free_target(&targets, cancel)?;
    for_each_prepared_request(
        inputs,
        root,
        stages,
        &target.1,
        account.id,
        &account.provider,
        cancel,
        |stage, request| {
            runner::preflight_at(&target.0, &frozen.archive, &request, cancel)
                .map_err(|error| preparation_error(error, format!("Runner stage {} preflight failed", stage + 1)))
        },
    )?;
    frozen
        .root
        .set(root.to_path_buf())
        .map_err(|_| "Review root already prepared")?;
    frozen
        .targets
        .set(targets)
        .map_err(|_| "Review runner preflight was already completed".to_string())
}

pub fn preflight_additional(
    inputs: &ReviewInputs,
    stages: &[(PathBuf, PathBuf, PathBuf)],
    cancel: &AtomicBool,
) -> Result<(), String> {
    let frozen = inputs.frozen.as_ref().ok_or("Frozen snapshot unavailable")?;
    let root = frozen.root.get().ok_or("Review was not preflighted")?;
    let targets = frozen.targets.get().ok_or("Review runners were not preflighted")?;
    let account = next_account(inputs, &[])?;
    for_each_prepared_request(
        inputs,
        root,
        stages,
        &targets[0].1,
        account.id,
        &account.provider,
        cancel,
        |_, _| Ok(()),
    )?;
    let (_slot, target) = free_target(targets, cancel)?;
    for_each_prepared_request(
        inputs,
        root,
        stages,
        &target.1,
        account.id,
        &account.provider,
        cancel,
        |stage, request| {
            runner::preflight_at(&target.0, &frozen.archive, &request, cancel).map_err(|error| {
                preparation_error(
                    error,
                    format!("Runner integration batch {} preflight failed", stage + 1),
                )
            })
        },
    )?;
    Ok(())
}

pub(super) fn execute(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    paths: &SpawnPaths,
) -> Result<(), String> {
    let frozen = inputs.frozen.as_ref().ok_or("Frozen review context unavailable")?;
    let targets = frozen
        .targets
        .get()
        .ok_or("Review inference attempted before global preflight")?;
    let _permit = crate::review::policy::acquire(
        crate::review::policy::MAX_PROCESSES,
        inputs.review_parallel_chunks.max(1),
        cancel,
        inputs.pr_id,
    )?;
    let stage = paths.prompt.parent().ok_or("Stage directory unavailable")?;
    let root = frozen.root.get().ok_or("Missing prepared review root")?;
    let context = if stage.join("context").is_dir() {
        stage.join("context")
    } else {
        root.join("context")
    };
    let mut tried = Vec::new();
    loop {
        let account = next_account(inputs, &tried)?;
        let result = {
            let slot = slot(targets, cancel)?;
            let version = &targets
                .iter()
                .find(|(path, _)| path == &slot.0)
                .ok_or("Runner unavailable")?
                .1;
            let mut request = runner::stage_request(
                &inputs.model,
                version,
                account.id,
                &account.provider,
                root,
                stage,
                &context,
                paths.prompt,
            )?;
            let (model, effort) = engine_for(inputs, &account.provider);
            runner::set_engine(&mut request, &model, &effort)?;
            if inputs.db_handle.is_some() {
                let label = crate::providers::engines::label(&account.provider, &model, &effort);
                let _ = accounts_db(inputs).and_then(|db| db.set_review_run_model(inputs.run_id, &label));
            }
            #[cfg(test)]
            super::usage_tests::guard_diagnostic_execution(stage)?;
            runner::execute_archive_at(
                &slot.0,
                &frozen.archive,
                &request,
                stage,
                paths.transcript,
                cancel,
                &mut || {
                    progress(
                        &crate::review::progress::execution_phase(
                            &spawn_phase_json(paths),
                            paths.progress_phase,
                            "running",
                            now_millis(),
                        ),
                        "Isolated review process started",
                    );
                },
            )
        };
        record_attempt(inputs, paths, &account);
        let error = match result {
            Ok(()) => {
                if account.provider == "claude" {
                    if let Ok(transcript) = fs::read_to_string(paths.transcript) {
                        crate::accounts::usage::observe_claude(account.id, &transcript);
                    }
                }
                let _ = accounts_db(inputs)?.mark_ai_account_used(account.id);
                return Ok(());
            }
            Err(error) => error,
        };
        let (failure, message) = crate::providers::claude::decode(&error);
        let db = accounts_db(inputs)?;
        let reason = match failure {
            crate::providers::claude::Failure::UsageLimit { resets_at } => {
                db.mark_ai_account_limited(account.id, resets_at, message)?;
                "hit its usage limit"
            }
            crate::providers::claude::Failure::Auth => {
                db.mark_ai_account_auth_failed(account.id, message)?;
                "was signed out"
            }
            _ => return Err(message.to_string()),
        };
        tried.push(account.id);
        if tried.len() > MAX_ACCOUNT_FAILOVERS {
            return Err(message.to_string());
        }
        if paths.transcript.exists() {
            fs::rename(paths.transcript, next_failover_transcript(paths.transcript))
                .map_err(|error| error.to_string())?;
        }
        progress(
            &crate::review::progress::execution_phase(
                &spawn_phase_json(paths),
                paths.progress_phase,
                "running",
                now_millis(),
            ),
            &format!(
                "Account \"{}\" {reason}; continuing with the next account",
                account.label
            ),
        );
    }
}

/// Attributes what one execution spent to the account it ran on, before a failover renames its
/// transcript, so every execution of a stage is recorded once. Diagnostics have no database.
fn record_attempt(inputs: &ReviewInputs, paths: &SpawnPaths, account: &crate::db::AiAccount) {
    if inputs.db_handle.is_none() {
        return;
    }
    let usage = super::usage::parse_transcript_usage_single(paths.transcript);
    if usage.by_model.is_empty() {
        return;
    }
    if let Ok(db) = accounts_db(inputs) {
        let _ = db.insert_usage_attempts(
            inputs.run_id,
            inputs.pr_id,
            paths.progress_phase,
            account,
            &usage.by_model,
        );
    }
}

/// The first unused `transcript.failover-N.json`. A stage whose output is repaired runs `execute`
/// again with a fresh failover count, so numbering by that count would overwrite an earlier file.
pub(super) fn next_failover_transcript(transcript: &Path) -> PathBuf {
    (1..)
        .map(|attempt| transcript.with_file_name(format!("transcript.failover-{attempt}.json")))
        .find(|path| !path.exists())
        .expect("an unused failover transcript name")
}

fn accounts_db(inputs: &ReviewInputs) -> Result<Database, String> {
    inputs.open_db()
}

/// The model and effort `provider` reviews with, read for each stage so a change in the app
/// reaches the next stage. Diagnostics, which have no database, use the defaults.
fn engine_for(inputs: &ReviewInputs, provider: &str) -> (String, String) {
    if inputs.db_handle.is_none() {
        return crate::providers::engines::default_engine(provider);
    }
    accounts_db(inputs)
        .and_then(|db| db.review_engine(provider))
        .unwrap_or_else(|_| crate::providers::engines::default_engine(provider))
}

/// The account a stage runs on: the first ready account by priority, the first account's provider
/// before the other, skipping the ones this stage already tried.
fn next_account(inputs: &ReviewInputs, tried: &[i64]) -> Result<crate::db::AiAccount, String> {
    let db = accounts_db(inputs)?;
    if let Some(account) = db.pick_ai_account(tried)? {
        return Ok(account);
    }
    Err(match db.next_ai_account_reset()? {
        Some(at) => format!(
            "Every account is at its usage limit; the next one resets {}",
            resets_in(at)
        ),
        None if !db.list_ai_accounts()?.is_empty() => {
            "No account is ready; sign in again from Settings → Accounts".into()
        }
        None => "Connect an AI account in Settings → Accounts to run reviews".into(),
    })
}

fn resets_in(at: i64) -> String {
    let minutes = ((at - now_millis()) / 60_000).max(1);
    if minutes < 60 {
        format!("in {minutes} min")
    } else {
        format!("in {} h {} min", minutes / 60, minutes % 60)
    }
}
