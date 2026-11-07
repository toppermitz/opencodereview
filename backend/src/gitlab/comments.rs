use super::*;

#[derive(Debug, Clone)]
pub struct IssueCommentUpsert {
    pub id: i64,
    pub created: bool,
}

/// Owns the payload file so every exit path removes it, including write errors.
pub struct PreparedReview {
    path: std::path::PathBuf,
}

impl Drop for PreparedReview {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl PreparedReview {
    pub(crate) fn in_directory(payload_json: &str, directory: &std::path::Path) -> Result<Self, String> {
        static NEXT_PAYLOAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        Self::with_paths(payload_json, || {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let sequence = NEXT_PAYLOAD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            directory.join(format!("ocr-review-{}-{stamp}-{sequence}.json", std::process::id()))
        })
    }

    fn with_paths(payload_json: &str, mut next_path: impl FnMut() -> std::path::PathBuf) -> Result<Self, String> {
        use std::io::Write;
        for _ in 0..16 {
            let path = next_path();
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = match options.open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            };
            let prepared = Self { path };
            let written = file.write_all(payload_json.as_bytes());
            drop(file);
            written.map_err(|error| error.to_string())?;
            return Ok(prepared);
        }
        Err("Could not create a unique review payload after 16 attempts".into())
    }
}

pub fn prepare_review(payload_json: &str) -> Result<PreparedReview, String> {
    PreparedReview::in_directory(payload_json, &std::env::temp_dir())
}

fn mr_diff_refs(full_name: &str, number: i64) -> Result<(String, String, String), String> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}");
    let bytes = run_raw("glab", &["api", &route])?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let refs = value.get("diff_refs").ok_or("Missing merge request diff_refs")?;
    Ok((
        refs.get("base_sha")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        refs.get("start_sha")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        refs.get("head_sha")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("sha").and_then(|v| v.as_str()))
            .unwrap_or_default()
            .to_string(),
    ))
}

/// GitLab quick-action line for a review verdict, or `None` for a plain comment.
pub(crate) fn gitlab_verdict_quick_action(event: &str) -> Option<&'static str> {
    match event {
        "APPROVE" => Some("/approve"),
        "REQUEST_CHANGES" => Some("/request_changes"),
        _ => None,
    }
}

/// Ensure the MR note body carries the GitLab verdict quick action on its own line.
pub(crate) fn with_gitlab_verdict(body: &str, event: &str) -> String {
    let Some(action) = gitlab_verdict_quick_action(event) else {
        return body.to_string();
    };
    if body.lines().any(|line| line.trim() == action) {
        return body.to_string();
    }
    if body.trim().is_empty() {
        action.to_string()
    } else {
        format!("{action}\n\n{body}")
    }
}

