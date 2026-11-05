use super::*;
use crate::gitlab::StatusCommentApi;

const STATUS_PUBLICATION_PREFIX: &str = "<!-- ocr-status-publication:v1 run=";
const STATUS_PUBLICATION_SUFFIX: &str = " -->";

pub(crate) fn status_publication_marker(run_id: i64) -> String {
    format!("{STATUS_PUBLICATION_PREFIX}{run_id}{STATUS_PUBLICATION_SUFFIX}")
}

/// Parses one comment body: `Some(Some(run))` for this installation's clean
/// result comment carrying a run marker, `Some(None)` for a legacy clean result
/// comment posted before run markers existed, and `None` for everything else
/// (other markers or installations, findings state, quoted markers, human
/// comments).
pub(crate) fn owned_clean_comment_run(body: &str, ownership_marker: &str) -> Option<Option<i64>> {
    let mut lines = body.lines();
    if lines.next()? != ownership_marker {
        return None;
    }
    let data = lines.next()?;
    if !data.starts_with("<!-- ocr-result-data:v1 ")
        || !data.ends_with(" -->")
        || !data.split_whitespace().any(|field| field == "state=clean")
    {
        return None;
    }
    let third = lines.next()?;
    if third.is_empty() {
        return Some(None);
    }
    third
        .strip_prefix(STATUS_PUBLICATION_PREFIX)
        .and_then(|rest| rest.strip_suffix(STATUS_PUBLICATION_SUFFIX))
        .and_then(|run| run.parse::<i64>().ok())
        .map(Some)
}

/// Maps a comment body to `Some(Some(generation))` for a generation-marked
/// family comment, `Some(None)` for a legacy family comment, or `None` for a
/// body outside the family.
type CommentClassifier<'a> = &'a dyn Fn(&str) -> Option<Option<i64>>;

/// Lists the comments a family owns on a PR: those authored by the current
/// GitHub account whose body `classify` recognizes.
fn owned_comment_generations(
    api: &mut impl StatusCommentApi,
    repo: &str,
    number: i64,
    classify: CommentClassifier<'_>,
) -> Result<Vec<(i64, Option<i64>)>, String> {
    let publisher = api.publisher_id()?;
    let comments = api.list(repo, number)?;
    Ok(comments
        .into_iter()
        .filter(|comment| comment.author_id == publisher)
        .filter_map(|comment| classify(&comment.body).map(|run| (comment.id, run)))
        .collect())
}

pub(crate) fn find_owned_comment(
    api: &mut impl StatusCommentApi,
    repo: &str,
    number: i64,
    classify: CommentClassifier<'_>,
    generation: i64,
) -> Result<Option<i64>, String> {
    Ok(owned_comment_generations(api, repo, number, classify)?
        .into_iter()
        .filter(|(_, run)| *run == Some(generation))
        .map(|(id, _)| id)
        .min())
}

/// Adopts the lowest-id comment already posted for this generation instead of
/// posting again, so a retry after a lost POST response cannot duplicate. Then
/// deletes the family's remaining legacy and same-or-older generation comments;
/// comments from newer generations are never deleted. A delete failure aborts
/// the call, and the next attempt adopts the posted comment and finishes the
/// deletions without posting again.
pub(crate) fn replace_owned_comment(
    api: &mut impl StatusCommentApi,
    repo: &str,
    number: i64,
    classify: CommentClassifier<'_>,
    generation: i64,
    body: &str,
) -> Result<i64, String> {
    let owned = owned_comment_generations(api, repo, number, classify)?;
    let adopted = owned
        .iter()
        .filter(|(_, run)| *run == Some(generation))
        .map(|(id, _)| *id)
        .min();
    let kept = match adopted {
        Some(id) => id,
        None => api.post(repo, number, body)?,
    };
    for (id, run) in owned {
        if id != kept && run.map(|run| run <= generation).unwrap_or(true) {
            api.delete(repo, number, id)?;
        }
    }
    Ok(kept)
}

/// Deletes owned family comments from before `generation` (legacy comments and
/// older generations). Comments from this or newer generations are kept.
pub(crate) fn delete_owned_comments_before(
    api: &mut impl StatusCommentApi,
    repo: &str,
    number: i64,
    classify: CommentClassifier<'_>,
    generation: i64,
) -> Result<(), String> {
    for (id, run) in owned_comment_generations(api, repo, number, classify)? {
        if run.map(|run| run < generation).unwrap_or(true) {
            api.delete(repo, number, id)?;
        }
    }
    Ok(())
}

/// Stamps a legacy-format body (reserved before run markers existed) with the
/// publication marker so a retry can adopt its posted comment by run instead
/// of posting again. Any other body is returned unchanged.
pub(crate) fn with_status_publication_marker(body: &str, ownership_marker: &str, run_id: i64) -> String {
    if owned_clean_comment_run(body, ownership_marker) != Some(None) {
        return body.to_string();
    }
    let Some((head, visible)) = body.split_once("\n\n") else {
        return body.to_string();
    };
    format!("{head}\n{}\n\n{visible}", status_publication_marker(run_id))
}

fn clean_classifier(ownership_marker: &str) -> impl Fn(&str) -> Option<Option<i64>> + '_ {
    move |body| owned_clean_comment_run(body, ownership_marker)
}

pub(crate) fn find_clean_comment(
    api: &mut impl StatusCommentApi,
    repo: &str,
    number: i64,
    ownership_marker: &str,
    run_id: i64,
) -> Result<Option<i64>, String> {
    find_owned_comment(api, repo, number, &clean_classifier(ownership_marker), run_id)
}

pub(crate) fn replace_clean_comment(
    api: &mut impl StatusCommentApi,
    repo: &str,
    number: i64,
    ownership_marker: &str,
    run_id: i64,
    body: &str,
) -> Result<i64, String> {
    replace_owned_comment(api, repo, number, &clean_classifier(ownership_marker), run_id, body)
}

pub(crate) fn retire_clean_comments_before(
    api: &mut impl StatusCommentApi,
    repo: &str,
    number: i64,
    ownership_marker: &str,
    findings_run_id: i64,
) -> Result<(), String> {
    delete_owned_comments_before(api, repo, number, &clean_classifier(ownership_marker), findings_run_id)
}

/// Best-effort cleanup once a findings review is accepted on GitHub: earlier
/// clean result comments are stale. Ignores every failure, so it can never
/// change the outcome of the findings flow; the next clean publication removes
/// any leftover comments.
pub(crate) fn retire_clean_comments_after_findings(db: &Database, full_name: &str, number: i64, findings_run_id: i64) {
    let enabled = db
        .settings()
        .map(|settings| settings.review_result_mode == "result_comment")
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let Ok(installation_id) = db.installation_id() else {
        return;
    };
    let marker = crate::db::result_marker_key(full_name, number, &installation_id);
    let _ = retire_clean_comments_before(
        &mut gitlab::GitLabStatusComments,
        full_name,
        number,
        &marker,
        findings_run_id,
    );
}

#[cfg(test)]
#[path = "status_comments_test_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "status_comments_tests.rs"]
mod tests;
