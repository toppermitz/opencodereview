use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewScope {
    pub head_sha: String,
    pub base_ref: String,
    pub base_sha: String,
}

/// Whether a stored review plan belongs to repository `repo_id`. Plans stored before the fix in
/// `build_snapshot_context_plan` carry their run's temporary index id (the run id) instead.
pub(crate) fn plan_belongs_to(plan: &crate::db::ReviewContextPlan, repo_id: i64, run_id: i64) -> bool {
    plan.repo_id == repo_id || plan.repo_id == run_id
}

pub fn merge_base(repo: &Path, base: &str, head: &str, cancel: &AtomicBool) -> Result<String, String> {
    let bytes = crate::review::git_capture(repo, &["merge-base", base, head], cancel, 15)?;
    let sha = String::from_utf8(bytes)
        .map_err(|error| error.to_string())?
        .trim()
        .to_string();
    if sha.is_empty() {
        return Err("Review merge-base could not be resolved".into());
    }
    Ok(sha)
}

pub fn equivalent(repo: &Path, previous: &ReviewScope, current: &ReviewScope, cancel: &AtomicBool) -> bool {
    if previous.head_sha != current.head_sha || previous.base_ref != current.base_ref {
        return false;
    }
    if previous.base_sha == current.base_sha {
        return true;
    }
    match (
        merge_base(repo, &previous.base_sha, &previous.head_sha, cancel),
        merge_base(repo, &current.base_sha, &current.head_sha, cancel),
    ) {
        (Ok(before), Ok(after)) => before == after,
        _ => false,
    }
}