/// Post a review as a summary note plus per-finding discussions on the MR.
/// Returns the summary note id (or first discussion note id) for marker recovery.
pub fn post_review(full_name: &str, number: i64, prepared: PreparedReview) -> Result<Option<i64>, String> {
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&prepared.path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("could not parse review payload: {e}"))?;
    let event = payload.get("event").and_then(|v| v.as_str()).unwrap_or("COMMENT");
    let body = with_gitlab_verdict(payload.get("body").and_then(|v| v.as_str()).unwrap_or_default(), event);
    let comments = payload
        .get("comments")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let project = project_api_id(full_name);
    let mut review_id = None;

    if !body.trim().is_empty() {
        let note = post_mr_note(full_name, number, &body)?;
        review_id = Some(note);
    }

    if !comments.is_empty() {
        let (base_sha, start_sha, head_sha) = mr_diff_refs(full_name, number)?;
        for comment in comments {
            let path = comment.get("path").and_then(|v| v.as_str()).unwrap_or_default();
            let line = comment.get("line").and_then(|v| v.as_i64()).unwrap_or_default();
            let side = comment.get("side").and_then(|v| v.as_str()).unwrap_or("RIGHT");
            let text = comment.get("body").and_then(|v| v.as_str()).unwrap_or_default();
            if path.is_empty() || text.is_empty() {
                continue;
            }
            let mut position = serde_json::json!({
                "base_sha": base_sha,
                "start_sha": start_sha,
                "head_sha": head_sha,
                "position_type": "text",
                "old_path": path,
                "new_path": path,
            });
            if side.eq_ignore_ascii_case("LEFT") {
                position["old_line"] = serde_json::json!(line);
            } else {
                position["new_line"] = serde_json::json!(line);
            }
            let discussion = serde_json::json!({ "body": text, "position": position });
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let path_file = std::env::temp_dir().join(format!("ocr-discussion-{}-{stamp}.json", std::process::id()));
            std::fs::write(&path_file, discussion.to_string()).map_err(|e| e.to_string())?;
            let route = format!("projects/{project}/merge_requests/{number}/discussions");
            let result = run_raw(
                "glab",
                &[
                    "api",
                    "--method",
                    "POST",
                    &route,
                    "--input",
                    &path_file.to_string_lossy(),
                ],
            );
            let _ = std::fs::remove_file(&path_file);
            let bytes = result.map_err(|error| format!("could not post review discussion (GitLab API): {error}"))?;
            if review_id.is_none() {
                review_id = serde_json::from_slice::<serde_json::Value>(&bytes)
                    .ok()
                    .and_then(|value| {
                        value
                            .pointer("/notes/0/id")
                            .or_else(|| value.get("id"))
                            .and_then(|id| id.as_i64())
                    });
            }
        }
    }
    Ok(review_id)
}

fn post_mr_note(full_name: &str, number: i64, body: &str) -> Result<i64, String> {
    write_issue_comment(full_name, number, None, body, "MR review").map(|u| u.id)
}

pub fn find_review_id_by_marker(full_name: &str, number: i64, marker: &str) -> Result<Option<i64>, String> {
    find_issue_comment_id_by_marker(full_name, number, marker)
}

pub fn upsert_issue_comment_by_marker(
    full_name: &str,
    number: i64,
    marker: &str,
    body: &str,
) -> Result<IssueCommentUpsert, String> {
    let existing = find_issue_comment_id_by_marker(full_name, number, marker)?;
    write_issue_comment(full_name, number, existing, body, "MR summary")
}

pub(crate) fn write_issue_comment(
    full_name: &str,
    number: i64,
    existing: Option<i64>,
    body: &str,
    kind: &str,
) -> Result<IssueCommentUpsert, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!("ocr-mr-note-{}-{stamp}.json", std::process::id()));
    std::fs::write(&path, serde_json::json!({ "body": body }).to_string()).map_err(|error| error.to_string())?;
    let path_str = path.to_string_lossy().to_string();
    let project = project_api_id(full_name);
    let (method, route, created) = if let Some(id) = existing {
        (
            "PUT",
            format!("projects/{project}/merge_requests/{number}/notes/{id}"),
            false,
        )
    } else {
        (
            "POST",
            format!("projects/{project}/merge_requests/{number}/notes"),
            true,
        )
    };
    let result = run_raw("glab", &["api", "--method", method, &route, "--input", &path_str]);
    let _ = std::fs::remove_file(&path);
    let bytes = result.map_err(|error| format!("could not upsert {kind} note (GitLab API): {error}"))?;
    let id = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value.get("id").and_then(|id| id.as_i64()))
        .ok_or_else(|| "GitLab did not return note id".to_string())?;
    Ok(IssueCommentUpsert { id, created })
}

pub fn find_issue_comment_id_by_marker(full_name: &str, number: i64, marker: &str) -> Result<Option<i64>, String> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}/notes?per_page=100");
    let bytes = run_raw("glab", &["api", "--paginate", &route])?;
    let comments = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|error| error.to_string())?
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(comments.into_iter().find_map(|comment| {
        let body = comment.get("body").and_then(|v| v.as_str()).unwrap_or_default();
        body.contains(marker)
            .then(|| comment.get("id").and_then(|v| v.as_i64()))
            .flatten()
    }))
}

const OCR_FINGERPRINT_PREFIX: &str = "<!-- ocr:fingerprint=";

#[derive(Debug, Clone)]
pub struct DedupeResult {
    pub post_comments: serde_json::Value,
    pub annotated_comments_json: String,
    pub suppressed_count: usize,
}

