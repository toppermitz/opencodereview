use crate::review_archive::{self, Archive, DependencyProvenance, ReferenceKind, Scope};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredLink {
    pub url: String,
    pub repository: String,
    pub reference: String,
    pub path: String,
    pub kind: ReferenceKind,
    pub commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RawDiscoveredLink {
    Sha {
        url: String,
        repository: String,
        sha: String,
        path: String,
    },
    Named {
        url: String,
        repository: String,
        candidate_splits: Vec<(String, String)>,
    },
}

impl RawDiscoveredLink {
    fn repository(&self) -> &str {
        match self {
            Self::Sha { repository, .. } => repository,
            Self::Named { repository, .. } => repository,
        }
    }

    /// Whether every path the link can name is a generated, lockfile or binary path the evidence
    /// policy leaves out. A named link qualifies only when all its reference splits do.
    fn only_unreviewable_paths(&self) -> bool {
        let unreviewable = |path: &str| {
            matches!(
                review_archive::path_exclusion(path),
                Ok(Some(
                    review_archive::Exclusion::Generated | review_archive::Exclusion::BinaryPath
                ))
            )
        };
        match self {
            Self::Sha { path, .. } => unreviewable(path),
            Self::Named { candidate_splits, .. } => {
                !candidate_splits.is_empty() && candidate_splits.iter().all(|(_, path)| unreviewable(path))
            }
        }
    }
}

fn is_valid_repo_name(owner: &str, repo: &str) -> bool {
    if owner.is_empty()
        || repo.is_empty()
        || owner.len() > 100
        || repo.len() > 100
        || matches!(owner, "." | "..")
        || matches!(repo, "." | "..")
    {
        return false;
    }
    owner.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && repo.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn is_valid_git_ref_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 256 {
        return false;
    }
    if name.starts_with('.') || name.ends_with('.') || name.ends_with(".lock") || name.ends_with('/') {
        return false;
    }
    if name.contains("..") || name.contains("//") || name.contains("@{") {
        return false;
    }
    for ch in name.chars() {
        if ch.is_ascii_control()
            || matches!(
                ch,
                ' ' | '~'
                    | '^'
                    | ':'
                    | '?'
                    | '*'
                    | '['
                    | '\\'
                    | ';'
                    | '&'
                    | '|'
                    | '$'
                    | '`'
                    | '"'
                    | '\''
                    | '<'
                    | '>'
                    | '('
                    | ')'
            )
        {
            return false;
        }
    }
    true
}

fn oid(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn scope(host: &str, repository: &str, revision: &str) -> Result<Scope, String> {
    let scope = Scope {
        host: host.into(),
        repository: repository.into(),
        head_sha: revision.into(),
        base_sha: revision.into(),
        merge_base_sha: revision.into(),
        prior_sha: None,
    };
    scope.validate().map_err(|error| error.to_string())?;
    Ok(scope)
}

fn repository_url(url: &str, host: &str) -> Option<String> {
    let repository = url
        .strip_prefix(&format!("https://{host}/"))
        .or_else(|| url.strip_prefix(&format!("git@{host}:")))?;
    let repository = repository.strip_suffix(".git").unwrap_or(repository);
    scope(host, repository, &"a".repeat(40)).ok()?;
    Some(repository.into())
}

pub(crate) fn sanitize_url(url: &str) -> String {
    let clean = url.split(['?', '#']).next().unwrap_or(url);
    if clean.len() > 160 {
        let mut end = 157;
        while end > 0 && !clean.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &clean[..end])
    } else {
        clean.to_string()
    }
}

// The job runner recognizes cancellation by its exact error value. Other errors
// reach persisted review history, so retain only caller-owned context, never stderr.
fn dependency_git_error(error: &str, context: &str, cancel: &AtomicBool) -> String {
    if error == "cancelled" || cancel.load(Ordering::Relaxed) {
        "cancelled".into()
    } else {
        format!("{context}: Git command failed")
    }
}

#[cfg(test)]
pub(crate) fn extract_raw_links(text: &str, host: &str) -> Result<Vec<RawDiscoveredLink>, String> {
    extract_raw_links_with_cancel(text, host, None, &AtomicBool::new(false))
}

pub(crate) fn extract_raw_links_with_cancel(
    text: &str,
    host: &str,
    primary_repository: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Vec<RawDiscoveredLink>, String> {
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    let host_prefix = format!("{host}/");
    for (start, _) in text.match_indices("https://") {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let suffix = &text[start + "https://".len()..];
        let url = suffix
            .split(|ch: char| ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']'))
            .next()
            .unwrap_or_default();
        let explicit = matches!(
            text[..start].chars().next_back(),
            Some('`' | '"' | '\'' | '<' | '(' | '[')
        );
        let url = if explicit {
            url
        } else {
            url.trim_end_matches(['.', ',', ';', ':', '!'])
        };
        let Some(path) = url.strip_prefix(&host_prefix) else {
            continue;
        };
        let parts = path.splitn(4, '/').collect::<Vec<_>>();
        if parts.len() < 3 || parts[2] != "blob" {
            continue;
        }
        // Primary-repository links are not dependency evidence, even if their URL
        // syntax is unsupported for external dependencies.
        if primary_repository.is_some_and(|primary| primary.eq_ignore_ascii_case(&format!("{}/{}", parts[0], parts[1])))
        {
            continue;
        }
        if url.len() > 1024 {
            let mut end = 157.min(url.len());
            while end > 0 && !url.is_char_boundary(end) {
                end -= 1;
            }
            let truncated = format!("https://{}...", &url[..end]);
            return Err(format!(
                "Pinned dependency link {} exceeds the supported length limit",
                sanitize_url(&truncated)
            ));
        }
        // Input bytes are bounded by the PR-text/archive budgets. Bound retained
        // links separately so duplicate occurrences do not consume selection space.
        if !seen.insert(url) {
            continue;
        }
        if seen.len() > 128 {
            return Err("Pinned dependency references exceed the discovery budget".into());
        }
        let full_url = format!("https://{url}");
        let owner = parts[0];
        let repo_name = parts[1];
        if !is_valid_repo_name(owner, repo_name) {
            return Err(format!(
                "Pinned dependency link {} has invalid repository format",
                sanitize_url(&full_url)
            ));
        }
        let repository = format!("{owner}/{repo_name}");
        if parts.len() < 4 {
            return Err(format!(
                "Pinned dependency link {} does not specify a reference or file path",
                sanitize_url(&full_url)
            ));
        }
        let rest = parts[3].split('#').next().unwrap_or_default();
        if rest.is_empty() {
            return Err(format!(
                "Pinned dependency link {} does not specify a reference or file path",
                sanitize_url(&full_url)
            ));
        }
        if rest.contains('%') || rest.contains('?') {
            return Err(format!(
                "Pinned dependency link {} contains an unsupported encoding or query",
                sanitize_url(&full_url)
            ));
        }
        let segments = rest.split('/').collect::<Vec<_>>();
        if segments.len() < 2 {
            return Err(format!(
                "Pinned dependency link {} does not specify a file path",
                sanitize_url(&full_url)
            ));
        }
        if (segments[0].len() == 40 || segments[0].len() == 64) && oid(segments[0], segments[0].len()) {
            let file_path = segments[1..].join("/");
            let sanitized_path = if file_path.len() > 160 {
                let mut end = 157;
                while end > 0 && !file_path.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &file_path[..end])
            } else {
                file_path.clone()
            };
            review_archive::safe_path(&file_path).map_err(|_| {
                format!(
                    "Pinned dependency link {} contains an invalid path: {sanitized_path}",
                    sanitize_url(&full_url)
                )
            })?;
            output.push(RawDiscoveredLink::Sha {
                url: full_url,
                repository,
                sha: segments[0].into(),
                path: file_path,
            });
        } else {
            if segments.len() > 17 {
                return Err(format!(
                    "Pinned dependency link {} exceeds the candidate interpretation budget",
                    sanitize_url(&full_url)
                ));
            }
            let mut candidate_splits = Vec::new();
            for i in 1..segments.len() {
                if cancel.load(Ordering::Relaxed) {
                    return Err("cancelled".into());
                }
                let ref_len: usize = segments[..i].iter().map(|s| s.len()).sum::<usize>() + (i - 1);
                let path_len: usize = segments[i..].iter().map(|s| s.len()).sum::<usize>() + (segments.len() - i - 1);
                if ref_len > 256 || path_len > 512 || ref_len == 0 || path_len == 0 {
                    continue;
                }
                let candidate_ref = segments[..i].join("/");
                let candidate_path = segments[i..].join("/");
                if is_valid_git_ref_name(&candidate_ref) && review_archive::safe_path(&candidate_path).is_ok() {
                    candidate_splits.push((candidate_ref, candidate_path));
                }
            }
            if candidate_splits.len() > 16 {
                return Err(format!(
                    "Pinned dependency link {} exceeds the candidate interpretation budget",
                    sanitize_url(&full_url)
                ));
            }
            if candidate_splits.is_empty() {
                return Err(format!(
                    "Pinned dependency link {} has invalid reference name or file path",
                    sanitize_url(&full_url)
                ));
            }
            output.push(RawDiscoveredLink::Named {
                url: full_url,
                repository,
                candidate_splits,
            });
        }
    }
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResolvedRef {
    Found { commit: String, kind: ReferenceKind },
    Collision,
    NotFound,
}

fn resolve_named_ref(
    repo_path: &Path,
    repository: &str,
    host: &str,
    ref_name: &str,
    cancel: &AtomicBool,
) -> Result<ResolvedRef, String> {
    let origin = crate::indexer::git(repo_path, &["remote", "get-url", "origin"], Some(cancel)).map_err(|e| {
        dependency_git_error(
            &e,
            &format!("Pinned dependency repository {repository} origin is unavailable"),
            cancel,
        )
    })?;
    let origin_str = std::str::from_utf8(&origin)
        .map_err(|_| format!("Pinned dependency repository {repository} origin is invalid UTF-8"))?;
    if repository_url(origin_str.trim(), host).as_deref() != Some(repository) {
        return Err("Dependency repository origin does not match the authorized GitHub repository".into());
    }
    let out = crate::indexer::git(
        repo_path,
        &[
            "ls-remote",
            "origin",
            &format!("refs/heads/{ref_name}"),
            &format!("refs/tags/{ref_name}"),
            &format!("refs/tags/{ref_name}^{{}}"),
        ],
        Some(cancel),
    )
    .map_err(|e| {
        dependency_git_error(
            &e,
            &format!("Failed to resolve reference '{ref_name}' in repository '{repository}'"),
            cancel,
        )
    })?;
    let text = String::from_utf8_lossy(&out);
    let mut branch_sha = None;
    let mut tag_peeled = None;
    let mut tag_sha = None;
    for line in text.lines() {
        let Some((sha, ref_spec)) = line.split_once('\t') else {
            continue;
        };
        let sha = sha.trim();
        if ref_spec == format!("refs/heads/{ref_name}") {
            branch_sha = Some(sha.to_string());
        } else if ref_spec == format!("refs/tags/{ref_name}^{{}}") {
            tag_peeled = Some(sha.to_string());
        } else if ref_spec == format!("refs/tags/{ref_name}") {
            tag_sha = Some(sha.to_string());
        }
    }
    if branch_sha.is_some() && (tag_peeled.is_some() || tag_sha.is_some()) {
        return Ok(ResolvedRef::Collision);
    }
    if let Some(sha) = branch_sha {
        return Ok(ResolvedRef::Found {
            commit: sha,
            kind: ReferenceKind::Branch,
        });
    }
    if let Some(sha) = tag_peeled {
        return Ok(ResolvedRef::Found {
            commit: sha,
            kind: ReferenceKind::Tag,
        });
    }
    if let Some(sha) = tag_sha {
        return Ok(ResolvedRef::Found {
            commit: sha,
            kind: ReferenceKind::Tag,
        });
    }
    Ok(ResolvedRef::NotFound)
}

// A contents API request checks the current gh session even when objects are cached.
// Monitoring subscriptions do not grant or deny supporting repository access.
fn verify_repository_read_access(repository: &str, host: &str, cancel: &AtomicBool) -> Result<(), String> {
    verify_repository_read_access_using(repository, host, cancel, |args| {
        crate::gitlab::glab_capture_cancellable(args, cancel)
    })
}

fn verify_repository_read_access_using(
    repository: &str,
    host: &str,
    cancel: &AtomicBool,
    mut request: impl FnMut(&[&str]) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let project = crate::gitlab::project_api_id(repository);
    let route = format!("projects/{project}/repository/commits?per_page=1");
    let bytes = request(&["api", &route]).map_err(|error| {
        dependency_git_error(
            &error,
            &format!("Could not verify authenticated glab read access for pinned dependency {repository}. Check GitLab access or retry"),
            cancel,
        )
    })?;
    let _ = host; // host is applied via GITLAB_HOST on every glab invocation
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid GitLab dependency access response")?;
    let commit = value
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim();
    if !oid(commit, 40) && !oid(commit, 64) {
        return Err(format!(
            "Could not verify authenticated glab read access for pinned dependency {repository}"
        ));
    }
    Ok(())
}

fn repository_is_public_using(
    repository: &str,
    host: &str,
    cancel: &AtomicBool,
    request: &mut impl FnMut(&[&str]) -> Result<Vec<u8>, String>,
) -> Result<bool, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let _ = host;
    let project = crate::gitlab::project_api_id(repository);
    let route = format!("projects/{project}");
    let bytes = request(&["api", &route]).map_err(|error| {
        dependency_git_error(
            &error,
            &format!("Could not verify the visibility of {repository}. Check GitLab access or retry"),
            cancel,
        )
    })?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| format!("Could not verify the visibility of {repository}"))?;
    match value.get("visibility").and_then(|v| v.as_str()) {
        Some("public") => Ok(true),
        Some(_) => Ok(false),
        None => Err(format!("Could not verify the visibility of {repository}")),
    }
}

