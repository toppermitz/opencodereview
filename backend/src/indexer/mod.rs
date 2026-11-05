use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::db::{
    RepoIndexEdgeInput, RepoIndexFileInput, RepoIndexPackageInput, RepoIndexPkgDepInput, RepoIndexSymbolEdgeInput,
    RepoIndexSymbolInput, RepoIndexWrite,
};

mod imports;
mod manifests;
mod paths;
mod symbols;

pub(crate) use imports::*;
pub(crate) use manifests::*;
pub(crate) use paths::*;
pub(crate) use symbols::*;

const MAX_BLOB_READ_BYTES: i64 = 2_000_000;
const MAX_PARSE_BYTES: i64 = 250_000;
const PROGRESS_EVERY: usize = 64;

pub struct IndexProgress {
    pub stage: String,
    pub processed: i64,
    pub total: i64,
    pub file_count: i64,
    pub package_count: i64,
    pub edge_count: i64,
    pub skipped_count: i64,
    pub current_path: Option<String>,
}

struct TreeEntry {
    path: String,
    blob_sha: String,
    size: i64,
}

/// Index the repo default branch via git object access (no checkout).
/// Reports staged progress through `on_progress` and is cancellable.
pub fn run_index(
    repo_path: &Path,
    default_branch: Option<&str>,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(IndexProgress),
) -> Result<RepoIndexWrite, String> {
    let base_ref = default_branch
        .map(|branch| format!("origin/{branch}"))
        .unwrap_or_else(|| "HEAD".to_string());
    run_index_for_ref(repo_path, &base_ref, default_branch, cancel, on_progress)
}