pub(crate) fn normalize_finding_text(value: &str) -> String {
    value
        .lines()
        .filter(|line| !line.trim_start().starts_with("<!-- ocr:fingerprint="))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn ocr_fingerprint(path: &str, line: i64, body: &str) -> String {
    let input = format!("{}:{}:{}", path, line, normalize_finding_text(body));
    let mut hash = 0xcbf29ce484222325u64;
    for byte in input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv64:{hash:016x}")
}

pub(crate) fn extract_ocr_fingerprint(body: &str) -> Option<String> {
    let start = body.find(OCR_FINGERPRINT_PREFIX)? + OCR_FINGERPRINT_PREFIX.len();
    let rest = &body[start..];
    let end = rest.find(" -->")?;
    Some(rest[..end].trim().to_string())
}

pub(crate) fn is_ocr_comment(body: &str) -> bool {
    extract_ocr_fingerprint(body).is_some() || crate::review::contains_review_attribution(body)
}

pub(crate) fn with_fingerprint_marker(path: &str, line: i64, body: &str) -> (String, String) {
    if let Some(fingerprint) = extract_ocr_fingerprint(body) {
        return (fingerprint, body.to_string());
    }
    let fingerprint = ocr_fingerprint(path, line, body);
    (
        fingerprint.clone(),
        format!("<!-- ocr:fingerprint={fingerprint} -->\n{body}"),
    )
}

pub(crate) fn github_review_comment(comment: &serde_json::Value, body: String) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for key in ["path", "line", "side", "start_line", "start_side"] {
        if let Some(value) = comment.get(key) {
            obj.insert(key.to_string(), value.clone());
        }
    }
    obj.insert("body".to_string(), serde_json::json!(body));
    serde_json::Value::Object(obj)
}

pub fn dedupe_review_comments(full_name: &str, number: i64, comments: serde_json::Value) -> DedupeResult {
    dedupe_against(&existing_ocr_comments(full_name, number), comments)
}

fn point_title(body: &str) -> Option<&str> {
    let start = [
        format!("**{} ", crate::review::FINDING_MARKER),
        format!("**{} ", crate::review::QUESTION_MARKER),
        "**[Finding] ".to_string(),
        "**[Question] ".to_string(),
    ]
    .into_iter()
    .find_map(|marker| body.find(&marker).map(|at| at + marker.len()))?;
    let end = body[start..].find("**\n").or_else(|| body[start..].find("**"))?;
    Some(body[start..start + end].trim())
}

fn already_posted<'a>(
    existing: &'a [ExistingOcrComment],
    fingerprint: &str,
    path: &str,
    line: i64,
    body: &str,
) -> Option<&'a ExistingOcrComment> {
    let title = point_title(body);
    existing.iter().find(|comment| {
        comment.fingerprint.as_deref() == Some(fingerprint)
            || title.zip(comment.title.as_deref()).is_some_and(|(title, other)| {
                crate::review::evidence::same_point((&comment.path, comment.line, other), (path, line, title))
            })
    })
}

pub(crate) fn dedupe_against(existing: &[ExistingOcrComment], comments: serde_json::Value) -> DedupeResult {
    let mut post_comments = Vec::new();
    let mut annotated = Vec::new();
    let mut suppressed_count = 0usize;
    for value in comments.as_array().cloned().unwrap_or_default() {
        let mut comment = value;
        let path = comment
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let line = comment.get("line").and_then(|v| v.as_i64()).unwrap_or_default();
        let body = comment
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let (fingerprint, body_with_marker) = with_fingerprint_marker(&path, line, &body);
        if let Some(duplicate) = already_posted(existing, &fingerprint, &path, line, &body) {
            if let Some(obj) = comment.as_object_mut() {
                obj.insert("status".to_string(), serde_json::json!("duplicate_suppressed"));
                obj.insert("fingerprint".to_string(), serde_json::json!(fingerprint));
                obj.insert("duplicateUrl".to_string(), serde_json::json!(duplicate.url));
                obj.insert("duplicateAuthor".to_string(), serde_json::json!(duplicate.author));
            }
            suppressed_count += 1;
            annotated.push(comment);
        } else {
            let post_comment = github_review_comment(&comment, body_with_marker.clone());
            if let Some(obj) = comment.as_object_mut() {
                obj.insert("body".to_string(), serde_json::json!(body_with_marker));
                obj.insert("status".to_string(), serde_json::json!("new"));
                obj.insert("fingerprint".to_string(), serde_json::json!(fingerprint));
            }
            post_comments.push(post_comment);
            annotated.push(comment);
        }
    }
    DedupeResult {
        post_comments: serde_json::Value::Array(post_comments),
        annotated_comments_json: serde_json::json!({ "comments": annotated }).to_string(),
        suppressed_count,
    }
}

