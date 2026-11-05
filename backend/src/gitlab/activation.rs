use super::*;

#[derive(Debug, Deserialize)]
pub(crate) struct ConversationComment {
    pub id: i64,
    #[allow(dead_code)]
    pub node_id: String,
    pub body: String,
    pub created_at: String,
    pub updated_at: String,
    pub issue_url: String,
    pub user: CommentAuthor,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CommentAuthor {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: String,
}

impl ConversationComment {
    pub(crate) fn command_number(&self, full_name: &str, enabled_at: i64) -> Option<i64> {
        if self.body.trim() != "/ocr review"
            || self.created_at != self.updated_at
            || self.user.kind != "User"
            || self.user.id <= 0
            || iso8601_to_epoch_ms(&self.created_at)? <= enabled_at
        {
            return None;
        }
        // Prefer GitLab MR web URLs: https://host/group/project/-/merge_requests/N
        if let Some(number) = self
            .issue_url
            .rsplit("/merge_requests/")
            .next()
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|n| n.parse::<i64>().ok())
            .filter(|n| *n > 0)
        {
            return Some(number);
        }
        // Fallback: GitHub-shaped paths kept for fixtures during migration tests.
        let (_, path) = self.issue_url.split_once("://")?;
        let path = path.split_once('/')?.1;
        let number = path
            .strip_prefix(&format!("repos/{full_name}/issues/"))
            .or_else(|| path.strip_prefix(&format!("api/v4/projects/")))?;
        number.parse::<i64>().ok().filter(|n| *n > 0)
    }
}

pub(crate) fn conversation_comments_page(
    full_name: &str,
    since: &str,
    page: usize,
) -> Result<Vec<ConversationComment>, String> {
    // The activation scanner pages until a short page. GitLab notes are fetched
    // completely on the first call so the scan cursor only advances after every
    // open MR's notes have been read (or a hard error aborts the poll).
    if page != 1 {
        return Ok(Vec::new());
    }
    let project = project_api_id(full_name);
    let mrs = repo::api_paginate(&format!(
        "projects/{project}/merge_requests?state=opened&order_by=updated_at&sort=desc&per_page=100"
    ))?;
    let mut comments = Vec::new();
    for mr in mrs {
        let iid = mr.get("iid").and_then(|v| v.as_i64()).unwrap_or_default();
        let web_url = mr
            .get("web_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if iid <= 0 {
            continue;
        }
        let notes = repo::api_paginate(&format!(
            "projects/{project}/merge_requests/{iid}/notes?sort=asc&order_by=updated_at&per_page=100"
        ))?;
        for note in notes {
            let updated_at = note
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !since.is_empty() && updated_at.as_str() < since {
                continue;
            }
            let id = note.get("id").and_then(|v| v.as_i64()).unwrap_or_default();
            let body = note
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let created_at = note
                .get("created_at")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let author_id = note.pointer("/author/id").and_then(|v| v.as_i64()).unwrap_or_default();
            comments.push(ConversationComment {
                id,
                node_id: id.to_string(),
                body,
                created_at,
                updated_at,
                issue_url: web_url.clone(),
                user: CommentAuthor {
                    id: author_id,
                    kind: "User".into(),
                },
            });
        }
    }
    Ok(comments)
}

pub(crate) fn activation_pr(full_name: &str, number: i64) -> Result<Option<GhPullRequest>, String> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}");
    let bytes = transport::glab_capture(&["api", &route])?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    match value
        .get("state")
        .and_then(|v| v.as_str())
        .map(repo::normalize_mr_state)
    {
        Some(state) if state == "OPEN" => {
            let mr = repo::parse_merge_request(&value)?;
            Ok(Some(mr))
        }
        Some(_) => Ok(None),
        None => Err("Missing merge request state".into()),
    }
}

pub(crate) fn is_pr_author(full_name: &str, number: i64, user_id: i64) -> Result<bool, String> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}");
    let bytes = transport::glab_capture(&["api", &route])?;
    let mr: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if !mr_author_response(&mr, user_id) {
        return Ok(false);
    }
    // Require Developer+ membership on the project (access_level >= 30).
    let member_route = format!("projects/{project}/members/all/{user_id}");
    match transport::glab_capture(&["api", &member_route]) {
        Ok(bytes) => {
            let member: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            Ok(member
                .get("access_level")
                .and_then(|v| v.as_i64())
                .is_some_and(|level| level >= 30))
        }
        Err(_) => Ok(false),
    }
}

fn mr_author_response(mr: &serde_json::Value, user_id: i64) -> bool {
    matches!(
        mr.get("state")
            .and_then(|v| v.as_str())
            .map(repo::normalize_mr_state)
            .as_deref(),
        Some("OPEN")
    ) && mr.pointer("/author/id").and_then(|v| v.as_i64()) == Some(user_id)
}

pub(crate) fn unedited_author_command(
    full_name: &str,
    number: i64,
    comment: &ConversationComment,
) -> Result<bool, String> {
    if comment.created_at != comment.updated_at || comment.body.trim() != "/ocr review" {
        return Ok(false);
    }
    is_pr_author(full_name, number, comment.user.id)
}

#[cfg(test)]
pub(crate) fn unedited_command_response(value: &serde_json::Value) -> Result<bool, String> {
    // Kept for unit tests that still feed GraphQL-shaped fixtures.
    if value.get("errors").is_some() {
        return Err("Could not verify command edit history".into());
    }
    let Some(node) = value.pointer("/data/node").filter(|v| !v.is_null()) else {
        return Ok(false);
    };
    let edited = node.get("lastEditedAt").ok_or("Missing command edit history")?;
    Ok(edited.is_null()
        && node
            .get("body")
            .and_then(|v| v.as_str())
            .is_some_and(|body| body.trim() == "/ocr review"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_requires_open_merge_request_author() {
        let mr = serde_json::json!({"state":"opened", "author":{"id":123}});
        assert!(mr_author_response(&mr, 123));
        assert!(!mr_author_response(&mr, 456));
        assert!(!mr_author_response(
            &serde_json::json!({"state":"closed","author":{"id":123}}),
            123
        ));
        assert!(!mr_author_response(
            &serde_json::json!({"state":"merged","author":{"id":123}}),
            123
        ));
    }

    #[test]
    fn activation_rejects_same_second_edit_and_missing_history() {
        assert!(unedited_command_response(
            &serde_json::json!({"data":{"node":{"body":" /ocr review\n", "lastEditedAt":null}}})
        )
        .unwrap());
        assert!(!unedited_command_response(
            &serde_json::json!({"data":{"node":{"body":"/ocr review", "lastEditedAt":"2026-09-21T01:00:00Z"}}})
        )
        .unwrap());
        assert!(!unedited_command_response(&serde_json::json!({"data":{"node":null}})).unwrap());
        assert!(unedited_command_response(&serde_json::json!({"data":{"node":{"body":"/ocr review"}}})).is_err());
        assert!(unedited_command_response(&serde_json::json!({"errors":[{}]})).is_err());
    }

    #[test]
    fn command_number_parses_gitlab_mr_urls() {
        let comment = ConversationComment {
            id: 1,
            node_id: "1".into(),
            body: "/ocr review".into(),
            created_at: "2026-01-01T00:00:01Z".into(),
            updated_at: "2026-01-01T00:00:01Z".into(),
            issue_url: "https://gitlab.example.com/acme/api/-/merge_requests/42".into(),
            user: CommentAuthor {
                id: 9,
                kind: "User".into(),
            },
        };
        assert_eq!(comment.command_number("acme/api", 0), Some(42));
    }
}