pub fn run_index_for_ref(
    repo_path: &Path,
    base_ref: &str,
    fetch_branch: Option<&str>,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(IndexProgress),
) -> Result<RepoIndexWrite, String> {
    if !repo_path.is_dir() {
        return Err("repo path is not a directory".to_string());
    }
    let base_ref = base_ref.to_string();

    // Stage: fetch
    emit(on_progress, "fetching", 0, 0, 0, 0, 0, 0, None);
    if let Some(branch) = fetch_branch {
        git(
            repo_path,
            &[
                "fetch",
                "origin",
                &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
                "--quiet",
            ],
            Some(cancel),
        )?;
    }
    check_cancel(cancel)?;

    // Stage: resolve base sha + read tree
    emit(on_progress, "reading_tree", 0, 0, 0, 0, 0, 0, None);
    let base_sha = String::from_utf8(git(repo_path, &["rev-parse", &base_ref], Some(cancel))?)
        .map_err(|error| error.to_string())?
        .trim()
        .to_string();
    let tree_bytes = git(repo_path, &["ls-tree", "-r", "-z", &base_sha], Some(cancel))?;
    let mut entries = parse_tree(&tree_bytes);
    load_index_blob_sizes(repo_path, &base_sha, &mut entries, cancel, on_progress)?;
    let total = entries.len() as i64;

    // Stage: classify
    emit(on_progress, "classifying", 0, total, 0, 0, 0, 0, None);
    let mut files: Vec<RepoIndexFileInput> = Vec::with_capacity(entries.len());
    let mut skipped_count = 0i64;
    let mut manifest_shas: Vec<(String, String)> = Vec::new(); // (path, blob_sha)
    let mut tsconfig_shas: Vec<(String, String)> = Vec::new(); // (path, blob_sha)
    let file_path_set: HashSet<String> = entries.iter().map(|entry| entry.path.clone()).collect();

    for (processed, entry) in entries.iter().enumerate() {
        if processed % PROGRESS_EVERY == 0 {
            check_cancel(cancel)?;
            emit(
                on_progress,
                "classifying",
                processed as i64,
                total,
                files.len() as i64,
                0,
                0,
                skipped_count,
                Some(&entry.path),
            );
        }
        let language = language_for_path(&entry.path).map(str::to_string);
        let is_binary = is_binary_path(&entry.path);
        let is_generated = is_generated_path(&entry.path);
        if is_binary || is_generated {
            skipped_count += 1;
        }
        if is_package_manifest(&entry.path) {
            if entry.size > MAX_BLOB_READ_BYTES {
                return Err(format!("Package manifest {} exceeds the {MAX_BLOB_READ_BYTES}-byte indexing limit ({} bytes). Reduce the manifest and re-index; no partial index was published.", entry.path, entry.size));
            }
            manifest_shas.push((entry.path.clone(), entry.blob_sha.clone()));
        }
        if is_tsconfig(&entry.path) {
            tsconfig_shas.push((entry.path.clone(), entry.blob_sha.clone()));
        }
        files.push(RepoIndexFileInput {
            path: entry.path.clone(),
            language,
            size: entry.size,
            blob_sha: entry.blob_sha.clone(),
            package_path: None,
            is_binary,
            is_generated,
        });
    }

    // Stage: detect packages (read manifests via one cat-file batch)
    emit(
        on_progress,
        "detecting_packages",
        0,
        manifest_shas.len() as i64,
        files.len() as i64,
        0,
        0,
        skipped_count,
        None,
    );
    let manager = detect_manager(&file_path_set);
    let mut batch = BatchReader::spawn(repo_path)?;
    let mut packages: Vec<RepoIndexPackageInput> = Vec::new();
    let mut deps: Vec<RepoIndexPkgDepInput> = Vec::new();
    let mut package_names: HashSet<String> = HashSet::new();
    let mut pending_deps: Vec<(String, Vec<(String, bool)>)> = Vec::new();
    // Directories whose package name came from requirements.txt, which declares none.
    let mut unnamed: HashSet<String> = HashSet::new();

    for (processed, (path, sha)) in manifest_shas.iter().enumerate() {
        check_cancel_kill(cancel, &mut batch)?;
        let package_path = manifest_dir(path);
        let content = batch.read_blob(sha).unwrap_or_default();
        if let Some(parsed) = parse_manifest(path, &content) {
            // One package per directory (`setup.py` next to `requirements.txt`): every manifest
            // there adds its dependencies, and a declared name replaces the one requirements.txt
            // makes up, which sorts first.
            if let Some((_, existing)) = pending_deps.iter_mut().find(|(from, _)| *from == package_path) {
                existing.extend(parsed.deps);
                if !path.ends_with("requirements.txt") {
                    if let Some(package) = packages
                        .iter_mut()
                        .find(|package| package.package_path == package_path && unnamed.remove(&package_path))
                    {
                        package_names.remove(&package.name);
                        package_names.insert(parsed.name.clone());
                        package.name = parsed.name;
                    }
                }
                continue;
            }
            if path.ends_with("requirements.txt") {
                unnamed.insert(package_path.clone());
            }
            package_names.insert(parsed.name.clone());
            packages.push(RepoIndexPackageInput {
                package_path: package_path.clone(),
                name: parsed.name.clone(),
                kind: package_kind(&package_path).to_string(),
                manager: manager.clone(),
                source_roots_json: serde_json::to_string(&source_roots(&package_path, &file_path_set))
                    .unwrap_or_else(|_| "[]".to_string()),
                test_globs_json: "[\"**/*.test.*\",\"**/*.spec.*\",\"tests/**\"]".to_string(),
            });
            pending_deps.push((package_path.clone(), parsed.deps));
        }
        if processed % 8 == 0 {
            emit(
                on_progress,
                "detecting_packages",
                processed as i64,
                manifest_shas.len() as i64,
                files.len() as i64,
                packages.len() as i64,
                0,
                skipped_count,
                Some(path),
            );
        }
    }
    // internal deps only (point to workspace packages)
    for (from_path, dep_list) in pending_deps {
        let mut seen = HashSet::new();
        for (name, dev) in dep_list {
            if package_names.contains(&name) && seen.insert(name.clone()) {
                deps.push(RepoIndexPkgDepInput {
                    from_package_path: from_path.clone(),
                    to_package_name: name,
                    dev,
                });
            }
        }
    }

    // tsconfig path aliases (baseUrl + paths), read via the same batch process.
    // Stored per tsconfig dir so the nearest config governs each source file.
    let mut tsconfig_rules: Vec<(String, Vec<AliasRule>)> = Vec::new();
    for (path, sha) in &tsconfig_shas {
        check_cancel_kill(cancel, &mut batch)?;
        let content = batch.read_blob(sha).unwrap_or_default();
        let text = String::from_utf8_lossy(&content);
        if let Some(rules) = parse_tsconfig_aliases(&text, &config_dir(path)) {
            if !rules.is_empty() {
                tsconfig_rules.push((config_dir(path), rules));
            }
        }
    }
    tsconfig_rules.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.len()));

    // assign package_path to each file (longest matching package dir)
    let mut package_paths: Vec<String> = packages.iter().map(|package| package.package_path.clone()).collect();
    package_paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
    for file in files.iter_mut() {
        file.package_path = owning_package(&file.path, &package_paths);
    }

    // Stage: parse imports for source files (same batch process)
    let source_indexes: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| {
            !file.is_binary
                && !file.is_generated
                && file.language.as_deref().map(is_code_language).unwrap_or(false)
                && file.size > 0
                && file.size <= MAX_PARSE_BYTES
        })
        .map(|(index, _)| index)
        .collect();
    let source_total = source_indexes.len() as i64;
    let mut edges: Vec<RepoIndexEdgeInput> = Vec::new();
    let mut symbols: Vec<RepoIndexSymbolInput> = Vec::new();
    // Called-symbol-name candidates per symbol (parallel to `symbols`), used to
    // build the symbol-level call/reference graph after all symbols are known.
    let mut symbol_refs: Vec<Vec<String>> = Vec::new();
    for (processed, index) in source_indexes.iter().enumerate() {
        if processed % PROGRESS_EVERY == 0 {
            check_cancel_kill(cancel, &mut batch)?;
            emit(
                on_progress,
                "parsing_imports",
                processed as i64,
                source_total,
                files.len() as i64,
                packages.len() as i64,
                edges.len() as i64,
                skipped_count,
                Some(&files[*index].path),
            );
        }
        let file = &files[*index];
        let content = match batch.read_blob(&file.blob_sha) {
            Some(bytes) => bytes,
            None => continue,
        };
        let text = String::from_utf8_lossy(&content);
        let file_symbols = extract_symbols(file, &text);
        if file.language.as_deref() == Some("python") {
            let lines: Vec<&str> = text.lines().collect();
            for symbol in &file_symbols {
                symbol_refs.push(python_symbol_calls(
                    &lines,
                    symbol.start_line,
                    symbol.end_line,
                    &symbol.name,
                ));
            }
        } else {
            for _ in &file_symbols {
                symbol_refs.push(Vec::new());
            }
        }
        symbols.extend(file_symbols);
        let alias_rules = nearest_alias_rules(&file.path, &tsconfig_rules);
        for (spec, kind) in extract_imports(&file.path, file.language.as_deref(), &text) {
            let (resolved, to_path) = resolve_spec(
                &file.path,
                file.language.as_deref(),
                &spec,
                &file_path_set,
                &package_names,
                alias_rules,
            );
            edges.push(RepoIndexEdgeInput {
                from_path: file.path.clone(),
                to_ref: spec,
                kind,
                resolved,
                to_path,
            });
        }
    }
    batch.finish();
    let symbol_edges = build_symbol_edges(&symbols, &symbol_refs);

    // Stage: write (handled by caller in a transaction)
    emit(
        on_progress,
        "writing",
        source_total,
        source_total,
        files.len() as i64,
        packages.len() as i64,
        edges.len() as i64,
        skipped_count,
        None,
    );

    Ok(RepoIndexWrite {
        base_ref,
        base_sha,
        files,
        packages,
        deps,
        edges,
        symbols,
        symbol_edges,
        skipped_count,
        total_count: total,
        duration_ms: 0,
    })
}