#[derive(Debug, Clone)]
pub struct KeepPoint {
    pub fingerprint: Option<String>,
    pub path: String,
    pub line: i64,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExistingOcrComment {
    author: String,
    url: String,
    path: String,
    line: i64,
    fingerprint: Option<String>,
    title: Option<String>,
    discussion_id: Option<String>,
    resolved: bool,
    resolvable: bool,
}

impl ExistingOcrComment {
    fn from_discussion(discussion: &serde_json::Value) -> Option<Self> {
        let discussion_id = discussion.get("id").and_then(|v| v.as_str()).map(str::to_string);
        let resolved = discussion.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false);
        let resolvable = discussion.get("resolvable").and_then(|v| v.as_bool()).unwrap_or(false);
        let notes = discussion.get("notes").and_then(|v| v.as_array())?;
        let position = discussion
            .get("position")
            .or_else(|| notes.first().and_then(|note| note.get("position")));
        notes
            .iter()
            .find_map(|note| Self::from_discussion_note(note, position, discussion_id.clone(), resolved, resolvable))
    }

    fn from_discussion_note(
        note: &serde_json::Value,
        position: Option<&serde_json::Value>,
        discussion_id: Option<String>,
        resolved: bool,
        resolvable: bool,
    ) -> Option<Self> {
        let body = note.get("body").and_then(|v| v.as_str()).unwrap_or_default();
        if !is_ocr_comment(body) {
            return None;
        }
        let author = note
            .pointer("/author/username")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let path = position
            .and_then(|p| p.get("new_path").or_else(|| p.get("old_path")))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let line = position
            .and_then(|p| p.get("new_line").or_else(|| p.get("old_line")).and_then(|v| v.as_i64()))
            .unwrap_or_default();
        Some(Self {
            author,
            url: note
                .get("web_url") // may be absent on notes
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            path,
            line,
            fingerprint: extract_ocr_fingerprint(body),
            title: point_title(body).map(str::to_string),
            discussion_id,
            resolved,
            resolvable,
        })
    }

    #[cfg(test)]
    fn from_github(comment: &serde_json::Value) -> Option<Self> {
        let body = comment.get("body").and_then(|v| v.as_str()).unwrap_or_default();
        if !is_ocr_comment(body) {
            return None;
        }
        let field = |pointer: &str| comment.pointer(pointer).and_then(|v| v.as_str());
        Some(Self {
            author: field("/user/login")
                .or_else(|| field("/author/username"))
                .unwrap_or("unknown")
                .to_string(),
            url: field("/html_url")
                .or_else(|| field("/web_url"))
                .unwrap_or_default()
                .to_string(),
            path: field("/path").unwrap_or_default().to_string(),
            line: ["/line", "/original_line"]
                .iter()
                .find_map(|pointer| comment.pointer(pointer).and_then(|v| v.as_i64()))
                .unwrap_or_default(),
            fingerprint: extract_ocr_fingerprint(body),
            title: point_title(body).map(str::to_string),
            discussion_id: field("/discussion_id").map(str::to_string),
            resolved: comment.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false),
            resolvable: comment.get("resolvable").and_then(|v| v.as_bool()).unwrap_or(true),
        })
    }
}

