use super::*;

const MAX_RELATED_ISSUES: usize = 8;
const MAX_ISSUE_DESCRIPTION_CHARS: usize = 6_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelatedIssue {
    pub(crate) iid: i64,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) state: String,
    pub(crate) web_url: String,
    pub(crate) closes_on_merge: bool,
}

/// Issues linked to an MR via GitLab (`related_issues` + `closes_issues`).
/// Covers references in title, description, commits and discussions (`#123`,
/// `Closes #123`, `Relates to #123`, etc.). Best-effort: API failures return empty.
pub(crate) fn mr_related_issues(full_name: &str, number: i64) -> Vec<RelatedIssue> {
    let project = project_api_id(full_name);
    let related = repo::api_paginate(&format!(
        "projects/{project}/merge_requests/{number}/related_issues?per_page=100"
    ))
    .unwrap_or_default();
    let closes = repo::api_paginate(&format!(
        "projects/{project}/merge_requests/{number}/closes_issues?per_page=100"
    ))
    .unwrap_or_default();
    merge_issue_lists(related, closes)
        .into_iter()
        .take(MAX_RELATED_ISSUES)
        .collect()
}

pub(crate) fn merge_issue_lists(related: Vec<serde_json::Value>, closes: Vec<serde_json::Value>) -> Vec<RelatedIssue> {
    let mut by_iid = std::collections::BTreeMap::<i64, RelatedIssue>::new();
    for (value, closes_on_merge) in related
        .into_iter()
        .map(|value| (value, false))
        .chain(closes.into_iter().map(|value| (value, true)))
    {
        let Some(issue) = parse_related_issue(&value, closes_on_merge) else {
            continue;
        };
        by_iid
            .entry(issue.iid)
            .and_modify(|existing| {
                if closes_on_merge {
                    existing.closes_on_merge = true;
                }
                if existing.description.trim().is_empty() && !issue.description.trim().is_empty() {
                    existing.description = issue.description.clone();
                }
                if existing.web_url.is_empty() && !issue.web_url.is_empty() {
                    existing.web_url = issue.web_url.clone();
                }
            })
            .or_insert(issue);
    }
    by_iid.into_values().collect()
}

fn parse_related_issue(value: &serde_json::Value, closes_on_merge: bool) -> Option<RelatedIssue> {
    // GitLab issues use `iid`; external trackers (e.g. Jira) may only expose `id` as a string.
    let iid = value
        .get("iid")
        .and_then(|v| v.as_i64())
        .or_else(|| value.get("id").and_then(|v| v.as_i64()))
        .or_else(|| {
            value.get("id").and_then(|v| v.as_str()).and_then(|s| {
                s.chars()
                    .filter(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .ok()
            })
        })?;
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("(untitled issue)")
        .trim()
        .to_string();
    let description = value
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Some(RelatedIssue {
        iid,
        title,
        description: truncate_chars(&description, MAX_ISSUE_DESCRIPTION_CHARS),
        state: value
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        web_url: value
            .get("web_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        closes_on_merge,
    })
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out = text.chars().take(max).collect::<String>();
    out.push_str("\n\n…(issue description truncated)");
    out
}

/// Markdown for `RELATED_ISSUES.md`. Empty when there are no linked issues.
pub(crate) fn related_issues_markdown(issues: &[RelatedIssue]) -> Option<String> {
    if issues.is_empty() {
        return None;
    }
    let mut body = String::from(
        "# Related issues\n\n\
These GitLab issues are linked to this merge request (title/description references like `#123`, \
`Closes #123`, `Relates to #123`, commits, or discussions). Treat them as the original request. \
The MR description may restate the work; prefer the issue when they disagree about what was asked.\n",
    );
    for issue in issues {
        let link = if issue.closes_on_merge {
            "closes on merge"
        } else {
            "related"
        };
        let url = if issue.web_url.is_empty() {
            String::new()
        } else {
            format!(" · {}", issue.web_url)
        };
        body.push_str(&format!(
            "\n## #{iid} — {title}\n\nState: {state} · {link}{url}\n\n{description}\n",
            iid = issue.iid,
            title = issue.title,
            state = issue.state,
            description = if issue.description.trim().is_empty() {
                "(no description)"
            } else {
                issue.description.trim()
            }
        ));
    }
    Some(body)
}

/// Fetch linked issues and write `RELATED_ISSUES.md` when any exist. Never fails the review.
pub(crate) fn write_related_issues_context(full_name: &str, number: i64, context_dir: &std::path::Path) {
    let Some(markdown) = related_issues_markdown(&mr_related_issues(full_name, number)) else {
        return;
    };
    if let Err(error) = std::fs::write(context_dir.join("RELATED_ISSUES.md"), markdown) {
        eprintln!("could not write RELATED_ISSUES.md for {full_name}!{number}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_dedupes_and_marks_closes_on_merge() {
        let related = vec![serde_json::json!({
            "iid": 111,
            "title": "Add export",
            "description": "Need CSV export",
            "state": "opened",
            "web_url": "https://gitlab.example/x/-/issues/111"
        })];
        let closes = vec![serde_json::json!({
            "iid": 111,
            "title": "Add export",
            "description": "Need CSV export",
            "state": "opened",
            "web_url": "https://gitlab.example/x/-/issues/111"
        })];
        let merged = merge_issue_lists(related, closes);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].closes_on_merge);
        assert_eq!(merged[0].iid, 111);
    }

    #[test]
    fn markdown_lists_issues_and_empty_means_none() {
        assert!(related_issues_markdown(&[]).is_none());
        let md = related_issues_markdown(&[RelatedIssue {
            iid: 222,
            title: "Fix login".into(),
            description: "Users cannot sign in".into(),
            state: "opened".into(),
            web_url: "https://example/-/issues/222".into(),
            closes_on_merge: true,
        }])
        .unwrap();
        assert!(md.contains("# Related issues"));
        assert!(md.contains("## #222 — Fix login"));
        assert!(md.contains("closes on merge"));
        assert!(md.contains("Users cannot sign in"));
    }

    #[test]
    fn long_descriptions_are_truncated() {
        let issue = parse_related_issue(
            &serde_json::json!({
                "iid": 1,
                "title": "Huge",
                "description": "x".repeat(MAX_ISSUE_DESCRIPTION_CHARS + 50),
                "state": "opened"
            }),
            false,
        )
        .unwrap();
        assert!(issue.description.contains("truncated"));
        assert!(issue.description.chars().count() < MAX_ISSUE_DESCRIPTION_CHARS + 40);
    }

    #[test]
    fn related_covers_title_and_relates_to_style_links() {
        // related_issues API returns issues from title, description, commits, notes.
        let related = vec![
            serde_json::json!({
                "iid": 10,
                "title": "From MR title",
                "description": "asked in title",
                "state": "opened"
            }),
            serde_json::json!({
                "iid": 20,
                "title": "Relates to",
                "description": "mentioned as Relates to #20",
                "state": "opened"
            }),
        ];
        let merged = merge_issue_lists(related, Vec::new());
        assert_eq!(merged.iter().map(|i| i.iid).collect::<Vec<_>>(), vec![10, 20]);
        assert!(merged.iter().all(|i| !i.closes_on_merge));
    }
}