pub fn remote_default_sha(repo_path: &Path, default_branch: Option<&str>) -> Result<String, String> {
    let branch = default_branch.ok_or("repo default branch is unknown")?;
    let output = String::from_utf8(git(
        repo_path,
        &["ls-remote", "origin", &format!("refs/heads/{branch}")],
        None,
    )?)
    .map_err(|error| error.to_string())?;
    output
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or("could not resolve remote default branch".to_string())
}

#[allow(clippy::too_many_arguments)]
fn emit(
    on_progress: &mut dyn FnMut(IndexProgress),
    stage: &str,
    processed: i64,
    total: i64,
    file_count: i64,
    package_count: i64,
    edge_count: i64,
    skipped_count: i64,
    current_path: Option<&str>,
) {
    on_progress(IndexProgress {
        stage: stage.to_string(),
        processed,
        total,
        file_count,
        package_count,
        edge_count,
        skipped_count,
        current_path: current_path.map(str::to_string),
    });
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("cancelled".to_string())
    } else {
        Ok(())
    }
}

fn check_cancel_kill(cancel: &AtomicBool, batch: &mut BatchReader) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        batch.finish();
        return Err("cancelled".to_string());
    }
    Ok(())
}

fn load_index_blob_sizes(
    repo_path: &Path,
    base_sha: &str,
    entries: &mut [TreeEntry],
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(IndexProgress),
) -> Result<(), String> {
    let candidates: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            !is_binary_path(&entry.path)
                && (!is_generated_path(&entry.path) || is_package_manifest(&entry.path))
                && (is_fts_eligible_path(&entry.path)
                    || language_for_path(&entry.path).is_some_and(is_code_language)
                    || is_package_manifest(&entry.path)
                    || is_tsconfig(&entry.path))
        })
        .map(|(index, _)| index)
        .collect();
    let inventory = git(
        repo_path,
        &[
            "rev-list",
            "--objects",
            "--no-object-names",
            "--missing=print",
            "--no-walk",
            base_sha,
        ],
        Some(cancel),
    )?;
    let inventory = String::from_utf8_lossy(&inventory);
    let mut missing_objects: HashSet<&str> = inventory.lines().filter_map(|line| line.strip_prefix('?')).collect();
    for (batch_number, indexes) in candidates.chunks(128).enumerate() {
        check_cancel(cancel)?;
        emit(
            on_progress,
            "reading_tree",
            (batch_number * 128) as i64,
            candidates.len() as i64,
            0,
            0,
            0,
            0,
            Some(&entries[indexes[0]].path),
        );
        let input = indexes
            .iter()
            .map(|index| format!("{}\n", entries[*index].blob_sha))
            .collect::<String>();
        let args = ["cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)"];
        let missing: Vec<String> = indexes
            .iter()
            .filter(|index| missing_objects.remove(entries[**index].blob_sha.as_str()))
            .map(|index| entries[*index].blob_sha.clone())
            .collect();
        if !missing.is_empty() {
            let mut fetch = vec![
                "-c",
                "fetch.negotiationAlgorithm=noop",
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                "--recurse-submodules=no",
                "origin",
            ];
            fetch.extend(missing.iter().map(String::as_str));
            git(repo_path, &fetch, Some(cancel))?;
        }
        let sizes = git_with_input(repo_path, &args, Some(cancel), input.as_bytes())?;
        let sizes = String::from_utf8_lossy(&sizes);
        let lines: Vec<_> = sizes.lines().collect();
        if lines.len() != indexes.len() {
            return Err("Incomplete blob metadata from git".to_string());
        }
        for (index, line) in indexes.iter().zip(lines) {
            let parts: Vec<_> = line.split_whitespace().collect();
            let size = parts
                .get(2)
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|size| *size >= 0)
                .ok_or_else(|| format!("Could not load indexed blob {}", entries[*index].path))?;
            entries[*index].size = size;
        }
    }
    Ok(())
}