fn matches_keep_point(comment: &ExistingOcrComment, keep: &[KeepPoint]) -> bool {
    keep.iter().any(|point| {
        point.fingerprint.is_some() && point.fingerprint == comment.fingerprint
            || point
                .title
                .as_deref()
                .zip(comment.title.as_deref())
                .is_some_and(|(title, other)| {
                    crate::review::evidence::same_point(
                        (&comment.path, comment.line, other),
                        (&point.path, point.line, title),
                    )
                })
    })
}

/// Discussion ids that should be resolved because they are open OCR threads not kept by the new review.
pub(crate) fn stale_ocr_discussion_ids(existing: &[ExistingOcrComment], keep: &[KeepPoint]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    existing
        .iter()
        .filter(|comment| comment.resolvable && !comment.resolved && !matches_keep_point(comment, keep))
        .filter_map(|comment| comment.discussion_id.clone())
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

pub(crate) fn keep_points_from_annotated_comments(annotated_json: &str) -> Vec<KeepPoint> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(annotated_json) else {
        return Vec::new();
    };
    value
        .get("comments")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter(|comment| {
            matches!(
                comment.get("status").and_then(|v| v.as_str()),
                Some("new" | "duplicate_suppressed")
            )
        })
        .map(|comment| {
            let path = comment
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let line = comment.get("line").and_then(|v| v.as_i64()).unwrap_or_default();
            let body = comment.get("body").and_then(|v| v.as_str()).unwrap_or_default();
            KeepPoint {
                fingerprint: comment
                    .get("fingerprint")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| extract_ocr_fingerprint(body)),
                title: point_title(body).map(str::to_string),
                path,
                line,
            }
        })
        .collect()
}

pub(crate) fn resolve_discussion(full_name: &str, number: i64, discussion_id: &str) -> Result<(), String> {
    let project = project_api_id(full_name);
    let encoded = urlencoding_path_segment(discussion_id);
    let route = format!("projects/{project}/merge_requests/{number}/discussions/{encoded}");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!("ocr-resolve-{}-{stamp}.json", std::process::id()));
    std::fs::write(&path, serde_json::json!({ "resolved": true }).to_string()).map_err(|e| e.to_string())?;
    let result = run_raw(
        "glab",
        &["api", "--method", "PUT", &route, "--input", &path.to_string_lossy()],
    );
    let _ = std::fs::remove_file(&path);
    result
        .map(|_| ())
        .map_err(|error| format!("could not resolve discussion (GitLab API): {error}"))
}

/// Resolve open OCR discussions that the current review is no longer standing behind.
/// Best-effort: individual failures are logged and skipped.
pub fn resolve_stale_ocr_discussions(full_name: &str, number: i64, keep: &[KeepPoint]) -> usize {
    let existing = existing_ocr_comments(full_name, number);
    let mut resolved = 0usize;
    for discussion_id in stale_ocr_discussion_ids(&existing, keep) {
        match resolve_discussion(full_name, number, &discussion_id) {
            Ok(()) => resolved += 1,
            Err(error) => eprintln!("resolve OCR discussion {discussion_id} on {full_name}!{number}: {error}"),
        }
    }
    resolved
}

fn urlencoding_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(crate) fn existing_ocr_comments(full_name: &str, number: i64) -> Vec<ExistingOcrComment> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}/discussions?per_page=100");
    let Ok(bytes) = run_raw("glab", &["api", "--paginate", &route]) else {
        return Vec::new();
    };
    let Ok(pages) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Vec::new();
    };
    pages
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(ExistingOcrComment::from_discussion)
        .collect()
}

#[cfg(test)]
mod dedupe_tests {
    use super::*;

    fn posted(path: &str, line: i64, title: &str) -> serde_json::Value {
        let body = format!(
            "<!-- ocr:fingerprint=fnv64:{line:016x} -->\nOpenCodeReview\n\n**{} {title}**\n\nDetail",
            crate::review::FINDING_MARKER
        );
        serde_json::json!({"path": path, "line": line, "body": body, "html_url": format!("https://x/{path}/{line}"), "user": {"login": "me"}})
    }

    fn new_comment(path: &str, line: i64, title: &str) -> serde_json::Value {
        serde_json::json!({"path": path, "line": line, "side": "RIGHT", "body": format!("**{} {title}**\n\nOther detail", crate::review::FINDING_MARKER)})
    }

