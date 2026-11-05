pub(crate) mod coverage;
pub(crate) mod dependencies;
pub(crate) mod evidence;
pub(crate) mod findings;
pub(crate) mod policy;
pub(crate) mod progress;
pub(crate) mod scope;
pub(crate) mod warnings;

pub(crate) mod budget;
mod context_support;
#[cfg(test)]
mod diagnostics;
pub(crate) mod frozen;
mod git;
mod integration;
mod lens;
pub(crate) mod output_repair;
mod paths;
pub(crate) mod published_text;
mod recovery;
#[cfg(test)]
mod recovery_tests;
mod risk;
mod usage;
mod validation;

pub(crate) use context_support::*;
pub(crate) use git::*;
/// Prefix of the preparation error for a pull request whose changed files are all excluded from
/// review evidence; the run is skipped, never failed or marked clean.
pub(crate) const NOTHING_REVIEWABLE: &str = "review has no reviewable files";
pub(crate) use paths::*;
pub(crate) use risk::*;
#[cfg(test)]
pub use usage::collect_usage;
pub use usage::finalize_usage;
#[cfg(test)]
pub use usage::normalize_model;
pub(crate) use usage::parse_transcript_usage;
#[cfg(test)]
pub(crate) use usage::transcript_events;
pub(crate) use validation::*;
mod chunks;
mod execution;
mod local;