fn parse_tree(tree: &[u8]) -> Vec<TreeEntry> {
    let mut entries = Vec::new();
    for record in tree.split(|byte| *byte == 0).filter(|record| !record.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let Some((meta, path)) = record.split_once('\t') else {
            continue;
        };
        let parts: Vec<&str> = meta.split_whitespace().collect();
        if parts.len() < 3 || parts[1] != "blob" {
            continue;
        }
        entries.push(TreeEntry {
            path: path.to_string(),
            blob_sha: parts[2].to_string(),
            size: parts.get(3).and_then(|size| size.parse().ok()).unwrap_or(-1),
        });
    }
    entries
}

pub(crate) struct ParsedManifest {
    name: String,
    deps: Vec<(String, bool)>,
}

pub(crate) struct ParsedSymbol {
    name: String,
    kind: String,
    start_line: i64,
    end_line: i64,
    signature: Option<String>,
    is_exported: bool,
}

/// A tsconfig `paths` alias rule, with targets already resolved to repo-root-relative dirs.
#[derive(Debug, Clone)]
pub(crate) struct AliasRule {
    /// Specifier prefix before `*` (e.g. "@/" for "@/*"), or the full pattern when no `*`.
    prefix: String,
    has_star: bool,
    /// Target templates rooted at repo root, prefix before `*` for star rules.
    targets: Vec<String>,
}

/// One long-lived `git cat-file --batch` process. Avoids spawning a process per file.
struct BatchReader {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
}

impl BatchReader {
    fn spawn(repo_path: &Path) -> Result<Self, String> {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(repo_path)
            .args(["cat-file", "--batch"])
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        apply_github_auth(&mut command);
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout from git cat-file")?);
        Ok(Self { child, stdin, stdout })
    }