    fn dedupe(existing: &[serde_json::Value], comments: Vec<serde_json::Value>) -> DedupeResult {
        let existing: Vec<_> = existing.iter().filter_map(ExistingOcrComment::from_github).collect();
        dedupe_against(&existing, serde_json::Value::Array(comments))
    }

    #[test]
    fn a_different_point_on_an_already_commented_line_is_posted() {
        let result = dedupe(
            &[posted("src/a.rs", 10, "Cache is never invalidated")],
            vec![new_comment("src/a.rs", 10, "Lock is held across the network call")],
        );
        assert_eq!(
            (result.suppressed_count, result.post_comments.as_array().unwrap().len()),
            (0, 1)
        );
    }

    #[test]
    fn the_same_point_moved_by_a_later_push_is_not_posted_again() {
        let result = dedupe(
            &[posted("src/a.rs", 10, "Cache is never invalidated")],
            vec![new_comment("src/a.rs", 42, "Cache is never invalidated")],
        );
        assert_eq!(result.suppressed_count, 1);
        assert!(result.annotated_comments_json.contains("https://x/src/a.rs/10"));
    }

    #[test]
    fn a_close_rewording_nearby_is_a_repeat_but_not_in_another_file() {
        let existing = [posted("src/a.rs", 10, "Cache entry is never invalidated after update")];
        let near = dedupe(
            &existing,
            vec![new_comment("src/a.rs", 12, "Cache entry never invalidated on update")],
        );
        assert_eq!(near.suppressed_count, 1);
        let other_file = dedupe(
            &existing,
            vec![new_comment(
                "src/b.rs",
                10,
                "Cache entry is never invalidated after update",
            )],
        );
        assert_eq!(other_file.suppressed_count, 0);
    }

    #[test]
    fn outdated_comments_keep_their_original_line_and_foreign_comments_are_ignored() {
        let mut outdated = posted("src/a.rs", 7, "Wrong default port");
        outdated["original_line"] = outdated["line"].take();
        let comment = ExistingOcrComment::from_github(&outdated).unwrap();
        assert_eq!(
            (comment.line, comment.title.as_deref()),
            (7, Some("Wrong default port"))
        );
        assert!(
            ExistingOcrComment::from_github(&serde_json::json!({"body": "LGTM", "path": "src/a.rs", "line": 7}))
                .is_none()
        );
    }

    #[test]
    fn gitlab_verdicts_are_appended_as_standalone_quick_actions() {
        assert_eq!(gitlab_verdict_quick_action("APPROVE"), Some("/approve"));
        assert_eq!(gitlab_verdict_quick_action("REQUEST_CHANGES"), Some("/request_changes"));
        assert_eq!(gitlab_verdict_quick_action("COMMENT"), None);
        assert_eq!(with_gitlab_verdict("", "APPROVE"), "/approve");
        assert_eq!(with_gitlab_verdict("Looks good", "APPROVE"), "/approve\n\nLooks good");
        assert_eq!(
            with_gitlab_verdict("/approve\n\nLooks good", "APPROVE"),
            "/approve\n\nLooks good"
        );
        assert_eq!(with_gitlab_verdict("Findings", "COMMENT"), "Findings");
        assert!(with_gitlab_verdict("Fix this", "REQUEST_CHANGES").starts_with("/request_changes\n\n"));
    }

