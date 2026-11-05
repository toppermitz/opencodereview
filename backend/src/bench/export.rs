//! The benchmark export: a review's published comments as plain findings with head-side line
//! ranges, the shape every benchmark adapter starts from.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Finding {
    pub kind: String,
    pub path: String,
    /// The side the comment was anchored on: `RIGHT` (head) or `LEFT` (base).
    pub side: String,
    pub start_line: i64,
    pub end_line: i64,
    /// A `LEFT` anchor, moved to the head line where its deleted lines used to be.
    pub mapped_from_left: bool,
    pub title: String,
    pub detail: String,
    pub action: String,
    pub body: String,
}

impl Finding {
    /// What a judge reads: the title, why it is wrong, and the fix.
    pub(crate) fn message(&self) -> String {
        let mut message = self.title.trim().to_string();
        if !message.is_empty() && !message.ends_with(['.', '!', '?']) {
            message.push('.');
        }
        for (label, text) in [("", self.detail.trim()), ("Correção sugerida: ", self.action.trim())] {
            if !text.is_empty() {
                if !message.is_empty() {
                    message.push(' ');
                }
                message.push_str(label);
                message.push_str(text);
            }
        }
        message
    }
}

/// The findings and questions among a review's inline comments (`{"comments": [...]}`), in
/// their published order. `diff` is the base...head diff, for moving `LEFT` anchors to the head.
pub(crate) fn points(comments_json: &str, diff: &str) -> Result<Vec<Finding>, String> {
    let value: Value = serde_json::from_str(comments_json).map_err(|_| "Review comments are not JSON")?;
    let comments = value
        .get("comments")
        .or(Some(&value))
        .and_then(Value::as_array)
        .ok_or("Review comments are not a list")?;
    let mut points = Vec::new();
    for comment in comments {
        let (Some(path), Some(line)) = (comment["path"].as_str(), comment["line"].as_i64()) else {
            continue;
        };
        let path = path.trim_start_matches("./").to_string();
        let side = comment["side"].as_str().unwrap_or("RIGHT").to_string();
        let start = comment["start_line"].as_i64().unwrap_or(line).min(line);
        let start_side = comment["start_side"].as_str().unwrap_or(&side).to_string();
        let to_head = |line: i64, side: &str| {
            if side == "LEFT" {
                head_line(diff, &path, line).unwrap_or(1)
            } else {
                line
            }
        };
        let (start_line, end_line) = {
            let (start, end) = (to_head(start, &start_side), to_head(line, &side));
            (start.min(end).max(1), start.max(end).max(1))
        };
        let body = comment["body"].as_str().unwrap_or_default().to_string();
        let parsed = parse_body(&body);
        points.push(Finding {
            kind: parsed.kind,
            path,
            mapped_from_left: side == "LEFT" || start_side == "LEFT",
            side,
            start_line,
            end_line,
            title: parsed.title,
            detail: parsed.detail,
            action: parsed.action,
            body,
        });
    }
    Ok(points)
}

/// The suggestions a review summary lists (`evidence::suggestions_section`), as findings on their
/// head line, after the inline ones: a benchmark scores everything the review shows its reader.
pub(crate) fn suggestions(body: &str) -> Vec<Finding> {
    let (_, section) = crate::review::split_suggestions(body);
    section
        .lines()
        .filter_map(|item| {
            let (location, rest) = item.strip_prefix("- `")?.split_once("` **")?;
            let (path, number) = location.rsplit_once(':')?;
            let (title, rest) = rest.split_once("**. ")?;
            let (detail, action) = rest
                .split_once(&format!(" {} ", crate::review::SUGGESTED_CHANGE_LABEL))
                .or_else(|| rest.split_once(&format!(" {} ", crate::review::LEGACY_SUGGESTED_CHANGE_LABEL)))?;
            let line = number.parse::<i64>().ok()?.max(1);
            Some(Finding {
                kind: "suggestion".into(),
                path: path.trim_start_matches("./").into(),
                side: "RIGHT".into(),
                start_line: line,
                end_line: line,
                mapped_from_left: false,
                title: unescape(title),
                detail: unescape(detail),
                action: unescape(action),
                body: item.to_string(),
            })
        })
        .collect()
}