pub(crate) use chunks::*;
pub use execution::execute;
#[cfg(test)]
pub(crate) use execution::{deferred_files_note, kept_review_body};
pub(crate) use execution::{review_settings_json, spawn_and_supervise, spawn_phase_json, SpawnPaths};
pub(crate) use local::*;

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::db::{
    ContextFile, Database, ReviewChunkCacheInput, ReviewContextPlan, ReviewRisk, ReviewRiskDriver, ReviewRunStageInput,
    INDEX_SPEC_VERSION,
};
use crate::{gitlab, indexer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const TAIL_MAX_CHARS: usize = 16_000;
const DEFAULT_TIMEOUT_SECS: u64 = 15 * 60;
const NO_OUTPUT_TIMEOUT_SECS: u64 = 3 * 60;
const MAX_RELATED_CONTEXT: usize = 16;
const DEFAULT_REVIEW_CHUNK_SIZE: usize = 5;
const DEFAULT_REVIEW_SHARD_THRESHOLD: usize = 20;
const DEFAULT_MAX_PARALLEL_REVIEW_CHUNKS: usize = 6;
const CHUNK_SCOPE_NOTE: &str = "Business-context checks are resolved in Final.\n";
const GIT_TIMEOUT_SECS: u64 = 120;
pub(crate) const REVIEW_PROMPT_VERSION: &str = "review-v23-narrow-repair";
/// Accounts a single stage may move through after usage limits or rejected sign-ins.
pub(crate) const MAX_ACCOUNT_FAILOVERS: usize = 8;
static CLAUDE_HOME_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewMap {
    dependency_edges: Vec<(String, String)>,
    pr_number: i64,
    title: String,
    description: String,
    head_sha: String,
    base_ref: String,
    changed_files: Vec<String>,
    reviewable_files: Vec<String>,
    omitted_noise_files: Vec<String>,
    diff_stats: DiffStats,
    categories: Vec<FileCategory>,
    packages: Vec<String>,
    dependent_packages: Vec<String>,
    related_files: Vec<ContextFile>,
    convention_files: Vec<String>,
    http_refs: Vec<HttpRef>,
    risk_triggers: Vec<String>,
    data_lifecycle_triggers: Vec<String>,
    public_contract_triggers: Vec<String>,
    test_relationship_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiffStats {
    added: usize,
    removed: usize,
    files: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileCategory {
    path: String,
    category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HttpRef {
    path: String,
    method: String,
    route: String,
    side: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewChunk {
    id: usize,
    files: Vec<String>,
    related_files: Vec<String>,
    global_context_files: Vec<String>,
    #[serde(default)]
    omitted_convention_files: Vec<String>,
    risk_triggers: Vec<String>,
    cross_chunk_group_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ChunkResult {
    cached: bool,
    chunk: ReviewChunk,
    summary: String,
    comments_json: Option<String>,
    evidence_warnings: Vec<crate::review::warnings::EvidenceWarning>,
    usage: RunUsage,
}

/// OpenCodeReview's built-in, default review skill. Embedded at build time.
pub const REVIEW_SKILL: &str = include_str!("../assets/review-skill.md");

#[derive(Clone)]
pub struct ReviewInputs {
    pub frozen: Option<std::sync::Arc<frozen::FrozenContext>>,
    pub run_id: i64,
    pub repo_id: i64,
    pub pr_id: i64,
    pub repo_path: PathBuf,
    pub full_name: String,
    pub number: i64,
    pub base_ref: String,
    pub fixed_head_sha: Option<String>,
    /// Review a local clone that already holds the pinned base and head (`ocr-bench`): no fetch
    /// from origin, no GitHub API. Requires `fixed_head_sha` and a SHA `base_ref`.
    pub offline: bool,
    pub expected_scope: Option<crate::review::scope::ReviewScope>,
    pub prior_scope: Option<crate::review::scope::ReviewScope>,
    /// Head SHA of the last review that produced output, if any. When it is an
    /// ancestor of the new head, the re-review is incremental (only the delta).
    pub last_reviewed_sha: Option<String>,
    pub force_full: bool,
    pub reprocess: bool,
    /// The ACTIVE skill version's content (built-in baseline, or an applied
    /// snapshot) — materialized to context/skill/SKILL.md as the review ruleset.
    pub skill_content: String,
    pub unresolved_review_head: Option<String>,
    pub unresolved_review_body: Option<String>,
    /// PR title + author description (intent/scope) — materialized to context/PR.md
    /// so the reviewer can respect stated intent (e.g. work split across a stack).
    pub pr_title: String,
    pub pr_body: String,
    pub model: String,
    pub review_depth_mode: String,
    pub review_deep_threshold: usize,
    pub review_chunk_size: usize,
    pub review_parallel_chunks: usize,
    /// None for diagnostics, which run without a database.
    pub db_handle: Option<crate::db::Db>,
    /// Where frozen review archives are stored.
    pub archive_dir: PathBuf,
    /// The time this review has, when it has a fixed one (`ocr-bench`); None runs every step.
    pub budget: Option<std::sync::Arc<budget::Budget>>,
}

impl ReviewInputs {
    pub(crate) fn open_db(&self) -> Result<Database, String> {
        self.db_handle.as_ref().ok_or("Review storage unavailable")?.open()
    }
}

pub(crate) fn should_reuse_review_coverage(inputs: &ReviewInputs) -> bool {
    !inputs.force_full
}

pub(crate) fn should_reuse_chunk_results(inputs: &ReviewInputs) -> bool {
    !inputs.force_full && !inputs.reprocess
}

pub struct ReviewOutput {
    pub body: String,
    pub comments_json: Option<String>,
    pub evidence_warnings: Vec<crate::review::warnings::EvidenceWarning>,
    pub usage: RunUsage,
    pub risk: ReviewRisk,
    pub no_new_changes: bool,
}

/// Per-model token usage within a run.
#[derive(Debug, Default, Clone)]
pub struct ModelUsageEntry {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

/// Token usage for a single review run, parsed from the Claude stream-json
/// transcript. A run uses several models (main agent, subagents, compactor);
/// usage is attributed PER MODEL using each step's own generation model.
#[derive(Debug, Default, Clone)]
pub struct RunUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub model: Option<String>,
    pub by_model: Vec<ModelUsageEntry>,
}

#[derive(Clone, Copy)]
pub(crate) struct StageMetrics {
    prompt_bytes: i64,
    diff_bytes: i64,
    context_bytes: i64,
    file_count: i64,
    related_file_count: i64,
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_output_tail(path: &Path) -> String {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut file = fs::File::open(path)?;
        let size = file.metadata()?.len();
        file.seek(SeekFrom::Start(size.saturating_sub(TAIL_MAX_CHARS as u64)))?;
        let mut bytes = Vec::new();
        file.take(TAIL_MAX_CHARS as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    read()
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default()
}

fn context_fingerprint(root: &Path) -> Result<String, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
                pending.push(entry.map_err(|error| error.to_string())?.path());
            }
        } else if metadata.is_file() {
            files.push(path);
        } else {
            return Err("Incomplete review: snapshot contains non-regular evidence".into());
        }
    }
    files.sort();
    let mut entries = Vec::new();
    for path in files {
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy()
            .to_string();
        entries.push((
            relative,
            sha256_hex(&fs::read(path).map_err(|error| error.to_string())?),
        ));
    }
    Ok(sha256_hex(
        serde_json::to_string(&entries)
            .map_err(|error| error.to_string())?
            .as_bytes(),
    ))
}

fn file_bytes(path: &Path) -> i64 {
    fs::metadata(path).map(|meta| meta.len() as i64).unwrap_or(0)
}

fn dir_bytes(path: &Path) -> i64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += dir_bytes(&path);
            } else {
                total += file_bytes(&path);
            }
        }
    }
    total
}