    #[test]
    fn stale_ocr_discussions_skip_kept_resolved_and_non_resolvable() {
        let open = ExistingOcrComment::from_github(&serde_json::json!({
            "path": "src/a.rs",
            "line": 10,
            "body": "<!-- ocr:fingerprint=fnv64:aaaaaaaaaaaaaaaa -->\n**[Achado] Cache is never invalidated**\n\nDetail",
            "html_url": "https://x/1",
            "user": {"login": "bot"},
            "discussion_id": "disc-open",
            "resolved": false,
            "resolvable": true
        }))
        .unwrap();
        let kept = ExistingOcrComment::from_github(&serde_json::json!({
            "path": "src/a.rs",
            "line": 12,
            "body": "<!-- ocr:fingerprint=fnv64:bbbbbbbbbbbbbbbb -->\n**[Achado] Lock is held across the network call**\n\nDetail",
            "html_url": "https://x/2",
            "user": {"login": "bot"},
            "discussion_id": "disc-kept",
            "resolved": false,
            "resolvable": true
        }))
        .unwrap();
        let already = ExistingOcrComment::from_github(&serde_json::json!({
            "path": "src/b.rs",
            "line": 1,
            "body": "<!-- ocr:fingerprint=fnv64:cccccccccccccccc -->\n**[Achado] Already fixed**\n\nDetail",
            "html_url": "https://x/3",
            "user": {"login": "bot"},
            "discussion_id": "disc-done",
            "resolved": true,
            "resolvable": true
        }))
        .unwrap();
        let keep = [KeepPoint {
            fingerprint: Some("fnv64:bbbbbbbbbbbbbbbb".into()),
            path: "src/a.rs".into(),
            line: 12,
            title: Some("Lock is held across the network call".into()),
        }];
        assert_eq!(
            stale_ocr_discussion_ids(&[open, kept, already], &keep),
            vec!["disc-open".to_string()]
        );
        assert_eq!(
            stale_ocr_discussion_ids(
                &[ExistingOcrComment::from_github(&serde_json::json!({
                    "path": "src/a.rs",
                    "line": 10,
                    "body": "<!-- ocr:fingerprint=fnv64:aaaaaaaaaaaaaaaa -->\n**[Achado] Cache is never invalidated**\n\nDetail",
                    "html_url": "https://x/1",
                    "user": {"login": "bot"},
                    "discussion_id": "disc-open",
                    "resolved": false,
                    "resolvable": true
                }))
                .unwrap()],
                &[]
            ),
            vec!["disc-open".to_string()]
        );
    }
}

#[cfg(test)]
mod prepared_review_tests {
    use super::PreparedReview;

    #[test]
    fn prepared_review_retries_collision_without_overwriting_or_removing_existing_file() {
        let existing = super::prepare_review("original").unwrap();
        let unused = super::prepare_review("placeholder").unwrap();
        let available = unused.path.clone();
        drop(unused);
        let mut paths = [existing.path.clone(), available.clone()].into_iter();
        let prepared = PreparedReview::with_paths("new payload", || paths.next().unwrap()).unwrap();
        assert_eq!(prepared.path, available);
        assert_eq!(std::fs::read_to_string(&existing.path).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(&prepared.path).unwrap(), "new payload");
        drop(prepared);
        assert!(!available.exists());
        assert_eq!(std::fs::read_to_string(&existing.path).unwrap(), "original");
    }

    #[test]
    fn prepared_review_exhausted_collisions_leave_existing_file_untouched() {
        let existing = super::prepare_review("original").unwrap();
        let mut attempts = 0;
        let result = PreparedReview::with_paths("replacement", || {
            attempts += 1;
            existing.path.clone()
        });
        assert!(result.is_err());
        assert_eq!(attempts, 16);
        assert_eq!(std::fs::read_to_string(&existing.path).unwrap(), "original");
    }

    #[test]
    fn prepared_review_concurrent_payloads_have_independent_contents_and_cleanup() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let handles: Vec<_> = (0..16)
            .map(|id| {
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let body = format!("payload-{id}");
                    (super::prepare_review(&body).unwrap(), body)
                })
            })
            .collect();
        let mut prepared: Vec<_> = handles.into_iter().map(|handle| handle.join().unwrap()).collect();
        let paths: std::collections::HashSet<_> = prepared.iter().map(|(file, _)| file.path.clone()).collect();
        assert_eq!(paths.len(), 16);
        while let Some((file, body)) = prepared.pop() {
            assert_eq!(std::fs::read_to_string(&file.path).unwrap(), body);
            let path = file.path.clone();
            drop(file);
            assert!(!path.exists());
            for (remaining, expected) in &prepared {
                assert_eq!(&std::fs::read_to_string(&remaining.path).unwrap(), expected);
            }
        }
    }
}