// Linked repositories are read with the operator's gh session, so their code can reach a pull
// request whose readers lack access to it. A private or internal dependency is accepted only for
// a private or internal pull request repository of the same owner.
fn authorize_dependency_disclosure_using(
    primary: &str,
    primary_public: &mut Option<bool>,
    repository: &str,
    host: &str,
    cancel: &AtomicBool,
    mut request: impl FnMut(&[&str]) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    if repository_is_public_using(repository, host, cancel, &mut request)? {
        return Ok(());
    }
    let public = match *primary_public {
        Some(public) => public,
        None => *primary_public.insert(repository_is_public_using(primary, host, cancel, &mut request)?),
    };
    let owner = |name: &str| name.split('/').next().unwrap_or_default().to_ascii_lowercase();
    let reason = if public {
        "it is a public repository"
    } else if owner(primary) != owner(repository) {
        "it belongs to another owner"
    } else {
        return Ok(());
    };
    Err(format!(
        "Linked repository {repository} is private or internal, and its code cannot be quoted in a review of {primary} because {reason}. Remove the link from the PR description or changed Markdown files (or the submodule) to review this pull request."
    ))
}

fn ensure_dependency_repository(
    root: &Path,
    repository: &str,
    host: &str,
    network: bool,
    prepared: &BTreeSet<String>,
    cancel: &AtomicBool,
    read_access: &mut dyn FnMut(&str, &str, &AtomicBool) -> Result<(), String>,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    if network {
        read_access(repository, host, cancel)?;
    } else if !prepared.contains(repository) {
        return Err(format!("Pinned dependency {repository} is not authorized for offline preparation. Hydrate it through the authenticated gh session first."));
    }
    let path = root.join(repository);
    if !path.join(".git").is_dir() {
        if !network {
            return Err(format!("Pinned dependency repository {repository} is unavailable. Hydrate it through the authenticated gh session before offline preparation."));
        }
        if path.exists() {
            return Err(format!(
                "Pinned dependency repository cache {repository} is not a Git repository"
            ));
        }
        let parent = path.parent().ok_or("Dependency repository parent unavailable")?;
        std::fs::create_dir_all(parent).map_err(|_| "Dependency repository cache unavailable")?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Dependency repository cache clock unavailable")?
            .as_nanos();
        let staging = parent.join(format!(
            ".dependency-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&staging).map_err(|_| "Dependency repository cache unavailable")?;
        let result: Result<(), String> = (|| {
            // Initialize only an object cache: do not checkout files or register a subscription.
            crate::indexer::git(
                &staging,
                &["init", "--template=", "--initial-branch=dependency-cache"],
                Some(cancel),
            )?;
            crate::indexer::git(
                &staging,
                &["remote", "add", "origin", &format!("https://{host}/{repository}.git")],
                Some(cancel),
            )?;
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            if std::fs::rename(&staging, &path).is_err() && !path.join(".git").is_dir() {
                return Err("Dependency repository cache installation failed".into());
            }
            Ok(())
        })();
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        result.map_err(|error| {
            dependency_git_error(
                &error,
                &format!("Could not prepare pinned dependency repository {repository}"),
                cancel,
            )
        })?;
    }
    let origin = crate::indexer::git(&path, &["remote", "get-url", "origin"], Some(cancel)).map_err(|error| {
        dependency_git_error(
            &error,
            &format!("Pinned dependency repository {repository} origin is unavailable"),
            cancel,
        )
    })?;
    if repository_url(
        std::str::from_utf8(&origin)
            .map_err(|_| "Invalid dependency repository origin")?
            .trim(),
        host,
    )
    .as_deref()
        != Some(repository)
    {
        return Err("Dependency repository origin does not match the authorized GitHub repository".into());
    }
    Ok(())
}

fn module_repositories(
    repo: &Path,
    text: &str,
    host: &str,
    cancel: &AtomicBool,
) -> Result<BTreeMap<String, String>, String> {
    let raw = crate::indexer::git_frozen_with_input(
        repo,
        &["config", "--no-includes", "--null", "--list", "--file", "-"],
        cancel,
        text.as_bytes(),
    )?;
    let raw = String::from_utf8(raw).map_err(|_| "Invalid submodule configuration")?;
    let mut modules = BTreeMap::<String, BTreeMap<String, String>>::new();
    for entry in raw.split('\0').filter(|entry| !entry.is_empty()) {
        let (key, value) = entry.split_once('\n').ok_or("Invalid submodule configuration")?;
        let Some(key) = key.strip_prefix("submodule.") else {
            continue;
        };
        let Some((name, field)) = key.rsplit_once('.') else {
            continue;
        };
        if matches!(field, "path" | "url")
            && modules
                .entry(name.into())
                .or_default()
                .insert(field.into(), value.into())
                .is_some()
        {
            return Err("Ambiguous submodule configuration".into());
        }
    }
    let mut result = BTreeMap::new();
    for module in modules.values() {
        let path = module.get("path").ok_or("Submodule path unavailable")?;
        review_archive::safe_path(path).map_err(|error| error.to_string())?;
        let repository = module
            .get("url")
            .and_then(|url| repository_url(url, host))
            .ok_or("Submodule must use an explicit repository on the configured GitHub host")?;
        if result.insert(path.clone(), repository).is_some() {
            return Err("Duplicate submodule path".into());
        }
    }
    Ok(result)
}

#[derive(Debug, PartialEq, Eq)]
struct CaptureRequest {
    repository: String,
    revision: String,
    paths: Option<BTreeSet<String>>,
    provenance: Vec<DependencyProvenance>,
}

// Mask Markdown examples only in the discovery input. Frozen review evidence remains intact.
fn extract_markdown_links(
    text: &str,
    host: &str,
    primary_repository: Option<&str>,
    cancel: &AtomicBool,
) -> Result<Vec<RawDiscoveredLink>, String> {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let mut prose = text.as_bytes().to_vec();
    let mut in_code_block = false;
    for (event, range) in Parser::new(text).into_offset_iter() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let is_code = match event {
            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
                true
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                true
            }
            Event::Code(_) => true,
            _ => in_code_block,
        };
        if is_code {
            prose[range].fill(b' ');
        }
    }
    let prose = String::from_utf8(prose).map_err(|_| "Invalid Markdown discovery input".to_string())?;
    extract_raw_links_with_cancel(&prose, host, primary_repository, cancel)
}

