use super::*;

pub(crate) fn activation_marker(full_name: &str, number: i64, installation: &str) -> String {
    format!("<!-- ocr-activation:v1 repo={full_name} pr={number} installation={installation} -->")
}

pub(crate) fn queued_for_review_text(full_name: &str, head: &str) -> String {
    format!("### ⏳ OpenCodeReview is reviewing this merge request\n\nQueued commit [`{}`](https://{}/{full_name}/-/commit/{head}). The results will appear here when the review finishes.\n\n{}", short_sha(head), gitlab::gitlab_host(), ocr_footer())
}

pub(crate) fn activation_confirmation_body(
    full_name: &str,
    number: i64,
    installation: &str,
    head: &str,
) -> (String, String) {
    let marker = activation_marker(full_name, number, installation);
    let body = format!("{marker}\n\n{}", queued_for_review_text(full_name, head));
    (marker, body)
}

#[cfg(test)]
pub(crate) fn scan_activation_pages(
    db: &Database,
    repo: &Repo,
    eligible: &HashMap<i64, (i64, String)>,
    started_at: i64,
    page: impl FnMut(&str, usize) -> Result<Vec<gitlab::ConversationComment>, String>,
    is_author: impl FnMut(i64, &gitlab::ConversationComment) -> Result<bool, String>,
) -> Result<(), String> {
    scan_activation_pages_with_lookup(db, repo, eligible, started_at, page, is_author, |_| Ok(None))
}

pub(crate) fn scan_activation_pages_with_lookup(
    db: &Database,
    repo: &Repo,
    eligible: &HashMap<i64, (i64, String)>,
    started_at: i64,
    mut page: impl FnMut(&str, usize) -> Result<Vec<gitlab::ConversationComment>, String>,
    mut is_author: impl FnMut(i64, &gitlab::ConversationComment) -> Result<bool, String>,
    mut resolve: impl FnMut(i64) -> Result<Option<(i64, String)>, String>,
) -> Result<(), String> {
    let (enabled_at, since) = db.comment_scan_cursor(repo.id)?;
    let mut errors = Vec::new();
    for page_number in 1.. {
        let comments = page(&since, page_number)?;
        let complete = comments.len() < 100;
        for comment in comments {
            let Some(number) = comment.command_number(&repo.full_name, enabled_at) else {
                continue;
            };
            if db.comment_activation_seen(repo.id, comment.id)? {
                continue;
            }
            let candidate = (|| {
                if !is_author(number, &comment)? {
                    return Ok(None);
                }
                if let Some(candidate) = eligible.get(&number) {
                    return Ok(Some(candidate.clone()));
                }
                resolve(number)
            })();
            let candidate = match candidate {
                Ok(candidate) => candidate,
                Err(error) => {
                    errors.push(error);
                    continue;
                }
            };
            let activation = candidate
                .map(|(pr_id, head)| {
                    let (marker, body) =
                        activation_confirmation_body(&repo.full_name, number, &db.installation_id()?, &head);
                    Ok::<_, String>((pr_id, head, marker, body))
                })
                .transpose()?;
            db.consume_activation_command(
                repo.id,
                enabled_at,
                comment.id,
                activation
                    .as_ref()
                    .map(|(id, head, marker, body)| (*id, head.as_str(), marker.as_str(), body.as_str())),
            )?;
        }
        if complete {
            break;
        }
    }
    if errors.is_empty() {
        db.finish_comment_scan(repo.id, started_at)
    } else {
        Err(errors.join("; "))
    }
}

pub(crate) fn poll_author_commands(
    state: &SharedState,
    repo: &Repo,
    eligible: &HashMap<i64, (i64, String)>,
    resolve: impl FnMut(i64) -> Result<Option<(i64, String)>, String>,
) -> Result<(), String> {
    let db = state.config.db.open()?;
    scan_activation_pages_with_lookup(
        &db,
        repo,
        eligible,
        crate::db::now_millis(),
        |since, page| gitlab::conversation_comments_page(&repo.full_name, since, page),
        |number, user| gitlab::unedited_author_command(&repo.full_name, number, user),
        resolve,
    )
}

pub(crate) fn author_activation_lane(
    draft_excluded: bool,
    activated: bool,
    filtered: bool,
    too_large: bool,
    automatic: bool,
) -> &'static str {
    if draft_excluded && !activated {
        "draft"
    } else if filtered {
        "auto_skipped"
    } else if too_large {
        "size_skipped"
    } else if automatic {
        "open"
    } else {
        "manual_required"
    }
}

#[cfg(test)]
#[path = "comment_activation_tests.rs"]
mod tests;