struct Parsed {
    kind: String,
    title: String,
    detail: String,
    action: String,
}

/// Reads the finding template `evidence::render` writes: a `**[Achado] title**` line, the detail
/// paragraph after it, and the suggested fix (or, for a question, what to confirm).
fn parse_body(body: &str) -> Parsed {
    let lines = body.lines().collect::<Vec<_>>();
    let heading = lines.iter().enumerate().find_map(|(index, line)| {
        let line = line.trim();
        let inner = line.strip_prefix("**")?.strip_suffix("**")?;
        let (kind, title) = crate::review::strip_point_marker(inner)?;
        Some((index, kind, title.trim()))
    });
    let Some((index, kind, title)) = heading else {
        return Parsed {
            kind: crate::review::findings::point_kind(body).into(),
            title: String::new(),
            detail: unescape(body.trim()),
            action: String::new(),
        };
    };
    let detail = lines[index + 1..]
        .iter()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty() && !line.trim_start().starts_with("<details>"))
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let action = lines
        .iter()
        .position(|line| {
            line.contains(&format!("{}</summary>", crate::review::FINDING_ACTION_SUMMARY))
                || line.contains(&format!("{}</summary>", crate::review::QUESTION_ACTION_SUMMARY))
                || line.contains("Suggested fix</summary>")
                || line.contains("What to confirm</summary>")
        })
        .map(|summary| {
            lines[summary + 1..]
                .iter()
                .take_while(|line| line.trim() != "</details>")
                .copied()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    Parsed {
        kind: kind.into(),
        title: unescape(title),
        detail: unescape(detail.trim()),
        action: unescape(action.trim()),
    }
}

/// Undoes `evidence::text`.
fn unescape(text: &str) -> String {
    text.replace("\\`", "`")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The head line that base line `old` of `path` corresponds to: the same line when it is
/// unchanged, or the head line where its deleted block used to be. `None` when the diff does not
/// touch `path`.
pub(crate) fn head_line(diff: &str, path: &str, old: i64) -> Option<i64> {
    let mut in_file = false;
    let mut touched = false;
    let mut delta = 0i64;
    let (mut old_at, mut new_at) = (0i64, 0i64);
    let mut in_hunk = false;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            if in_file {
                break;
            }
            in_hunk = false;
            continue;
        }
        if !in_hunk {
            if let Some(name) = line.strip_prefix("--- ").or_else(|| line.strip_prefix("+++ ")) {
                let name = name
                    .strip_prefix("a/")
                    .or_else(|| name.strip_prefix("b/"))
                    .unwrap_or(name);
                in_file |= name == path;
                continue;
            }
        }
        if !in_file {
            continue;
        }
        if let Some(header) = line.strip_prefix("@@ ") {
            let (old_start, new_start) = hunk_starts(header)?;
            if old < old_start {
                return Some((old + delta).max(1));
            }
            touched = true;
            in_hunk = true;
            old_at = old_start;
            new_at = new_start;
            continue;
        }
        if !in_hunk {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'-') => {
                if old_at == old {
                    return Some(new_at.max(1));
                }
                old_at += 1;
            }
            Some(b'+') => new_at += 1,
            Some(b'\\') => {}
            _ => {
                if old_at == old {
                    return Some(new_at.max(1));
                }
                old_at += 1;
                new_at += 1;
            }
        }
        delta = new_at - old_at;
    }
    touched.then_some((old + delta).max(1))
}

/// `-a,b +c,d @@` → (a, c).
fn hunk_starts(header: &str) -> Option<(i64, i64)> {
    let mut parts = header.split_whitespace();
    let start =
        |part: Option<&str>, sign: char| -> Option<i64> { part?.strip_prefix(sign)?.split(',').next()?.parse().ok() };
    Some((start(parts.next(), '-')?, start(parts.next(), '+')?))
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