fn is_markdown_document(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown"))
}

fn discover(
    archive: &Archive,
    repo: &Path,
    changed: &[String],
    pr_body: &str,
    authorized: &BTreeSet<String>,
    root: &Path,
    network: bool,
    recorded: Option<&[DependencyProvenance]>,
    cancel: &AtomicBool,
    read_access: &mut dyn FnMut(&str, &str, &AtomicBool) -> Result<(), String>,
) -> Result<Vec<CaptureRequest>, String> {
    let host = &archive.scope().host;
    if pr_body.len() as u64 > review_archive::MAX_FILE_BYTES {
        return Err("PR text exceeds the dependency discovery budget".into());
    }
    let mut raw_links: BTreeSet<_> = extract_markdown_links(pr_body, host, Some(&archive.scope().repository), cancel)?
        .into_iter()
        .collect();
    for entry in archive.entries().iter().filter(|entry| {
        changed.contains(&entry.source.path) && entry.exclusion.is_none() && is_markdown_document(&entry.source.path)
    }) {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        raw_links.extend(extract_markdown_links(
            archive
                .resolve(&entry.source.revision, &entry.source.path)
                .map_err(|error| error.to_string())?,
            host,
            Some(&archive.scope().repository),
            cancel,
        )?);
        if raw_links.len() > 128 {
            return Err("Pinned dependency references exceed the discovery budget".into());
        }
    }
    if raw_links.len() > 128 {
        return Err("Pinned dependency references exceed the discovery budget".into());
    }

    // Links to generated, lockfile or binary paths are left out before their repository is counted,
    // checked or fetched, like the files themselves.
    let raw_links: Vec<RawDiscoveredLink> = raw_links
        .into_iter()
        .filter(|l| l.repository() != archive.scope().repository && !l.only_unreviewable_paths())
        .collect();

    let mut distinct_repos: BTreeSet<_> = raw_links.iter().map(|link| link.repository().to_string()).collect();
    if distinct_repos.len() > 16 {
        return Err("Pinned dependency selection exceeds its bounded repository/file limit".into());
    }
    for repo_name in &distinct_repos {
        ensure_dependency_repository(root, repo_name, host, network, authorized, cancel, read_access)?;
    }

    let mut ref_cache = BTreeMap::<(String, String), ResolvedRef>::new();
    if let Some(recorded_provs) = recorded {
        for prov in recorded_provs {
            ref_cache.insert(
                (prov.repository.clone(), prov.reference.clone()),
                ResolvedRef::Found {
                    commit: prov.commit.clone(),
                    kind: prov.reference_kind,
                },
            );
        }
    }
    let mut resolved_links = Vec::new();

    for raw in raw_links {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        match raw {
            RawDiscoveredLink::Sha {
                url,
                repository,
                sha,
                path,
            } => {
                resolved_links.push(DiscoveredLink {
                    url,
                    repository,
                    reference: sha.clone(),
                    path,
                    kind: ReferenceKind::Commit,
                    commit: sha,
                });
            }
            RawDiscoveredLink::Named {
                url,
                repository,
                candidate_splits,
            } => {
                let repo_dir = root.join(&repository);
                let mut matching = Vec::new();
                let mut had_collision = None;

                let mut found_recorded = BTreeSet::new();
                if let Some(recorded_provs) = recorded {
                    for (cand_ref, cand_path) in &candidate_splits {
                        for prov in recorded_provs {
                            if prov.repository == repository && prov.reference == *cand_ref && prov.path == *cand_path {
                                found_recorded.insert((
                                    cand_ref.clone(),
                                    cand_path.clone(),
                                    prov.commit.clone(),
                                    prov.reference_kind,
                                ));
                            }
                        }
                    }
                }

                if !found_recorded.is_empty() {
                    if found_recorded.len() > 1 {
                        return Err(format!("Ambiguous pinned dependency link {}: multiple valid reference interpretations match in repository {}", sanitize_url(&url), repository));
                    }
                    let (reference, path, commit, kind) = found_recorded.into_iter().next().unwrap();
                    resolved_links.push(DiscoveredLink {
                        url,
                        repository,
                        reference,
                        path,
                        kind,
                        commit,
                    });
                } else if !network {
                    return Err(format!(
                        "Pinned dependency link {} requires hydration before offline review preparation",
                        sanitize_url(&url)
                    ));
                } else {
                    for (cand_ref, cand_path) in candidate_splits {
                        if cancel.load(Ordering::Relaxed) {
                            return Err("cancelled".into());
                        }
                        if ref_cache.len() > 128 {
                            return Err("Pinned dependency resolution exceeds the reference budget".into());
                        }
                        let key = (repository.clone(), cand_ref.clone());
                        let resolved = if let Some(cached) = ref_cache.get(&key) {
                            cached.clone()
                        } else {
                            let res = resolve_named_ref(&repo_dir, &repository, host, &cand_ref, cancel)?;
                            ref_cache.insert(key, res.clone());
                            res
                        };
                        match resolved {
                            ResolvedRef::Found { commit, kind } => {
                                matching.push((cand_ref, cand_path, commit, kind));
                            }
                            ResolvedRef::Collision => {
                                had_collision = Some(cand_ref);
                            }
                            ResolvedRef::NotFound => {}
                        }
                    }

                    if let Some(collided) = had_collision {
                        return Err(format!(
                            "Ambiguous reference in {}: '{}' exists as both a branch and a tag in {}",
                            sanitize_url(&url),
                            collided,
                            repository
                        ));
                    }
                    if matching.is_empty() {
                        return Err(format!("Pinned dependency reference is unavailable for {}: reference could not be resolved in repository {}", sanitize_url(&url), repository));
                    }
                    if matching.len() > 1 {
                        return Err(format!("Ambiguous pinned dependency link {}: multiple valid reference interpretations match in repository {}", sanitize_url(&url), repository));
                    }
                    let (reference, path, commit, kind) = matching.remove(0);
                    resolved_links.push(DiscoveredLink {
                        url,
                        repository,
                        reference,
                        path,
                        kind,
                        commit,
                    });
                }
            }
        }
    }

    // A link to a generated, lockfile or binary path is left out like the file itself; a link to
    // private configuration or a private path still stops preparation.
    let mut kept_links = Vec::with_capacity(resolved_links.len());
    for link in resolved_links {
        let entry = review_archive::SourceEntry {
            path: link.path.clone(),
            revision: link.commit.clone(),
            git_blob_id: link.commit.clone(),
            mode: "100644".into(),
            bytes: 0,
        };
        match entry.exclusion().map_err(|error| error.to_string())? {
            None => kept_links.push(link),
            Some(review_archive::Exclusion::Generated | review_archive::Exclusion::BinaryPath) => {}
            Some(_) => {
                return Err(format!(
                    "Pinned dependency {} contains a requested path excluded by evidence policy",
                    link.repository
                ))
            }
        }
    }
    let resolved_links = kept_links;

    let mut submodule_requests = Vec::new();
    for revision in archive.scope().revisions() {
        let gitlinks = archive
            .entries()
            .iter()
            .filter(|entry| entry.source.revision == revision && entry.source.mode == "160000")
            .collect::<Vec<_>>();
        if gitlinks.is_empty() {
            continue;
        }
        let modules = module_repositories(
            repo,
            archive
                .resolve(revision, ".gitmodules")
                .map_err(|_| "Pinned submodule configuration is unavailable")?,
            host,
            cancel,
        )?;
        for entry in gitlinks {
            let repository = modules
                .get(&entry.source.path)
                .ok_or("Pinned submodule has no repository mapping")?;
            if distinct_repos.insert(repository.clone()) {
                if distinct_repos.len() > 16 {
                    return Err("Pinned dependency selection exceeds its bounded repository/file limit".into());
                }
                ensure_dependency_repository(root, repository, host, network, authorized, cancel, read_access)?;
            }
            submodule_requests.push((repository.clone(), entry.source.git_blob_id.clone()));
        }
    }

    let mut merged = BTreeMap::<(String, String), (Option<BTreeSet<String>>, Vec<DependencyProvenance>)>::new();

    for link in resolved_links {
        let key = (link.repository.clone(), link.commit.clone());
        let prov = DependencyProvenance {
            url: link.url,
            repository: link.repository,
            reference: link.reference,
            reference_kind: link.kind,
            path: link.path.clone(),
            commit: link.commit,
        };
        match merged.entry(key) {
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert((Some(BTreeSet::from([link.path])), vec![prov]));
            }
            std::collections::btree_map::Entry::Occupied(mut e) => {
                let (paths, provenance) = e.get_mut();
                if let Some(paths) = paths {
                    paths.insert(link.path);
                }
                provenance.push(prov);
            }
        }
    }

    for (sub_repo, sub_rev) in submodule_requests {
        let key = (sub_repo, sub_rev);
        match merged.entry(key) {
            std::collections::btree_map::Entry::Vacant(e) => {
                e.insert((None, Vec::new()));
            }
            std::collections::btree_map::Entry::Occupied(mut e) => {
                e.get_mut().0 = None;
            }
        }
    }

    if merged.len() > 16
        || merged
            .values()
            .filter_map(|(p, _)| p.as_ref())
            .map(BTreeSet::len)
            .sum::<usize>()
            > 128
    {
        return Err("Pinned dependency selection exceeds its bounded repository/file limit".into());
    }

    Ok(merged
        .into_iter()
        .map(|((repository, revision), (paths, mut provenance))| {
            provenance.sort();
            provenance.dedup();
            CaptureRequest {
                repository,
                revision,
                paths,
                provenance,
            }
        })
        .collect())
}

