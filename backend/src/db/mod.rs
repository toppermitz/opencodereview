use std::collections::{HashMap, HashSet};

use pg::{params, Conn, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod accounts;
mod comment_activation;
mod findings;
mod handle;
pub mod import_sqlite;
mod indexing;
mod instances;
mod lifecycle;
mod migrations;
pub(crate) mod pg;
mod queue_acknowledgements;
mod repositories;
mod review_board;
mod review_context;
mod review_coverage;
mod review_requests;
mod review_results;
mod review_runs;
mod review_usage;
mod settings;
mod skills;
mod usage_report;
pub use accounts::AiAccount;
pub use handle::Db;
pub(crate) use handle::ARCHIVE_GATE_KEY;
pub use instances::{INSTANCE_HEARTBEAT, INSTANCE_TIMEOUT_MS};
pub use usage_report::{UsageFilter, USAGE_RANGES};

#[cfg(test)]
pub(crate) use queue_acknowledgements::QUEUE_POST_SETTLE_MS;
pub(crate) use review_results::result_marker_key;

const PENDING_REVIEW_MAX_LINE_BYTES: usize = 1_800;

fn append_bounded_review_text(output: &mut String, text: &str) {
    for segment in text.split_inclusive('\n') {
        let has_newline = segment.ends_with('\n');
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        if line.is_empty() {
            if has_newline {
                output.push('\n');
            }
            continue;
        }
        let mut start = 0;
        while start < line.len() {
            let mut end = (start + PENDING_REVIEW_MAX_LINE_BYTES).min(line.len());
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            output.push_str(&line[start..end]);
            if end < line.len() || has_newline {
                output.push('\n');
            }
            start = end;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub theme: String,
    pub font_size: i64,
    pub sidebar_collapsed: bool,
    pub mission_workspace_configured: bool,
    pub mission_index_done: bool,
    pub poll_interval_seconds: i64,
    pub auto_skip_keywords: Vec<String>,
    pub review_auto_enabled: bool,
    pub review_concurrency: i64,
    pub review_depth_mode: String,
    pub review_deep_threshold: i64,
    pub review_chunk_size: i64,
    pub review_parallel_chunks: i64,
    pub review_max_files: i64,
    /// Above this many changed files a review covers only the highest-priority ones and lists
    /// the rest as not reviewed. `0` reviews every file.
    pub review_focus_files: i64,
    /// Runs one more execution per review stage that looks for what the first pass usually
    /// misses (tests, docs, maintainability, API shape) and adds only new findings.
    pub review_lens_pass: bool,
    /// Lists the review's lower-priority suggestions (tests, docs, naming) in a collapsed section
    /// of its summary. Off by default: a review then posts only its findings and questions.
    pub review_suggestions_enabled: bool,
    pub dismiss_stale_approvals: bool,
    pub auto_index_stale_repos: bool,
    /// Opt-in auto-post mode when a finished review HAS findings, instead of waiting
    /// for manual approval: `"off"` (wait), `"comment"` (post as a NO-verdict
    /// COMMENT review — no approve/request-changes), or `"request_changes"` (post with
    /// GitLab `/request_changes`). Clean reviews wait for manual unless `auto_approve_enabled`.
    pub auto_post_mode: String,
    pub review_assigned_only: bool,
    pub review_target_filter_enabled: bool,
    pub review_target_logins: Vec<String>,
    pub review_target_teams: Vec<String>,
    pub auto_approve_enabled: bool,
    pub auto_approve_max_risk_score: i64,
    pub index_strategy: String,
    pub pr_summary_comment_enabled: bool,
    pub review_result_mode: String,
    pub review_archive_retention_days: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsUpdate {
    pub settings: HashMap<String, String>,
    pub repositories: Option<RepositorySelection>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositorySelection {
    pub logins: Vec<String>,
    pub repo_ids: Vec<i64>,
    #[serde(default)]
    pub monitor_all: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshot {
    pub settings: Settings,
    pub workspaces: Vec<Workspace>,
    pub selected_repos: Vec<Repo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollStatus {
    pub poll_interval_seconds: i64,
    pub running: bool,
    pub last_poll_at: Option<i64>,
    pub next_poll_at: Option<i64>,
    pub last_error: Option<String>,
    pub monitored_repo_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCard {
    pub id: i64,
    pub repo_id: i64,
    pub repo_full_name: String,
    pub number: i64,
    pub title: String,
    pub author: String,
    pub author_avatar_url: Option<String>,
    pub state: String,
    pub lane: String,
    pub status_reason: Option<String>,
    /// Who produced the posted decision: `"you"` (manual) or `"auto"` (AI auto-post).
    /// Drives the actor badge + the "Auto-posted by AI" supervision section.
    pub decided_by: Option<String>,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub base_sha: Option<String>,
    pub head_sha: String,
    pub latest_run_head_sha: Option<String>,
    pub is_stacked: bool,
    pub url: Option<String>,
    pub updated_at: i64,
    pub result_state: Option<String>,
    pub result_observation: Option<String>,
    pub result_observed_head_sha: Option<String>,
    pub result_reviewed_head_sha: Option<String>,
    pub result_prior_head_sha: Option<String>,
    pub result_publication_status: Option<String>,
    pub result_error: Option<String>,
    pub result_inherited: bool,
    /// The latest review has nothing but summary suggestions.
    pub suggestions_only: bool,
    /// The latest review asks questions and reports no finding.
    pub questions_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResult {
    pub id: i64,
    pub pr_id: i64,
    pub installation_id: String,
    pub marker_key: String,
    pub github_comment_id: Option<i64>,
    pub run_id: Option<i64>,
    pub actionable_run_id: Option<i64>,
    pub observed_head_sha: String,
    pub reviewed_head_sha: String,
    pub prior_result_state: Option<String>,
    pub prior_result_head_sha: Option<String>,
    pub observation: String,
    pub effective_state: String,
    pub publication_status: String,
    pub body: Option<String>,
    pub body_hash: Option<String>,
    pub github_review_id: Option<i64>,
    pub github_review_url: Option<String>,
    pub reviewer_version: String,
    pub skill_version: String,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub posted_at: Option<i64>,
}

pub struct LegacyReviewOutcome {
    pub run_id: i64,
    pub status: String,
    pub decision: Option<String>,
    pub head_sha: String,
    pub reviewed_head_sha: Option<String>,
    pub comments_json: Option<String>,
    pub review_body: Option<String>,
}

pub struct ReviewResultInput<'a> {
    pub pr_id: i64,
    pub run_id: i64,
    pub actionable_run_id: Option<i64>,
    pub observed_head_sha: &'a str,
    pub reviewed_head_sha: &'a str,
    pub prior_result_state: Option<&'a str>,
    pub prior_result_head_sha: Option<&'a str>,
    pub observation: &'a str,
    pub effective_state: &'a str,
    pub publication_status: &'a str,
    pub body: Option<&'a str>,
    pub github_review_id: Option<i64>,
    pub github_review_url: Option<&'a str>,
    pub reviewer_version: &'a str,
    pub skill_version: &'a str,
}

pub struct InterruptedReviewPost {
    pub run_id: i64,
    pub pr_id: i64,
    pub full_name: String,
    pub number: i64,
    pub head_sha: String,
    pub event: String,
    pub decision: String,
    pub actor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadComment {
    pub author: Option<String>,
    pub author_avatar_url: Option<String>,
    pub body: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThread {
    pub gh_id: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub comment_count: i64,
    pub last_author: Option<String>,
    pub last_at: Option<i64>,
    pub comments: Vec<ThreadComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRun {
    pub id: i64,
    pub pr_id: i64,
    pub status: String,
    pub head_sha: String,
    pub review_body: Option<String>,
    pub comments_json: Option<String>,
    pub risk_score: Option<i64>,
    pub risk_level: Option<String>,
    pub risk_json: Option<String>,
    pub error_message: Option<String>,
    pub phase_json: Option<String>,
    pub tail: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub created_at: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub usage_model: Option<String>,
    pub model: Option<String>,
    pub context_index_sha: Option<String>,
    pub reviewed_head_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewProgressSnapshot {
    pub run_id: i64,
    pub pr_id: i64,
    pub status: String,
    pub head_sha: String,
    pub error_message: Option<String>,
    pub phase_json: Option<String>,
    pub tail: Option<String>,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRiskDriver {
    pub label: String,
    pub detail: String,
    pub points: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRisk {
    pub score: i64,
    pub level: String,
    pub summary: String,
    pub auto_merge_eligible: bool,
    pub drivers: Vec<ReviewRiskDriver>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub run_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRunStage {
    pub id: i64,
    pub run_id: i64,
    pub pr_id: i64,
    pub stage: String,
    pub chunk_id: Option<i64>,
    pub model: Option<String>,
    pub status: String,
    pub cache_key: Option<String>,
    pub cache_hit: bool,
    pub potential_cache_hit: bool,
    pub potential_savings_tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub prompt_bytes: i64,
    pub diff_bytes: i64,
    pub context_bytes: i64,
    pub file_count: i64,
    pub related_file_count: i64,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ReviewChunkCacheInput {
    pub cache_key: String,
    pub repo_id: i64,
    pub pr_id: i64,
    pub run_id: i64,
    pub chunk_id: i64,
    pub head_sha: String,
    pub prior_sha: Option<String>,
    pub model: String,
    pub prompt_version: String,
    pub skill_hash: String,
    pub index_spec_version: i64,
    pub chunk_json: String,
    pub summary: String,
    pub comments_json: Option<String>,
    pub usage_json: String,
    pub prompt_bytes: i64,
    pub diff_bytes: i64,
    pub context_bytes: i64,
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct ReviewRunStageInput {
    pub run_id: i64,
    pub pr_id: i64,
    pub stage: String,
    pub chunk_id: Option<i64>,
    pub model: Option<String>,
    pub status: String,
    pub cache_key: Option<String>,
    pub cache_hit: bool,
    pub potential_cache_hit: bool,
    pub potential_savings_tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub prompt_bytes: i64,
    pub diff_bytes: i64,
    pub context_bytes: i64,
    pub file_count: i64,
    pub related_file_count: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewStageBreakdown {
    pub run_id: i64,
    pub pr_id: i64,
    pub stages: Vec<ReviewRunStage>,
    pub total_tokens: i64,
    pub potential_cache_hits: i64,
    pub potential_cache_savings_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewBoardGroup {
    pub lane: String,
    pub title: String,
    pub items: Vec<ReviewCard>,
}

/// The indexed package dependency graph (internal packages only) — for the
/// interactive visualization in the index details. Nodes = packages; edges =
/// internal package→package deps (external/npm deps are excluded).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexGraphNode {
    pub id: String,
    pub name: String,
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexGraphEdge {
    pub source: String,
    pub target: String,
    pub dev: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexGraph {
    pub nodes: Vec<IndexGraphNode>,
    pub edges: Vec<IndexGraphEdge>,
}

/// A snapshot of the review skill. `id`/`version` 0 = the built-in baseline (not
/// stored in the table); positive ids are stored, immutable snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillVersion {
    pub id: i64,
    pub version: i64,
    pub note: Option<String>,
    pub char_count: i64,
    pub active: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAction {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub title: String,
    pub message: String,
    pub repo_full_name: Option<String>,
    pub pr_id: Option<i64>,
    pub pr_number: Option<i64>,
    pub pr_title: Option<String>,
    pub author: Option<String>,
    pub author_avatar_url: Option<String>,
    pub created_at: i64,
    pub seen_at: Option<i64>,
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoIndexStatus {
    pub repo_id: i64,
    pub status: String,
    pub stage: Option<String>,
    pub base_ref: Option<String>,
    pub base_sha: Option<String>,
    pub remote_base_sha: Option<String>,
    pub spec_version: i64,
    pub file_count: i64,
    pub package_count: i64,
    pub edge_count: i64,
    pub symbol_count: i64,
    pub skipped_count: i64,
    pub total_count: i64,
    pub started_at: Option<i64>,
    pub indexed_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub error_message: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: i64,
    pub login: String,
    pub kind: String,
    pub selected: bool,
    pub repo_count: i64,
    pub review_skill_repo_count: i64,
    pub refreshed_at: Option<i64>,
    pub monitor_all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repo {
    pub id: i64,
    pub workspace_id: Option<i64>,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub is_private: bool,
    pub default_branch: Option<String>,
    pub skill_path: Option<String>,
    pub skill_status: String,
    pub skill_checked_at: Option<i64>,
    pub skill_error: Option<String>,
    pub subscription_enabled: bool,
    pub review_mode: String,
    pub skip_draft_prs: bool,
    pub skip_self_prs: bool,
    /// Last poll failure for this repo (e.g. SAML SSO not authorized for the org),
    /// with an optional actionable URL (the SSO authorize link). Cleared on success.
    pub poll_error: Option<String>,
    pub poll_error_url: Option<String>,
    pub poll_error_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceInput {
    pub login: String,
    pub kind: String,
}

#[derive(Debug, Clone)]
pub struct RepoInput {
    pub workspace_login: String,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub is_private: bool,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PullRequestInput {
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub author: String,
    pub author_avatar_url: Option<String>,
    pub is_draft: bool,
    pub head_sha: String,
    pub base_sha: Option<String>,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub default_branch: Option<String>,
    pub url: Option<String>,
    pub lane: String,
    pub status_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewCoverageInput<'a> {
    pub repo_id: i64,
    pub pr_id: i64,
    pub run_id: i64,
    pub base_ref: &'a str,
    pub base_sha: &'a str,
    pub merge_base_sha: &'a str,
    pub head_sha: &'a str,
    pub changed_files: &'a [String],
    pub covered_patch_ids: &'a [String],
    pub diff_hash: &'a str,
    pub diff_text: &'a str,
    pub source_json: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub(crate) struct HistoricalReviewCoverageCandidate {
    pub run_id: i64,
    pub pr_id: i64,
    pub base_ref: String,
    pub base_sha: String,
    pub head_sha: String,
    pub manifest_json: Option<String>,
}

pub const INDEX_SPEC_VERSION: i64 = 4;

#[derive(Debug, Clone)]
pub struct RepoIndexFileInput {
    pub path: String,
    pub language: Option<String>,
    pub size: i64,
    pub blob_sha: String,
    pub package_path: Option<String>,
    pub is_binary: bool,
    pub is_generated: bool,
}

#[derive(Debug, Clone)]
pub struct RepoIndexPackageInput {
    pub package_path: String,
    pub name: String,
    pub kind: String,
    pub manager: String,
    pub source_roots_json: String,
    pub test_globs_json: String,
}

#[derive(Debug, Clone)]
pub struct RepoIndexPkgDepInput {
    pub from_package_path: String,
    pub to_package_name: String,
    pub dev: bool,
}

#[derive(Debug, Clone)]
pub struct RepoIndexEdgeInput {
    pub from_path: String,
    pub to_ref: String,
    pub kind: String,
    pub resolved: bool,
    pub to_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RepoIndexSymbolInput {
    pub name: String,
    pub kind: String,
    pub language: String,
    pub path: String,
    pub package_path: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub signature: Option<String>,
    pub is_exported: bool,
}

#[derive(Debug, Clone)]
pub struct RepoIndexSymbolEdgeInput {
    pub from_symbol_index: usize,
    pub to_symbol_index: usize,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPackage {
    pub package_path: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextFile {
    pub path: String,
    pub package_path: Option<String>,
    pub reason: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewContextPlan {
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub reviewed_base_sha: Option<String>,
    #[serde(default)]
    pub merge_base_sha: Option<String>,
    #[serde(default)]
    pub scope_reason: Option<String>,
    #[serde(default)]
    pub scope_selection: Option<crate::review::scope::ScopeSelection>,
    #[serde(default)]
    pub dependency_edges: Vec<(String, String)>,
    pub repo_id: i64,
    pub changed_files: Vec<String>,
    pub packages: Vec<ContextPackage>,
    pub dependent_packages: Vec<String>,
    pub related_files: Vec<ContextFile>,
    pub test_files: Vec<String>,
    pub convention_files: Vec<String>,
    pub omitted_count: i64,
    // Audit of what was actually reviewed (set by run_inner before persisting).
    #[serde(default)]
    pub incremental: bool,
    #[serde(default)]
    pub range: String,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub base_sha: Option<String>,
    #[serde(default)]
    pub prior_sha: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RepoIndexWrite {
    pub base_ref: String,
    pub base_sha: String,
    pub files: Vec<RepoIndexFileInput>,
    pub packages: Vec<RepoIndexPackageInput>,
    pub deps: Vec<RepoIndexPkgDepInput>,
    pub edges: Vec<RepoIndexEdgeInput>,
    pub symbols: Vec<RepoIndexSymbolInput>,
    pub symbol_edges: Vec<RepoIndexSymbolEdgeInput>,
    pub skipped_count: i64,
    pub total_count: i64,
    pub duration_ms: i64,
}

pub struct Database {
    conn: Conn,
    db: Db,
}

fn parent_dir(path: &str) -> Option<String> {
    path.rsplit_once('/').map(|(dir, _)| dir.to_string())
}

fn is_contractish_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("schema")
        || lower.contains("type")
        || lower.contains("contract")
        || lower.contains("api")
        || lower.contains("route")
        || lower.contains("config")
}

fn is_entrypoint_path(path: &str) -> bool {
    path.ends_with("/index.ts")
        || path.ends_with("/index.tsx")
        || path.ends_with("/index.js")
        || path.ends_with("/index.jsx")
}

fn merge_review_phase_progress(previous: Option<&str>, next: &str, tail: &str, now: i64) -> String {
    let old: Vec<serde_json::Value> = previous
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    let Ok(mut phases) = serde_json::from_str::<Vec<serde_json::Value>>(next) else {
        return next.to_string();
    };
    phases.truncate(16);
    let active = phases
        .iter()
        .position(|phase| phase["status"] == "running" || phase["status"] == "queued")
        .or_else(|| {
            phases
                .iter()
                .position(|phase| matches!(phase["status"].as_str(), Some("failed" | "error" | "cancelled")))
        })
        .or_else(|| phases.iter().rposition(|phase| phase["status"] == "ok"));
    for (index, phase) in phases.iter_mut().enumerate() {
        let prior = old.iter().find(|old| old["id"] == phase["id"]);
        let Some(object) = phase.as_object_mut() else {
            continue;
        };
        if let Some(prior) = prior.and_then(|value| value.as_object()) {
            for key in [
                "detail",
                "log",
                "queuedAt",
                "startedAt",
                "finishedAt",
                "events",
                "chunks",
            ] {
                if key == "chunks" && object.contains_key(key) {
                    continue;
                }
                if let Some(value) = prior.get(key) {
                    object.insert(key.to_string(), value.clone());
                }
            }
        }
        let status = object
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        if status == "queued" {
            object.entry("queuedAt").or_insert(serde_json::json!(now));
        }
        if status == "running" {
            object.entry("startedAt").or_insert(serde_json::json!(now));
        }
        if matches!(status.as_str(), "ok" | "failed" | "error" | "cancelled") && object.contains_key("startedAt") {
            object.entry("finishedAt").or_insert(serde_json::json!(now));
        }
        if Some(index) == active {
            let log = tail
                .chars()
                .rev()
                .take(8_192)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            let detail = tail
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect::<String>();
            if !detail.is_empty() {
                let mut events = object
                    .get("events")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                if events.last().and_then(|event| event["message"].as_str()) != Some(detail.as_str()) {
                    events.push(serde_json::json!({ "at": now, "message": detail }));
                }
                if events.len() > 40 {
                    events.drain(..events.len() - 40);
                }
                object.insert("events".to_string(), serde_json::json!(events));
                object.insert("detail".to_string(), serde_json::json!(detail));
            }
            if !log.is_empty() {
                object.insert("log".to_string(), serde_json::json!(log));
            }
        }
    }
    serde_json::to_string(&phases).unwrap_or_else(|_| next.to_string())
}

pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn review_result_from_row(row: &pg::Row) -> Result<ReviewResult, pg::Error> {
    Ok(ReviewResult {
        id: row.get(0)?,
        pr_id: row.get(1)?,
        installation_id: row.get(2)?,
        marker_key: row.get(3)?,
        github_comment_id: row.get(4)?,
        run_id: row.get(5)?,
        actionable_run_id: row.get(6)?,
        observed_head_sha: row.get(7)?,
        reviewed_head_sha: row.get(8)?,
        prior_result_state: row.get(9)?,
        prior_result_head_sha: row.get(10)?,
        observation: row.get(11)?,
        effective_state: row.get(12)?,
        publication_status: row.get(13)?,
        body: row.get(14)?,
        body_hash: row.get(15)?,
        github_review_id: row.get(16)?,
        github_review_url: row.get(17)?,
        reviewer_version: row.get(18)?,
        skill_version: row.get(19)?,
        error_message: row.get(20)?,
        created_at: row.get(21)?,
        updated_at: row.get(22)?,
        posted_at: row.get(23)?,
    })
}

pub const DEFAULT_POLL_INTERVAL_SECONDS: i64 = 60;
pub const DEFAULT_REVIEW_CONCURRENCY: i64 = 20;

const REVIEW_LANES: &[(&str, &str)] = &[
    ("manual_required", "Manual required"),
    ("ready", "Review ready"),
    ("reviewing", "AI reviewing"),
    ("queued", "Queued for review"),
    ("open", "Open / not reviewed"),
    ("draft", "Draft PRs"),
    ("auto_skipped", "Ignored by rule"),
    ("skipped", "Skipped"),
    ("size_skipped", "Skipped by file limit"),
    ("auto_posted", "Auto-posted by AI"),
    ("posted", "Posted / reviewed"),
    ("reviewed_cleanly", "Reviewed cleanly"),
    ("approved", "Approved"),
    ("merged", "Merged"),
    ("closed", "Closed"),
    ("needs_setup", "Needs setup"),
    ("failed", "Failed"),
];

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "concurrency_tests.rs"]
mod concurrency_tests;

#[cfg(test)]
#[path = "instances_tests.rs"]
mod instances_tests;