fn validate_review_stage_inputs(context: &Path, additional_inputs: Option<&Path>, prompt: &Path) -> Result<(), String> {
    let context_bytes = dir_bytes(context) as usize;
    let prompt_bytes = file_bytes(prompt) as usize;
    let additional = additional_inputs.map_or(0, |directory| {
        let nested_context = if context.starts_with(directory) {
            context_bytes
        } else {
            0
        };
        (dir_bytes(directory) as usize)
            .saturating_sub(prompt_bytes)
            .saturating_sub(nested_context)
    });
    crate::review::policy::validate_prepared_inputs(context_bytes + additional, prompt_bytes)
}

fn record_stage(
    inputs: &ReviewInputs,
    stage: &str,
    chunk_id: Option<i64>,
    status: &str,
    cache_key: Option<String>,
    potential_cache_hit: bool,
    usage: &RunUsage,
    metrics: StageMetrics,
    started_at: i64,
    finished_at: i64,
) {
    if let Ok(db) = inputs.open_db() {
        let _ = db.insert_review_run_stage(&ReviewRunStageInput {
            run_id: inputs.run_id,
            pr_id: inputs.pr_id,
            stage: stage.to_string(),
            chunk_id,
            model: Some(stage_model(inputs, usage)),
            status: status.to_string(),
            cache_key,
            cache_hit: status == "cached",
            potential_cache_hit,
            potential_savings_tokens: if potential_cache_hit {
                usage.input_tokens + usage.output_tokens
            } else {
                0
            },
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            prompt_bytes: metrics.prompt_bytes,
            diff_bytes: metrics.diff_bytes,
            context_bytes: metrics.context_bytes,
            file_count: metrics.file_count,
            related_file_count: metrics.related_file_count,
            started_at,
            finished_at: Some(finished_at),
        });
    }
}

/// Model id shown on a stage: Claude transcripts name the model; Codex usage is rewritten to the
/// engine captured at execution (see `usage::write_stage_engine`). Unresolved "codex" and cache
/// hits with no usage keep the job's engine.
fn stage_model(inputs: &ReviewInputs, usage: &RunUsage) -> String {
    match usage.model.as_deref() {
        Some("unknown") | None | Some("codex") => inputs.model.clone(),
        Some(model) => model.to_string(),
    }
}

fn build_snapshot_context_plan(
    inputs: &ReviewInputs,
    paths: &[String],
    head: &str,
    base: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
) -> Result<(ReviewContextPlan, Option<String>), String> {
    let db_handle = inputs.db_handle.as_ref().ok_or("Review storage unavailable")?;
    // The head commit's index lives in `scratch` under this run's id, apart from the repository's
    // own index; `execute` drops it when the run ends. No connection is held while indexing.
    let index_id = inputs.run_id;
    let stale = {
        let db = db_handle.open_scratch()?;
        db.ensure_temp_repo_for_index(index_id, &inputs.full_name)?;
        let status = db.repo_index_status(index_id)?;
        let stale = status.status != "ready"
            || status.base_sha.as_deref() != Some(head)
            || status.spec_version != INDEX_SPEC_VERSION;
        if stale {
            db.begin_repo_index(index_id, Some(head))?;
        }
        stale
    };
    if stale {
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let write = indexer::run_index_for_ref(&inputs.repo_path, head, None, cancel, &mut |event| {
            if last_emit.elapsed() >= Duration::from_secs(1) {
                progress(
                    &phase_json("context", "running"),
                    &format!(
                        "Indexing pinned PR snapshot: {} ({}/{})",
                        event.stage, event.processed, event.total
                    ),
                );
                last_emit = Instant::now();
            }
        })?;
        db_handle.open_scratch()?.write_repo_index(index_id, &write)?;
    }
    let db = db_handle.open_scratch()?;
    let mut plan = db.build_review_context_plan(index_id, paths, MAX_RELATED_CONTEXT)?;
    for path in paths {
        for (related, _, _) in db.related_index_files(index_id, std::slice::from_ref(path), 100)? {
            plan.dependency_edges.push((path.clone(), related));
        }
    }
    plan.dependency_edges.sort();
    plan.dependency_edges.dedup();
    for file in &mut plan.related_files {
        file.source = "head".into();
    }
    plan.convention_files = git::convention_files_at(&inputs.repo_path, base, cancel)?;
    // The plan comes from this run's temporary index; it belongs to the pull request's repository.
    plan.repo_id = inputs.repo_id;
    Ok((plan, Some(head.to_string())))
}

#[cfg(test)]
#[path = "tests/failover_usage.rs"]
mod failover_usage_tests;
#[cfg(test)]
#[path = "tests/guidelines.rs"]
mod guidelines_tests;
#[cfg(test)]
#[path = "tests/usage_chunks.rs"]
mod usage_chunks_tests;
#[cfg(test)]
#[path = "tests/usage_publication.rs"]
mod usage_publication_tests;
#[cfg(test)]
#[path = "tests/usage.rs"]
mod usage_tests;