pub(crate) fn capture_with_recorded(
    archive: Archive,
    repo: &Path,
    changed: &[String],
    pr_body: &str,
    prepared: &BTreeSet<String>,
    network: bool,
    recorded: Option<&[DependencyProvenance]>,
    cancel: &AtomicBool,
) -> Result<Archive, String> {
    let primary = archive.scope().repository.clone();
    let mut primary_public = None;
    capture_with_recorded_and_access(
        archive,
        repo,
        changed,
        pr_body,
        prepared,
        network,
        recorded,
        cancel,
        &mut |repository, host, cancel| {
            verify_repository_read_access(repository, host, cancel)?;
            authorize_dependency_disclosure_using(&primary, &mut primary_public, repository, host, cancel, |args| {
                crate::gitlab::glab_capture_cancellable(args, cancel)
            })
        },
    )
}

fn capture_with_recorded_and_access(
    archive: Archive,
    repo: &Path,
    changed: &[String],
    pr_body: &str,
    authorized: &BTreeSet<String>,
    network: bool,
    recorded: Option<&[DependencyProvenance]>,
    cancel: &AtomicBool,
    read_access: &mut dyn FnMut(&str, &str, &AtomicBool) -> Result<(), String>,
) -> Result<Archive, String> {
    let root = repo
        .parent()
        .and_then(Path::parent)
        .ok_or("Repository root unavailable")?;
    if root.join(&archive.scope().repository) != repo {
        return Err("Review repository does not match the configured repository root".into());
    }
    let requests = discover(
        &archive,
        repo,
        changed,
        pr_body,
        authorized,
        root,
        network,
        recorded,
        cancel,
        read_access,
    )?;
    if requests.is_empty() {
        return Ok(archive);
    }
    let mut dependencies = Vec::new();
    let mut budget = archive.descriptor();
    for request in requests {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let path = root.join(&request.repository);
        let scope = scope(&archive.scope().host, &request.repository, &request.revision)?;
        let unavailable = || {
            format!("Pinned dependency {} @ {} is unavailable. Retry authenticated gh dependency preparation and make this exact commit available before reviewing.", request.repository, request.revision)
        };
        if !path.join(".git").is_dir() {
            return Err(unavailable());
        }
        let has_commit = crate::indexer::git_frozen_with_input(
            &path,
            &["cat-file", "-e", &format!("{}^{{commit}}", request.revision)],
            cancel,
            &[],
        )
        .is_ok();
        if !has_commit {
            let is_obj =
                crate::indexer::git_frozen_with_input(&path, &["cat-file", "-e", &request.revision], cancel, &[])
                    .is_ok();
            if is_obj {
                return Err(format!(
                    "Pinned dependency reference targets a non-commit object: {} in {}",
                    request.revision, request.repository
                ));
            }
            if network {
                let origin = crate::indexer::git(&path, &["remote", "get-url", "origin"], Some(cancel))
                    .map_err(|e| dependency_git_error(&e, &unavailable(), cancel))?;
                if repository_url(
                    std::str::from_utf8(&origin).map_err(|_| unavailable())?.trim(),
                    &scope.host,
                )
                .as_deref()
                    != Some(request.repository.as_str())
                {
                    return Err("Dependency repository origin does not match the authorized GitHub repository".into());
                }
                crate::indexer::git(
                    &path,
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-write-fetch-head",
                        "--no-auto-maintenance",
                        "--recurse-submodules=no",
                        "origin",
                        &request.revision,
                    ],
                    Some(cancel),
                )
                .map_err(|e| dependency_git_error(&e, &unavailable(), cancel))?;
                if crate::indexer::git_frozen_with_input(
                    &path,
                    &["cat-file", "-e", &format!("{}^{{commit}}", request.revision)],
                    cancel,
                    &[],
                )
                .is_err()
                {
                    return Err(format!(
                        "Pinned dependency reference targets a non-commit object: {} in {}",
                        request.revision, request.repository
                    ));
                }
                review_archive::hydrate(&path, &scope, cancel).map_err(|error| format!("{} {error}", unavailable()))?;
            } else {
                return Err(unavailable());
            }
        } else if network {
            let origin = crate::indexer::git(&path, &["remote", "get-url", "origin"], Some(cancel))
                .map_err(|e| dependency_git_error(&e, &unavailable(), cancel))?;
            if repository_url(
                std::str::from_utf8(&origin).map_err(|_| unavailable())?.trim(),
                &scope.host,
            )
            .as_deref()
                != Some(request.repository.as_str())
            {
                return Err("Dependency repository origin does not match the authorized GitHub repository".into());
            }
            review_archive::hydrate(&path, &scope, cancel).map_err(|error| format!("{} {error}", unavailable()))?;
        }
        let dependency = review_archive::capture_selected(&path, scope, request.paths.as_ref(), cancel)
            .map_err(|error| format!("{} {error}", unavailable()))?;
        if let Some(paths) = &request.paths {
            for p in paths {
                dependency.resolve(&request.revision, p).map_err(|error| {
                    format!(
                        "Pinned dependency {} @ {}: {p}: {error}",
                        request.repository, request.revision
                    )
                })?;
            }
        }
        let descriptor = dependency.descriptor();
        budget.blob_bytes += descriptor.blob_bytes;
        budget.entry_count += descriptor.entry_count;
        review_archive::validate_limits(budget.blob_bytes, 0, budget.entry_count).map_err(|error| error.to_string())?;
        dependencies.push((dependency, request.provenance));
    }
    archive
        .with_dependencies_and_provenance(dependencies)
        .map_err(|error| error.to_string())
}

pub(crate) fn capture(
    archive: Archive,
    repo: &Path,
    changed: &[String],
    pr_body: &str,
    authorized: &BTreeSet<String>,
    network: bool,
    cancel: &AtomicBool,
) -> Result<Archive, String> {
    capture_with_recorded(archive, repo, changed, pr_body, authorized, network, None, cancel)
}

#[cfg(test)]
#[path = "dependencies_tests.rs"]
mod tests;