    /// Request one object and read its full content. Returns None if missing.
    fn read_blob(&mut self, sha: &str) -> Option<Vec<u8>> {
        let stdin = self.stdin.as_mut()?;
        stdin.write_all(sha.as_bytes()).ok()?;
        stdin.write_all(b"\n").ok()?;
        stdin.flush().ok()?;

        let mut header = Vec::new();
        self.stdout.read_until(b'\n', &mut header).ok()?;
        let header = String::from_utf8_lossy(&header);
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 3 || parts[1] == "missing" {
            return None;
        }
        let size: usize = parts[2].parse().ok()?;
        if size > MAX_BLOB_READ_BYTES as usize {
            std::io::copy(
                &mut self.stdout.by_ref().take(size.checked_add(1)? as u64),
                &mut std::io::sink(),
            )
            .ok()?;
            return None;
        }
        let mut content = vec![0u8; size + 1]; // content + trailing newline
        self.stdout.read_exact(&mut content).ok()?;
        content.truncate(size);
        Some(content)
    }

    fn finish(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for BatchReader {
    fn drop(&mut self) {
        self.finish();
    }
}

fn apply_github_auth(command: &mut Command) {
    crate::gitlab::apply_git_https_auth(command);
}

pub(crate) fn git(repo_path: &Path, args: &[&str], cancel: Option<&AtomicBool>) -> Result<Vec<u8>, String> {
    git_with_input(repo_path, args, cancel, &[])
}

struct GitTempFiles(Vec<std::path::PathBuf>);

impl Drop for GitTempFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn git_with_input(
    repo_path: &Path,
    args: &[&str],
    cancel: Option<&AtomicBool>,
    input: &[u8],
) -> Result<Vec<u8>, String> {
    git_command_with_input(repo_path, args, cancel, input, false)
}

pub(crate) fn git_frozen_with_input(
    repo_path: &Path,
    args: &[&str],
    cancel: &AtomicBool,
    input: &[u8],
) -> Result<Vec<u8>, String> {
    git_command_with_input(repo_path, args, Some(cancel), input, true)
}

fn git_command_with_input(
    repo_path: &Path,
    args: &[&str],
    cancel: Option<&AtomicBool>,
    input: &[u8],
    frozen: bool,
) -> Result<Vec<u8>, String> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = format!(
        "ocr-git-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let stdout_path = std::env::temp_dir().join(format!("{stamp}.out"));
    let stderr_path = std::env::temp_dir().join(format!("{stamp}.err"));
    let stdin_path = std::env::temp_dir().join(format!("{stamp}.in"));
    let _files = GitTempFiles(vec![stdin_path.clone(), stdout_path.clone(), stderr_path.clone()]);
    fs::write(&stdin_path, input).map_err(|error| error.to_string())?;
    let stdin_file = File::open(&stdin_path).map_err(|error| error.to_string())?;
    let stdout_file = File::create(&stdout_path).map_err(|error| error.to_string())?;
    let stderr_file = File::create(&stderr_path).map_err(|error| error.to_string())?;
    let mut command = Command::new("git");
    command
        .stdin(Stdio::from(stdin_file))
        .env("GIT_NO_REPLACE_OBJECTS", "1");
    if frozen || args.first() == Some(&"cat-file") {
        command.env("GIT_NO_LAZY_FETCH", "1");
    }
    if frozen {
        command
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    if !frozen {
        apply_github_auth(&mut command);
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let started = Instant::now();
    let oversized = || {
        frozen
            && [&stdout_path, &stderr_path]
                .iter()
                .any(|path| fs::metadata(path).is_ok_and(|meta| meta.len() > 16 * 1024 * 1024))
    };
    // Most git calls finish in a few milliseconds; the wait grows to 50ms only for long ones.
    let mut wait = Duration::from_millis(1);
    loop {
        if oversized() {
            crate::review::kill_tree(child.id(), &mut child);
            return Err("Frozen Git capture exceeds its byte budget".into());
        }
        if cancel.map(|flag| flag.load(Ordering::Relaxed)).unwrap_or(false) {
            crate::review::kill_tree(child.id(), &mut child);
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err("cancelled".to_string());
        }
        if started.elapsed() > Duration::from_secs(120) {
            crate::review::kill_tree(child.id(), &mut child);
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err(format!("git {} timed out", args.join(" ")));
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(error) => {
                crate::review::kill_tree(child.id(), &mut child);
                return Err(error.to_string());
            }
        }
        std::thread::sleep(wait);
        wait = (wait * 2).min(Duration::from_millis(50));
    }
    let status = child.wait().map_err(|error| error.to_string())?;
    if oversized() {
        return Err("Frozen Git capture exceeds its byte budget".into());
    }
    let stdout = fs::read(&stdout_path).map_err(|error| error.to_string())?;
    let stderr_bytes = fs::read(&stderr_path).map_err(|error| error.to_string())?;
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
mod tests;
