use super::*;

pub(crate) fn kill_tree(pid: u32, child: &mut std::process::Child) {
    // Negative PID targets the whole process group (model CLI + child gh). Use the
    // syscall: procps-ng 4.0.2 `kill -KILL -<pgid>` signals every process when
    // the group is already gone, and runtime images may not ship `kill` at all.
    #[cfg(unix)]
    if let Ok(group) = i32::try_from(pid) {
        if group > 1 {
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Fetch the merge-request head into a private local ref via git transport,
/// never the REST API. Returns the head SHA. Does not touch the working tree.
pub(crate) fn fetch_pr_head(repo_path: &Path, number: i64, cancel: &AtomicBool) -> Result<String, String> {
    let refspec = format!("refs/merge-requests/{number}/head:refs/ocr/pr-{number}");
    git_capture(
        repo_path,
        &["fetch", "origin", &refspec, "--no-tags", "--force", "--quiet"],
        cancel,
        GIT_TIMEOUT_SECS,
    )
    .map_err(|error| format!("could not fetch merge request head via git: {error}"))?;
    let sha = String::from_utf8(git_capture(
        repo_path,
        &["rev-parse", &format!("refs/ocr/pr-{number}")],
        cancel,
        30,
    )?)
    .map_err(|error| error.to_string())?
    .trim()
    .to_string();
    if sha.is_empty() {
        return Err("could not resolve merge request head SHA after fetch".to_string());
    }
    Ok(sha)
}

/// Refresh the immediate base branch's remote-tracking ref before reviewing.
/// Fetch failures propagate so stale refs cannot silently change the scope.
pub(crate) fn fetch_base(repo_path: &Path, base_ref: &str, cancel: &AtomicBool) -> Result<(), String> {
    let branch = base_ref.strip_prefix("origin/").unwrap_or(base_ref);
    if branch.is_empty() || branch == "HEAD" {
        return Err("A branch base imediata do MR está ausente. Atualize o MR antes de revisar.".into());
    }
    let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    git_capture(
        repo_path,
        &["fetch", "origin", &refspec, "--no-tags", "--quiet"],
        cancel,
        GIT_TIMEOUT_SECS,
    )
    .map(|_| ())
}

/// Remote-tracking ref for a PR base branch ("feature-x" → "origin/feature-x").
/// Diffs and base-content reads use this so they match GitHub's PR diff even when
/// no local branch exists — e.g. a STACKED PR whose base is another PR's branch.
pub(crate) fn remote_base(base_ref: &str) -> String {
    let branch = base_ref.strip_prefix("origin/").unwrap_or(base_ref);
    format!("origin/{branch}")
}

pub(crate) fn resolve_rev_sha(repo_path: &Path, rev: &str, cancel: &AtomicBool) -> Option<String> {
    git_capture(repo_path, &["rev-parse", rev], cancel, 15)
        .ok()
        .map(|out| String::from_utf8_lossy(&out).trim().to_string())
        .filter(|sha| !sha.is_empty())
}

pub(crate) fn rev_exists(repo_path: &Path, rev: &str, cancel: &AtomicBool) -> bool {
    git_capture(repo_path, &["rev-parse", "--verify", "--quiet", rev], cancel, 15)
        .map(|out| !String::from_utf8_lossy(&out).trim().is_empty())
        .unwrap_or(false)
}

/// The remote's default branch ("origin/main"/"origin/master"), best-effort.
pub(crate) fn remote_default_branch(repo_path: &Path, cancel: &AtomicBool) -> Option<String> {
    if let Ok(out) = git_capture(repo_path, &["rev-parse", "--abbrev-ref", "origin/HEAD"], cancel, 15) {
        let name = String::from_utf8_lossy(&out).trim().to_string();
        if name.starts_with("origin/") && name != "origin/HEAD" {
            return Some(name);
        }
    }
    ["origin/main", "origin/master"]
        .into_iter()
        .find(|c| rev_exists(repo_path, c, cancel))
        .map(str::to_string)
}

/// Resolve the exact PR base to an immutable SHA after refreshing its remote ref.
/// A failed fetch must not reuse a stale remote-tracking ref, including stacks
/// whose parent branch was deleted. Live GitHub metadata supplies retargeted
/// base names; a missing base requires refreshing the PR rather than guessing.
/// Historical SHA inputs are resolved separately by the caller.
pub(crate) fn resolve_base_rev(repo_path: &Path, base_ref: &str, cancel: &AtomicBool) -> Result<String, String> {
    check_cancel(cancel)?;
    fetch_base(repo_path, base_ref, cancel).map_err(|error| {
        if error == "cancelled" { return error; }
        format!("Não foi possível atualizar a base do MR `{base_ref}`: {error}. Recusando usar um ref antigo ou cair em {}. Atualize o MR e tente de novo.", remote_default_branch(repo_path, cancel).unwrap_or_else(|| "o branch padrão".into()))
    })?;
    let primary = remote_base(base_ref);
    resolve_rev_sha(repo_path, &primary, cancel)
        .ok_or_else(|| format!("PR base `{base_ref}` could not be pinned after fetch"))
}

pub(crate) fn looks_like_sha(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn cleanup_pr_ref(repo_path: &Path, number: i64) -> Result<(), String> {
    let cancel = AtomicBool::new(false);
    let _ = git_capture(
        repo_path,
        &["update-ref", "-d", &format!("refs/ocr/pr-{number}")],
        &cancel,
        15,
    );
    Ok(())
}

/// Files changed by the PR vs the indexed base branch (merge-base diff), local.
pub(crate) fn changed_files_local(repo_path: &Path, range: &str, cancel: &AtomicBool) -> Result<Vec<String>, String> {
    let output = git_capture(
        repo_path,
        &["diff", "--name-only", "--no-renames", "-z", range],
        cancel,
        60,
    )?;
    String::from_utf8(output)
        .map(|paths| {
            paths
                .split('\0')
                .filter(|path| !path.is_empty())
                .map(str::to_string)
                .collect()
        })
        .map_err(|error| format!("Git changed-file paths are not UTF-8: {error}"))
}

/// The paths in a `git diff --raw -z --no-renames` listing.
fn diff_paths(raw: &[u8]) -> Vec<String> {
    let fields = raw.split(|byte| *byte == 0).collect::<Vec<_>>();
    fields
        .windows(2)
        .filter(|pair| pair[0].first() == Some(&b':'))
        .map(|pair| String::from_utf8_lossy(pair[1]).into_owned())
        .collect()
}

/// Blobs a diff of `range` reads. Reading trees only, so it never triggers a lazy fetch.
pub(crate) fn diff_blob_ids(raw: &[u8]) -> Vec<String> {
    let mut ids = raw
        .split(|byte| *byte == 0)
        .filter_map(|field| std::str::from_utf8(field).ok()?.strip_prefix(':'))
        .flat_map(|meta| {
            meta.split_whitespace()
                .skip(2)
                .take(2)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|id| id.bytes().all(|b| b.is_ascii_hexdigit()) && id.bytes().any(|b| b != b'0'))
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// A blobless clone otherwise fetches every missing blob separately during the diff, which
/// makes large PRs exceed the diff deadline. Fetch the missing ones in batches first.
pub(crate) fn prefetch_diff_blobs(repo_path: &Path, range: &str, cancel: &AtomicBool) -> Result<(), String> {
    let raw = git_capture(
        repo_path,
        &["diff", "--raw", "--no-abbrev", "--no-renames", "-z", range],
        cancel,
        60,
    )?;
    let ids = diff_blob_ids(&raw);
    if ids.is_empty() {
        return Ok(());
    }
    // Docker's Git 2.39 aborts `cat-file --batch-check` on a missing promisor object and fails
    // `rev-list` on a missing tip, and listing every object overflows the capture budget on large
    // repositories. So walk only the diff's paths from the range's commits, which are always
    // present: `--missing=print` marks the absent blobs with `?` without fetching them.
    let (from, to) = range
        .split_once("...")
        .or_else(|| range.split_once(".."))
        .ok_or("Invalid diff range")?;
    let mut input = format!("{from}\n{to}\n");
    if range.contains("...") {
        let base = git_capture(repo_path, &["merge-base", from, to], cancel, 30)?;
        input.push_str(String::from_utf8_lossy(&base).trim());
        input.push('\n');
    }
    input.push_str("--\n");
    for path in diff_paths(&raw) {
        input.push_str(&path);
        input.push('\n');
    }
    let listed = crate::indexer::git_frozen_with_input(
        repo_path,
        &[
            "--literal-pathspecs",
            "rev-list",
            "--objects",
            "--no-walk",
            "--no-object-names",
            "--missing=print",
            "--stdin",
        ],
        cancel,
        input.as_bytes(),
    )?;
    let ids = ids.into_iter().collect::<std::collections::HashSet<_>>();
    let missing = String::from_utf8_lossy(&listed)
        .lines()
        .filter_map(|line| line.strip_prefix('?'))
        .filter(|id| ids.contains(*id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    for batch in missing.chunks(128) {
        let mut fetch = vec![
            "-c",
            "fetch.negotiationAlgorithm=noop",
            "fetch",
            "--no-tags",
            "--no-write-fetch-head",
            "--recurse-submodules=no",
            "--no-auto-maintenance",
            "origin",
        ];
        fetch.extend(batch.iter().map(String::as_str));
        crate::indexer::git(repo_path, &fetch, Some(cancel))?;
    }
    Ok(())
}

/// Unified diff for the given range (full PR `base...head`, or incremental `old..head`).
pub(crate) fn diff_local(repo_path: &Path, range: &str, cancel: &AtomicBool) -> Result<String, String> {
    prefetch_diff_blobs(repo_path, range, cancel)?;
    let output = git_capture(repo_path, &["diff", range], cancel, 90)?;
    Ok(String::from_utf8_lossy(&output).to_string())
}

/// The changed files of a range, a rename or copy counted once at its new path.
pub(crate) fn changed_file_targets_local(
    repo_path: &Path,
    range: &str,
    cancel: &AtomicBool,
) -> Result<Vec<String>, String> {
    let output = git_capture(
        repo_path,
        &["diff", "--name-status", "-z", "--find-renames", range],
        cancel,
        60,
    )?;
    let fields = output
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let mut targets = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let status = fields[index];
        index += 1;
        if index >= fields.len() {
            return Err("Git returned an incomplete changed-file inventory".into());
        }
        index += if status.first().is_some_and(|byte| matches!(byte, b'R' | b'C')) {
            2
        } else {
            1
        };
        if index > fields.len() {
            return Err("Git returned an incomplete changed-file inventory".into());
        }
        let target = std::str::from_utf8(fields[index - 1])
            .map_err(|error| format!("Git changed-file paths are not UTF-8: {error}"))?;
        targets.push(target.to_string());
    }
    Ok(targets)
}

/// Diff a range limited to specific paths (used by incremental re-review to show
/// only the author's new PR files, excluding main-merge noise). Empty paths → "".
pub(crate) fn diff_local_paths(repo_path: &Path, range: &str, paths: &[String], cancel: &AtomicBool) -> String {
    if paths.is_empty() {
        return String::new();
    }
    // Paths are names, not globs: `[key].ts` must not match `k.ts`.
    let literal = paths.iter().map(|path| format!(":(literal){path}")).collect::<Vec<_>>();
    let mut args: Vec<&str> = vec!["diff", range, "--"];
    args.extend(literal.iter().map(String::as_str));
    git_capture(repo_path, &args, cancel, 90)
        .map(|output| String::from_utf8_lossy(&output).to_string())
        .unwrap_or_default()
}

/// True if `ancestor` is an ancestor of `descendant` (so an incremental diff is valid).
pub(crate) fn is_ancestor(repo_path: &Path, ancestor: &str, descendant: &str, cancel: &AtomicBool) -> bool {
    git_capture(
        repo_path,
        &["merge-base", "--is-ancestor", ancestor, descendant],
        cancel,
        15,
    )
    .is_ok()
}

/// Read a file's content at a given ref/sha from the local git object DB.
pub(crate) fn show_blob(repo_path: &Path, reference: &str, path: &str, cancel: &AtomicBool) -> Option<String> {
    // `cat-file` reads exactly that blob and fails when it is absent; `show` prints nothing and
    // succeeds for a deleted path such as `[key].ts`.
    let output = git_capture(
        repo_path,
        &["cat-file", "blob", &format!("{reference}:{path}")],
        cancel,
        30,
    )
    .ok()?;
    String::from_utf8(output).ok()
}

pub(crate) const CONVENTION_FILES: &[&str] = &[
    "REVIEW.md",
    "AGENTS.md",
    "CONVENTIONS.md",
    "CONTRIBUTING.md",
    "CLAUDE.md",
    ".cursorrules",
    "README.md",
];

/// Root convention docs present at the pinned base, in `CONVENTION_FILES` order.
pub(crate) fn convention_files_at(repo_path: &Path, base: &str, cancel: &AtomicBool) -> Result<Vec<String>, String> {
    let mut args = vec!["ls-tree", "--name-only", "-z", base, "--"];
    args.extend_from_slice(CONVENTION_FILES);
    let output = git_capture(repo_path, &args, cancel, 30)?;
    let text = String::from_utf8_lossy(&output);
    let found: std::collections::HashSet<&str> = text.split('\0').filter(|name| !name.is_empty()).collect();
    Ok(CONVENTION_FILES
        .iter()
        .copied()
        .filter(|doc| found.contains(doc))
        .map(str::to_string)
        .collect())
}

pub(crate) fn materialize_files(
    repo_path: &Path,
    base_ref: &str,
    head_sha: &str,
    changed: &[String],
    plan: &ReviewContextPlan,
    context_dir: &Path,
    skill_content: &str,
    head_context: bool,
    cancel: &AtomicBool,
) -> Result<(), String> {
    // Changed files at the PR head (local, from the fetched ref).
    let changed_dir = context_dir.join("changed");
    for path in changed {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        if let Some(content) = show_blob(repo_path, head_sha, path, cancel) {
            write_under(&changed_dir, path, &content);
        }
    }
    // Related files from the base branch (local).
    let related_dir = context_dir.join("related");
    for file in &plan.related_files {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        if let Some(content) = show_blob(
            repo_path,
            if head_context { head_sha } else { base_ref },
            &file.path,
            cancel,
        ) {
            write_under(&related_dir, &file.path, &content);
        }
    }
    // OpenCodeReview's active review skill (built-in baseline, or a snapshot the user applied).
    let _ = fs::create_dir_all(context_dir.join("skill"));
    let skill_content = if head_context {
        crate::review::evidence::reasoning_skill(skill_content)
    } else {
        skill_content.to_string()
    };
    fs::write(context_dir.join("skill").join("SKILL.md"), skill_content).map_err(|error| error.to_string())?;
    // The repo's OWN convention docs (if any) from the base branch — context only.
    let conventions_dir = context_dir.join("conventions");
    for path in &plan.convention_files {
        if let Some(content) = show_blob(repo_path, base_ref, path, cancel) {
            write_under(&conventions_dir, path, &content);
        }
    }
    Ok(())
}

pub(crate) fn write_under(dir: &Path, rel: &str, content: &str) {
    let dest = dir.join(rel);
    if let Some(parent) = dest.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(dest, content);
}

/// Bounded `git -C <repo> <args>` capturing stdout to a temp file (safe for large
/// diffs), killed on cancel or timeout. Never blocks the caller indefinitely.
pub(crate) fn git_capture(
    repo_path: &Path,
    args: &[&str],
    cancel: &AtomicBool,
    timeout_secs: u64,
) -> Result<Vec<u8>, String> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = format!(
        "ocr-rgit-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let stdout_path = std::env::temp_dir().join(format!("{stamp}.out"));
    let stderr_path = std::env::temp_dir().join(format!("{stamp}.err"));
    let stdout_file = fs::File::create(&stdout_path).map_err(|error| error.to_string())?;
    let stderr_file = fs::File::create(&stderr_path).map_err(|error| error.to_string())?;
    let mut command = Command::new("git");
    command.env("GIT_NO_REPLACE_OBJECTS", "1").arg("-C").arg(repo_path);
    if args.first() == Some(&"diff") {
        command
            .arg("diff")
            .args(["--no-ext-diff", "--no-textconv", "--full-index"])
            .args(&args[1..]);
    } else {
        command.args(args);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    // Host-aware HTTPS auth shared with clone/fetch elsewhere, so review git
    // transports authenticate against the resolved (possibly Enterprise) host
    // rather than a duplicated, drift-prone local copy.
    crate::gitlab::apply_git_https_auth(&mut command);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let started = Instant::now();
    // Most git calls finish in a few milliseconds; the wait grows to 80ms only for long ones.
    let mut wait = Duration::from_millis(1);
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err("cancelled".to_string());
        }
        if started.elapsed() > Duration::from_secs(timeout_secs) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err(format!("git {} timed out after {timeout_secs}s", args.join(" ")));
        }
        if child.try_wait().map_err(|error| error.to_string())?.is_some() {
            break;
        }
        std::thread::sleep(wait);
        wait = (wait * 2).min(Duration::from_millis(80));
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    let stdout = fs::read(&stdout_path).unwrap_or_default();
    let stderr_bytes = fs::read(&stderr_path).unwrap_or_default();
    let _ = fs::remove_file(&stdout_path);
    let _ = fs::remove_file(&stderr_path);
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git failed with status {status}")
        } else {
            stderr
        });
    }
    Ok(stdout)
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
