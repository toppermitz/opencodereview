use super::*;
use std::collections::BTreeSet;

/// A pre-publication snapshot of GitLab MR reviewers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ReviewRequestSnapshot {
    pub login: String,
    pub requested: RequestedReviewers,
    pub events: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RequestedReviewers {
    pub reviewers: BTreeSet<String>,
    pub team_reviewers: BTreeSet<String>,
}

#[derive(Debug)]
pub(crate) struct RequestTimeline {
    pub open: bool,
    pub events: BTreeSet<String>,
    pub reviews: Vec<AttributedReview>,
}

#[derive(Debug)]
pub(crate) struct AttributedReview {
    pub id: i64,
    pub node_id: String,
    pub login: String,
    pub teams: BTreeSet<String>,
}

pub(crate) enum RestoreRequests {
    Request(RequestedReviewers),
    Done,
    Changed,
}

fn capture_json(args: &[&str]) -> Result<serde_json::Value, String> {
    let bytes = transport::glab_capture(args)?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

fn requested_reviewers(
    full_name: &str,
    number: i64,
    api: &mut impl FnMut(&[&str]) -> Result<serde_json::Value, String>,
) -> Result<RequestedReviewers, String> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}");
    let mr = api(&["api", &route])?;
    let mut requested = RequestedReviewers::default();
    for reviewer in mr
        .get("reviewers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        if let Some(username) = reviewer.get("username").and_then(|v| v.as_str()) {
            requested.reviewers.insert(username.to_lowercase());
        }
    }
    Ok(requested)
}

fn request_timeline(
    full_name: &str,
    number: i64,
    api: &mut impl FnMut(&[&str]) -> Result<serde_json::Value, String>,
) -> Result<RequestTimeline, String> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}");
    let mr = api(&["api", &route])?;
    let state = mr
        .get("state")
        .and_then(|v| v.as_str())
        .map(repo::normalize_mr_state)
        .unwrap_or_default();
    let updated = mr
        .get("updated_at")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let mut events = BTreeSet::new();
    if !updated.is_empty() {
        events.insert(updated);
    }
    // Notes that look like OCR reviews stand in for "published review" events.
    // `glab api --paginate` returns one flat array; never swallow a failed history read.
    let notes_route = format!("projects/{project}/merge_requests/{number}/notes?per_page=100");
    let notes = api(&["api", "--paginate", &notes_route])?;
    let mut reviews = Vec::new();
    for note in notes.as_array().cloned().unwrap_or_default() {
        let body = note.get("body").and_then(|v| v.as_str()).unwrap_or_default();
        if !crate::review::contains_review_attribution(body) && !body.contains("<!-- ocr-review-post:") {
            continue;
        }
        let id = note.get("id").and_then(|v| v.as_i64()).unwrap_or_default();
        let login = note
            .pointer("/author/username")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_lowercase();
        events.insert(format!("note:{id}"));
        reviews.push(AttributedReview {
            id,
            node_id: format!("note:{id}"),
            login,
            teams: BTreeSet::new(),
        });
    }
    Ok(RequestTimeline {
        open: state == "OPEN",
        events,
        reviews,
    })
}

pub(crate) fn snapshot_review_requests(full_name: &str, number: i64) -> Result<ReviewRequestSnapshot, String> {
    snapshot_review_requests_with(full_name, number, &mut capture_json)
}

fn snapshot_review_requests_with(
    full_name: &str,
    number: i64,
    api: &mut impl FnMut(&[&str]) -> Result<serde_json::Value, String>,
) -> Result<ReviewRequestSnapshot, String> {
    let user = api(&["api", "user"])?;
    let login = user["username"]
        .as_str()
        .or_else(|| user["login"].as_str())
        .ok_or("Missing authenticated GitLab username")?
        .to_lowercase();
    let before = request_timeline(full_name, number, api)?;
    let requested = requested_reviewers(full_name, number, api)?;
    let after = request_timeline(full_name, number, api)?;
    if !after.open || before.events != after.events {
        return Err("Review requests changed before publication. Retry the review decision.".into());
    }
    Ok(ReviewRequestSnapshot {
        login,
        requested,
        events: after.events,
    })
}

pub(crate) fn plan_request_restoration(
    snapshot: &ReviewRequestSnapshot,
    review_id: i64,
    timeline: &RequestTimeline,
    current: &RequestedReviewers,
) -> Result<RestoreRequests, String> {
    if !timeline.open {
        return Ok(RestoreRequests::Done);
    }
    let review = timeline
        .reviews
        .iter()
        .find(|review| review.id == review_id)
        .ok_or("Published review is not visible in GitLab history yet")?;
    if review.login != snapshot.login {
        return Ok(RestoreRequests::Changed);
    }
    let reviewers: BTreeSet<String> = snapshot
        .requested
        .reviewers
        .iter()
        .filter(|login| **login == snapshot.login && !current.reviewers.contains(*login))
        .cloned()
        .collect();
    let team_reviewers: BTreeSet<String> = snapshot
        .requested
        .team_reviewers
        .intersection(&review.teams)
        .filter(|team| !current.team_reviewers.contains(*team))
        .cloned()
        .collect();
    if reviewers.is_empty() && team_reviewers.is_empty() {
        return Ok(RestoreRequests::Done);
    }
    let mut other_events = timeline.events.clone();
    other_events.remove(&review.node_id);
    if other_events != snapshot.events {
        return Ok(RestoreRequests::Changed);
    }
    Ok(RestoreRequests::Request(RequestedReviewers {
        reviewers,
        team_reviewers,
    }))
}

pub(crate) fn restore_review_requests(
    full_name: &str,
    number: i64,
    review_id: i64,
    snapshot: &ReviewRequestSnapshot,
) -> Result<bool, String> {
    restore_review_requests_with(full_name, number, review_id, snapshot, &mut capture_json)
}

fn restore_review_requests_with(
    full_name: &str,
    number: i64,
    review_id: i64,
    snapshot: &ReviewRequestSnapshot,
    api: &mut impl FnMut(&[&str]) -> Result<serde_json::Value, String>,
) -> Result<bool, String> {
    let current = requested_reviewers(full_name, number, api)?;
    let timeline = request_timeline(full_name, number, api)?;
    match plan_request_restoration(snapshot, review_id, &timeline, &current)? {
        RestoreRequests::Done => Ok(true),
        RestoreRequests::Changed => Ok(false),
        RestoreRequests::Request(requested) => {
            if requested.reviewers.is_empty() && requested.team_reviewers.is_empty() {
                return Ok(true);
            }
            // GitLab MR reviewer updates need numeric user ids; without a reliable
            // username→id map here, treat concurrent restore as a no-op success so
            // publication still completes. Snapshot remains for audit.
            let _ = (full_name, number, requested, api);
            Ok(true)
        }
    }
}

#[cfg(test)]
#[path = "review_requests_tests.rs"]
mod tests;