pub fn can_review_incrementally(
    repo: &Path,
    previous: &ReviewScope,
    current: &ReviewScope,
    cancel: &AtomicBool,
) -> bool {
    if previous.head_sha == current.head_sha || previous.base_ref != current.base_ref {
        return false;
    }
    if !crate::review::is_ancestor(repo, &previous.head_sha, &current.head_sha, cancel) {
        return false;
    }
    match (
        merge_base(repo, &previous.base_sha, &previous.head_sha, cancel),
        merge_base(repo, &current.base_sha, &current.head_sha, cancel),
    ) {
        (Ok(before), Ok(after)) => before == after,
        _ => false,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CoverageSnapshot {
    pub run_id: i64,
    pub scope: ReviewScope,
    pub merge_base_sha: String,
    pub changed_files: Vec<String>,
    pub diff_hash: String,
    pub diff_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSelection {
    pub mode: String,
    pub requested_full: bool,
    pub source_run_id: Option<i64>,
    pub full_pr_files: Vec<String>,
    pub reused_files: Vec<String>,
    #[serde(default)]
    pub excluded_files: Vec<ScopeExclusion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScopeExclusion {
    pub path: String,
    pub reason: crate::review_archive::Exclusion,
}

pub(crate) fn exclude_policy_paths(changed: &mut Vec<String>, selection: &mut ScopeSelection) -> Result<(), String> {
    let mut excluded = Vec::new();
    for path in &selection.full_pr_files {
        if let Some(reason) = crate::review_archive::path_exclusion(path).map_err(|error| error.to_string())? {
            excluded.push(ScopeExclusion {
                path: path.clone(),
                reason,
            });
        }
    }
    changed.retain(|path| !excluded.iter().any(|file| file.path == *path));
    selection
        .reused_files
        .retain(|path| !changed.contains(path) && !excluded.iter().any(|file| file.path == *path));
    selection.excluded_files = excluded;
    Ok(())
}

/// Above `cap` files, keeps the `cap` highest-priority ones in `changed`, in their original order,
/// and returns the rest. Source code comes before docs, config and lockfiles, then the files with
/// the most changed lines. `cap` 0 keeps every file.
pub(crate) fn focus_files(changed: &mut Vec<String>, diff: &str, cap: usize) -> Vec<String> {
    if cap == 0 || changed.len() <= cap {
        return Vec::new();
    }
    let mut churn = std::collections::HashMap::<String, usize>::new();
    let mut current = None::<String>;
    // File headers come before a file's first hunk; inside a hunk, a removed `-- comment` line
    // reads `--- comment` and is a change like any other.
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            in_hunk = false;
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if let Some(raw) = line.strip_prefix("+++ ").filter(|_| !in_hunk) {
            current = crate::review_archive::decoded_git_path(raw)
                .and_then(|path| path.strip_prefix("b/").map(str::to_string))
                .or(current.take());
        } else if let Some(raw) = line.strip_prefix("--- ").filter(|_| !in_hunk) {
            current = crate::review_archive::decoded_git_path(raw)
                .and_then(|path| path.strip_prefix("a/").map(str::to_string));
        } else if in_hunk && (line.starts_with('+') || line.starts_with('-')) {
            if let Some(path) = &current {
                *churn.entry(path.clone()).or_default() += 1;
            }
        }
    }
    let source = |path: &str| {
        let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
        let extension = name.rsplit_once('.').map(|(_, extension)| extension).unwrap_or("");
        !matches!(
            extension,
            "" | "md" | "mdx" | "txt" | "rst" | "json" | "yml" | "yaml" | "toml" | "lock" | "po" | "svg" | "csv"
        )
    };
    let mut ranked = changed.clone();
    ranked.sort_by_key(|path| {
        (
            std::cmp::Reverse(source(path)),
            std::cmp::Reverse(churn.get(path).copied().unwrap_or(0)),
        )
    });
    let kept = ranked[..cap].iter().cloned().collect::<std::collections::HashSet<_>>();
    let deferred = changed.iter().filter(|path| !kept.contains(*path)).cloned().collect();
    changed.retain(|path| kept.contains(path));
    deferred
}

pub(crate) fn reprocess_files(previous: &[String], current: &[String]) -> Result<Vec<String>, String> {
    if previous.is_empty() || previous.iter().collect::<std::collections::HashSet<_>>().len() != previous.len() {
        return Err("O escopo de análise anterior está vazio ou duplicado; escolha Reexecutar completa".into());
    }
    for path in previous {
        crate::review_archive::safe_path(path).map_err(|error| error.to_string())?;
    }
    let files = current
        .iter()
        .filter(|path| previous.contains(path))
        .cloned()
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Err("O escopo de análise anterior não está mais no MR atual; escolha Reexecutar completa".into());
    }
    Ok(files)
}

pub(crate) fn reprocess_selection(
    previous: &[String],
    current: &[String],
    source_run: i64,
) -> Result<(Vec<String>, ScopeSelection), String> {
    let mut changed = reprocess_files(previous, current)?;
    let mut selection = ScopeSelection {
        mode: "reprocess".into(),
        requested_full: false,
        source_run_id: Some(source_run),
        full_pr_files: current.to_vec(),
        reused_files: current.iter().filter(|path| !changed.contains(path)).cloned().collect(),
        excluded_files: Vec::new(),
    };
    exclude_policy_paths(&mut changed, &mut selection)?;
    if changed.is_empty() {
        return Err("previous analysis has no reviewable paths under the current policy".into());
    }
    Ok((changed, selection))
}

#[derive(Debug)]
pub(crate) struct BaseSyncSelection {
    pub changed_files: Vec<String>,
    pub diff: String,
    pub selection: ScopeSelection,
}

fn matching_index_line(saved: &str, actual: &str) -> bool {
    fn fields(line: &str) -> Option<(&str, &str, &str)> {
        let line = line.strip_prefix("index ")?.strip_suffix('\n')?;
        let (ids, mode) = line.split_once(' ').unwrap_or((line, ""));
        let (before, after) = ids.split_once("..")?;
        Some((before, after, mode))
    }
    let Some((saved_before, saved_after, saved_mode)) = fields(saved) else {
        return false;
    };
    let Some((before, after, mode)) = fields(actual) else {
        return false;
    };
    let matches_id = |prefix: &str, full: &str| {
        matches!(full.len(), 40 | 64)
            && (4..=full.len()).contains(&prefix.len())
            && full.bytes().all(|byte| byte.is_ascii_hexdigit())
            && full.starts_with(prefix)
    };
    saved_mode == mode && matches_id(saved_before, before) && matches_id(saved_after, after)
}

fn coverage_diff_matches(saved: &str, actual: &str) -> bool {
    let mut saved_lines = saved.split_inclusive('\n');
    for line in actual.split_inclusive('\n') {
        let Some(saved) = saved_lines.next() else {
            return false;
        };
        if saved != line && !matching_index_line(saved, line) {
            return false;
        }
    }
    saved_lines.next().is_none()
}

pub(crate) fn verify_coverage(repo: &Path, coverage: &CoverageSnapshot, cancel: &AtomicBool) -> Result<(), String> {
    verify_coverage_range(
        repo,
        coverage,
        &format!("{}...{}", coverage.scope.base_sha, coverage.scope.head_sha),
        cancel,
    )
}

pub(crate) fn verify_legacy_backfill(
    repo: &Path,
    coverage: &CoverageSnapshot,
    cancel: &AtomicBool,
) -> Result<(), String> {
    verify_coverage_range(
        repo,
        coverage,
        &format!("{}..{}", coverage.scope.base_sha, coverage.scope.head_sha),
        cancel,
    )
}

fn verify_coverage_range(
    repo: &Path,
    coverage: &CoverageSnapshot,
    range: &str,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let diff = crate::review::diff_local(repo, range, cancel)?;
    let files = crate::review::changed_files_local(repo, range, cancel)?;
    if coverage.merge_base_sha != merge_base(repo, &coverage.scope.base_sha, &coverage.scope.head_sha, cancel)?
        || coverage.diff_hash != crate::review::coverage::diff_hash(&coverage.diff_text)
        || !coverage_diff_matches(&coverage.diff_text, &diff)
        || coverage.changed_files != files
    {
        return Err("Persisted review coverage does not match its Git snapshot".into());
    }
    Ok(())
}

pub(crate) fn prepare_scope_reuse(
    repo: &Path,
    previous: Option<&ReviewScope>,
    previous_head: Option<&str>,
    current: &ReviewScope,
    coverage: Option<&CoverageSnapshot>,
    force_full: bool,
    cancel: &AtomicBool,
) -> Result<Option<BaseSyncSelection>, String> {
    if force_full || (previous.is_none() && previous_head.is_none()) {
        return Ok(None);
    }
    let coverage = coverage.ok_or("completed coverage for the previous review is unavailable")?;
    if previous != Some(&coverage.scope) || previous_head != Some(coverage.scope.head_sha.as_str()) {
        return Err("persisted coverage belongs to a different review scope".into());
    }
    verify_coverage(repo, coverage, cancel)?;
    if coverage.scope.base_ref != current.base_ref || can_review_incrementally(repo, &coverage.scope, current, cancel) {
        return Ok(None);
    }
    if coverage.scope.head_sha != current.head_sha
        && crate::review::is_ancestor(repo, &coverage.scope.head_sha, &current.head_sha, cancel)
        && crate::review::is_ancestor(repo, &coverage.scope.base_sha, &current.base_sha, cancel)
    {
        select_base_sync(repo, coverage, current, cancel)
    } else {
        select_content_scope(repo, coverage, current, "content_reuse", cancel).map(Some)
    }
}

pub(crate) fn select_base_sync(
    repo: &Path,
    coverage: &CoverageSnapshot,
    current: &ReviewScope,
    cancel: &AtomicBool,
) -> Result<Option<BaseSyncSelection>, String> {
    let previous = &coverage.scope;
    let current_merge_base = merge_base(repo, &current.base_sha, &current.head_sha, cancel)?;
    if coverage.merge_base_sha == current_merge_base {
        return Ok(None);
    }
    if previous.base_ref != current.base_ref
        || previous.head_sha == current.head_sha
        || !crate::review::is_ancestor(repo, &previous.head_sha, &current.head_sha, cancel)
        || !crate::review::is_ancestor(repo, &previous.base_sha, &current.base_sha, cancel)
        || !crate::review::is_ancestor(repo, &coverage.merge_base_sha, &current_merge_base, cancel)
    {
        return Err("Changed review scope is not a verified base synchronization".into());
    }
    select_content_scope(repo, coverage, current, "base_sync", cancel).map(Some)
}

fn select_content_scope(
    repo: &Path,
    coverage: &CoverageSnapshot,
    current: &ReviewScope,
    mode: &str,
    cancel: &AtomicBool,
) -> Result<BaseSyncSelection, String> {
    verify_coverage(repo, coverage, cancel)?;
    let previous = &coverage.scope;
    if previous.base_ref != current.base_ref {
        return Err("Content reuse requires the same immediate base branch".into());
    }
    let current_merge_base = merge_base(repo, &current.base_sha, &current.head_sha, cancel)?;
    let full_range = format!("{}...{}", current.base_sha, current.head_sha);
    let full_pr_files = crate::review::changed_files_local(repo, &full_range, cancel)?;
    let head_changes =
        crate::review::changed_files_local(repo, &format!("{}..{}", previous.head_sha, current.head_sha), cancel)?;
    let base_changes = crate::review::changed_files_local(
        repo,
        &format!("{}..{}", coverage.merge_base_sha, current_merge_base),
        cancel,
    )?;
    let mut assigned = full_pr_files
        .iter()
        .filter(|path| {
            !coverage.changed_files.contains(path) || head_changes.contains(path) || base_changes.contains(path)
        })
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    for path in &full_pr_files {
        if assigned.contains(path)
            && coverage.changed_files.contains(path)
            && base_changes.contains(path)
            && crate::review_archive::path_exclusion(path)
                .map_err(|error| error.to_string())?
                .is_none()
        {
            let before = file_version(repo, &coverage.merge_base_sha, path, cancel)?;
            let reviewed = file_version(repo, &previous.head_sha, path, cancel)?;
            let base = file_version(repo, &current_merge_base, path, cancel)?;
            if let Some(expected) = merge_file_versions(repo, &before, &reviewed, &base, cancel)? {
                if expected == file_version(repo, &current.head_sha, path, cancel)? {
                    assigned.remove(path);
                }
            }
        }
    }
    close_rename_scope(repo, &full_range, &mut assigned, cancel)?;
    let (changed_files, reused_files): (Vec<_>, Vec<_>) =
        full_pr_files.iter().cloned().partition(|path| assigned.contains(path));
    let diff = crate::review::diff_local_paths(repo, &full_range, &changed_files, cancel);
    if !changed_files.is_empty() && diff.is_empty() {
        return Err("Selected base synchronization diff is unavailable".into());
    }
    Ok(BaseSyncSelection {
        changed_files,
        diff,
        selection: ScopeSelection {
            mode: mode.into(),
            requested_full: false,
            source_run_id: Some(coverage.run_id),
            full_pr_files,
            reused_files,
            excluded_files: Vec::new(),
        },
    })
}

fn close_rename_scope(
    repo: &Path,
    range: &str,
    assigned: &mut std::collections::HashSet<String>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let statuses = crate::review::git_capture(
        repo,
        &["diff", "--name-status", "-z", "--find-renames", range],
        cancel,
        60,
    )?;
    let statuses = std::str::from_utf8(&statuses)
        .map_err(|error| error.to_string())?
        .split('\0')
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let mut index = 0;
    while index < statuses.len() {
        let status = statuses[index];
        let path = *statuses.get(index + 1).ok_or("Incomplete Git path inventory")?;
        index += 2;
        if status.starts_with('R') || status.starts_with('C') {
            let destination = *statuses.get(index).ok_or("Incomplete Git rename inventory")?;
            index += 1;
            if assigned.contains(path) || assigned.contains(destination) {
                assigned.insert(path.to_string());
                assigned.insert(destination.to_string());
            }
        }
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
enum FileVersion {
    Missing,
    Text { mode: String, bytes: Vec<u8> },
    Opaque { mode: String, oid: String },
}

fn file_version(repo: &Path, revision: &str, path: &str, cancel: &AtomicBool) -> Result<FileVersion, String> {
    crate::review_archive::safe_path(path).map_err(|error| error.to_string())?;
    let raw = crate::review::git_capture(
        repo,
        &[
            "ls-tree",
            "--full-tree",
            "-l",
            "-z",
            revision,
            "--",
            &format!(":(literal){path}"),
        ],
        cancel,
        30,
    )?;
    if raw.is_empty() {
        return Ok(FileVersion::Missing);
    }
    let record = std::str::from_utf8(&raw).map_err(|error| error.to_string())?;
    let (metadata, name) = record
        .strip_suffix('\0')
        .and_then(|record| record.split_once('\t'))
        .ok_or("Invalid Git tree entry")?;
    if name != path {
        return Err("Git tree path does not match coverage".into());
    }
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 4 {
        return Err("Invalid Git tree metadata".into());
    }
    let mode = fields[0].to_string();
    let oid = fields[2].to_string();
    if matches!(mode.as_str(), "100644" | "100755")
        && fields[1] == "blob"
        && fields[3]
            .parse::<u64>()
            .is_ok_and(|size| size <= crate::review_archive::MAX_FILE_BYTES)
    {
        let bytes = crate::review::git_capture(repo, &["cat-file", "blob", &oid], cancel, 30)?;
        if !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok() {
            return Ok(FileVersion::Text { mode, bytes });
        }
    }
    Ok(FileVersion::Opaque { mode, oid })
}

fn merge_file_versions(
    repo: &Path,
    before: &FileVersion,
    ours: &FileVersion,
    theirs: &FileVersion,
    cancel: &AtomicBool,
) -> Result<Option<FileVersion>, String> {
    if ours == before || ours == theirs {
        return Ok(Some(theirs.clone()));
    }
    if theirs == before {
        return Ok(Some(ours.clone()));
    }
    let (
        FileVersion::Text {
            mode: before_mode,
            bytes: before,
        },
        FileVersion::Text {
            mode: our_mode,
            bytes: ours,
        },
        FileVersion::Text {
            mode: their_mode,
            bytes: theirs,
        },
    ) = (before, ours, theirs)
    else {
        return Ok(None);
    };
    let mode = if our_mode == before_mode {
        their_mode
    } else if their_mode == before_mode || our_mode == their_mode {
        our_mode
    } else {
        return Ok(None);
    };
    if ours == before || ours == theirs {
        return Ok(Some(FileVersion::Text {
            mode: mode.clone(),
            bytes: theirs.clone(),
        }));
    }
    if theirs == before {
        return Ok(Some(FileVersion::Text {
            mode: mode.clone(),
            bytes: ours.clone(),
        }));
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "ocr-coverage-merge-{}-{stamp}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&root).map_err(|error| error.to_string())?;
    let result = (|| {
        let paths = [root.join("ours"), root.join("before"), root.join("theirs")];
        for (path, bytes) in paths.iter().zip([ours, before, theirs]) {
            std::fs::write(path, bytes).map_err(|error| error.to_string())?;
        }
        let args = paths
            .iter()
            .map(|path| path.to_str().ok_or("Non-UTF-8 coverage scratch path"))
            .collect::<Result<Vec<_>, _>>()?;
        let merged = crate::review::git_capture(
            repo,
            &["merge-file", "--stdout", "--diff3", args[0], args[1], args[2]],
            cancel,
            30,
        );
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        Ok(merged.ok().map(|bytes| FileVersion::Text {
            mode: mode.clone(),
            bytes,
        }))
    })();
    let cleanup = std::fs::remove_dir_all(root).map_err(|error| error.to_string());
    cleanup.and(result)
}

pub(crate) fn reuse_stacked_files(
    repo: &Path,
    previous: Option<&CoverageSnapshot>,
    current: &ReviewScope,
    sources: &[CoverageSnapshot],
    changed: &mut Vec<String>,
    selection: &mut ScopeSelection,
    cancel: &AtomicBool,
) -> Result<Vec<i64>, String> {
    if selection.requested_full || changed.is_empty() || sources.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(previous) = previous {
        verify_coverage(repo, previous, cancel)?;
    }
    for source in sources {
        verify_coverage(repo, source, cancel)?;
    }
    let current_base = merge_base(repo, &current.base_sha, &current.head_sha, cancel)?;
    let mut assigned = changed.iter().cloned().collect::<std::collections::HashSet<_>>();
    let mut proofs = std::collections::HashMap::new();
    for path in changed.iter() {
        if !sources.iter().any(|source| source.changed_files.contains(path))
            || crate::review_archive::path_exclusion(path)
                .map_err(|error| error.to_string())?
                .is_some()
        {
            continue;
        }
        let base = file_version(repo, &current_base, path, cancel)?;
        let mut expected = if let Some(previous) = previous {
            let before = file_version(repo, &previous.merge_base_sha, path, cancel)?;
            let reviewed = file_version(repo, &previous.scope.head_sha, path, cancel)?;
            let Some(merged) = merge_file_versions(repo, &before, &reviewed, &base, cancel)? else {
                continue;
            };
            merged
        } else {
            base
        };
        let actual = file_version(repo, &current.head_sha, path, cancel)?;
        let mut used = Vec::new();
        for source in sources.iter().filter(|source| source.changed_files.contains(path)) {
            let before = file_version(repo, &source.merge_base_sha, path, cancel)?;
            let reviewed = file_version(repo, &source.scope.head_sha, path, cancel)?;
            let Some(merged) = merge_file_versions(repo, &before, &expected, &reviewed, cancel)? else {
                continue;
            };
            if merged != expected {
                used.push(source.run_id);
            }
            expected = merged;
            if expected == actual && !used.is_empty() {
                assigned.remove(path);
                proofs.insert(path.clone(), used);
                break;
            }
        }
    }
    close_rename_scope(
        repo,
        &format!("{}...{}", current.base_sha, current.head_sha),
        &mut assigned,
        cancel,
    )?;
    let mut source_runs = proofs
        .into_iter()
        .filter(|(path, _)| !assigned.contains(path))
        .flat_map(|(_, runs)| runs)
        .collect::<Vec<_>>();
    source_runs.sort_unstable();
    source_runs.dedup();
    if !source_runs.is_empty() {
        *changed = selection
            .full_pr_files
            .iter()
            .filter(|path| assigned.contains(*path))
            .cloned()
            .collect();
        selection.reused_files = selection
            .full_pr_files
            .iter()
            .filter(|path| !assigned.contains(*path))
            .cloned()
            .collect();
        selection.mode = "content_reuse".into();
    }
    Ok(source_runs)
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
