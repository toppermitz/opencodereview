use super::*;

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn fixture() -> (ReviewRequestSnapshot, RequestTimeline) {
    (
        ReviewRequestSnapshot {
            login: "reviewer".into(),
            requested: RequestedReviewers {
                reviewers: set(&["reviewer", "other"]),
                team_reviewers: set(&["backend", "frontend"]),
            },
            events: set(&["original-request"]),
        },
        RequestTimeline {
            open: true,
            events: set(&["original-request", "note:42"]),
            reviews: vec![AttributedReview {
                id: 42,
                node_id: "note:42".into(),
                login: "reviewer".into(),
                teams: set(&["backend", "not-requested"]),
            }],
        },
    )
}

#[test]
fn restores_only_consumed_authenticated_user_and_attributed_requested_teams() {
    let (snapshot, timeline) = fixture();
    let RestoreRequests::Request(plan) =
        plan_request_restoration(&snapshot, 42, &timeline, &RequestedReviewers::default()).unwrap()
    else {
        panic!("expected restoration");
    };
    assert_eq!(plan.reviewers, set(&["reviewer"]));
    assert_eq!(plan.team_reviewers, set(&["backend"]));
}

#[test]
fn does_not_request_reviewers_already_pending() {
    let (snapshot, timeline) = fixture();
    assert!(matches!(
        plan_request_restoration(&snapshot, 42, &timeline, &snapshot.requested).unwrap(),
        RestoreRequests::Done
    ));
}

#[test]
fn concurrent_review_request_removal_or_addition_wins_even_by_same_user() {
    for event in ["human-review", "request-removed", "new-request", "review-dismissed"] {
        let (snapshot, mut timeline) = fixture();
        timeline.events.insert(event.into());
        assert!(matches!(
            plan_request_restoration(&snapshot, 42, &timeline, &RequestedReviewers::default()).unwrap(),
            RestoreRequests::Changed
        ));
    }
}

#[test]
fn missing_history_identity_or_closed_pr_never_restores() {
    let (snapshot, mut timeline) = fixture();
    timeline.events.remove("original-request");
    assert!(matches!(
        plan_request_restoration(&snapshot, 42, &timeline, &RequestedReviewers::default()).unwrap(),
        RestoreRequests::Changed
    ));
    let (snapshot, mut timeline) = fixture();
    timeline.reviews[0].login = "different-user".into();
    assert!(matches!(
        plan_request_restoration(&snapshot, 42, &timeline, &RequestedReviewers::default()).unwrap(),
        RestoreRequests::Changed
    ));
    timeline.open = false;
    assert!(matches!(
        plan_request_restoration(&snapshot, 42, &timeline, &RequestedReviewers::default()).unwrap(),
        RestoreRequests::Done
    ));
}

#[test]
fn missing_published_review_is_retryable_without_another_publication() {
    let (snapshot, timeline) = fixture();
    assert!(plan_request_restoration(&snapshot, 99, &timeline, &RequestedReviewers::default()).is_err());
}

#[test]
fn no_attribution_and_no_user_request_never_adds_a_request() {
    let (mut snapshot, mut timeline) = fixture();
    snapshot.requested.reviewers.clear();
    timeline.reviews[0].teams.clear();
    assert!(matches!(
        plan_request_restoration(&snapshot, 42, &timeline, &RequestedReviewers::default()).unwrap(),
        RestoreRequests::Done
    ));
}

fn mr(state: &str, reviewers: &[&str], updated_at: &str) -> serde_json::Value {
    serde_json::json!({
        "state": state,
        "updated_at": updated_at,
        "reviewers": reviewers.iter().map(|u| serde_json::json!({"username": u})).collect::<Vec<_>>(),
    })
}

fn notes_pages(bodies: &[(&str, i64, &str)]) -> serde_json::Value {
    // Flat array matching `glab api --paginate` (no --slurp).
    let notes: Vec<_> = bodies
        .iter()
        .map(|(body, id, user)| {
            serde_json::json!({
                "id": id,
                "body": body,
                "author": {"username": user}
            })
        })
        .collect();
    serde_json::Value::Array(notes)
}

#[test]
fn snapshot_requires_stable_history_and_complete_reads_before_publication() {
    let attribution = format!("{}\nreview", crate::review::REVIEW_ATTRIBUTION);
    let mut replies = vec![
        serde_json::json!({"username":"Reviewer"}),
        mr("opened", &["Reviewer", "other"], "t1"),
        notes_pages(&[(&attribution, 1, "reviewer")]),
        mr("opened", &["Reviewer", "other"], "t1"),
        mr("opened", &["Reviewer", "other"], "t1"),
        notes_pages(&[(&attribution, 1, "reviewer")]),
    ]
    .into_iter();
    let snapshot = snapshot_review_requests_with("o/r", 1, &mut |_| Ok(replies.next().unwrap())).unwrap();
    assert_eq!(snapshot.login, "reviewer");
    assert!(snapshot.requested.reviewers.contains("reviewer"));
    assert!(snapshot_review_requests_with("o/r", 1, &mut |_| Err("offline".into())).is_err());
}

#[test]
fn restore_noops_when_reviewers_already_match() {
    let (snapshot, _) = fixture();
    let attribution = format!("{}\nreview", crate::review::REVIEW_ATTRIBUTION);
    let mr_body = mr("opened", &["reviewer"], "original-request");
    let notes = notes_pages(&[(&attribution, 42, "reviewer")]);
    assert!(restore_review_requests_with("o/r", 1, 42, &snapshot, &mut |args| {
        if args.iter().any(|arg| arg.contains("/notes")) {
            Ok(notes.clone())
        } else {
            Ok(mr_body.clone())
        }
    })
    .unwrap());
}

#[test]
fn restore_skips_when_history_changed() {
    let (snapshot, _) = fixture();
    let attribution = format!("{}\nreview", crate::review::REVIEW_ATTRIBUTION);
    let mr_body = mr("opened", &[], "human-changed-the-mr");
    let notes = notes_pages(&[(&attribution, 42, "reviewer")]);
    assert!(!restore_review_requests_with("o/r", 1, 42, &snapshot, &mut |args| {
        if args.iter().any(|arg| arg.contains("/notes")) {
            Ok(notes.clone())
        } else {
            Ok(mr_body.clone())
        }
    })
    .unwrap());
}
