use super::*;
static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    db: Database,
    repo: Repo,
    pr_id: i64,
}
impl Fixture {
    fn new(lane: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "ocr-activation-{}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
        db.ensure_temp_repo_for_index(1, "acme/repo").unwrap();
        db.save_repo_subscription(1, true, "manual", true, false).unwrap();
        let repo = db.repo_by_id(1).unwrap();
        let pr_id = db
            .upsert_pull_request(&crate::db::PullRequestInput {
                repo_id: 1,
                number: 7,
                title: "Feature".into(),
                author: "author".into(),
                author_avatar_url: None,
                is_draft: true,
                head_sha: "abc1234567".into(),
                base_sha: Some("base".into()),
                base_ref: Some("main".into()),
                head_ref: Some("feature".into()),
                default_branch: Some("main".into()),
                url: None,
                lane: lane.into(),
                status_reason: None,
            })
            .unwrap();
        Self { root, db, repo, pr_id }
    }
    fn eligible(&self) -> HashMap<i64, (i64, String)> {
        HashMap::from([(7, (self.pr_id, "abc1234567".into()))])
    }
    fn scan(&self, comments: Vec<gitlab::ConversationComment>, author: bool) -> Result<(), String> {
        let mut comments = Some(comments);
        scan_activation_pages(
            &self.db,
            &self.repo,
            &self.eligible(),
            crate::db::now_millis(),
            |_, _| Ok(comments.take().unwrap()),
            |_, _| Ok(author),
        )
    }
    fn activation_marker_body(&self) -> (String, String) {
        crate::db::Db::for_test_at(self.root.join("test.sqlite"))
            .open()
            .unwrap()
            .query_row(
                "SELECT marker, body FROM pr_comment_activations WHERE pr_id = $1",
                [self.pr_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn comment(id: i64) -> gitlab::ConversationComment {
    serde_json::from_value(serde_json::json!({"id":id,"node_id":format!("node{id}"),"body":"/ocr review","created_at":"2099-01-01T00:00:00Z","updated_at":"2099-01-01T00:00:00Z","issue_url":"https://api.github.com/repos/acme/repo/issues/7","user":{"id":123,"type":"User"}})).unwrap()
}

#[test]
fn activation_exact_parser_rejects_edits_history_bots_and_other_routes() {
    for body in [
        "/ocr review now",
        "please /ocr review",
        "/OpenCodeReview review",
        "/ocr  review",
        "```\n/ocr review\n```",
        "/ocr review\nmore",
    ] {
        let mut c = comment(1);
        c.body = body.into();
        assert_eq!(c.command_number("acme/repo", 0), None);
    }
    let mut c = comment(1);
    c.body = " \n/ocr review\t".into();
    assert_eq!(c.command_number("acme/repo", 0), Some(7));
    c.updated_at = "2099-01-01T00:00:01Z".into();
    assert_eq!(c.command_number("acme/repo", 0), None);
    let mut c = comment(1);
    c.user.kind = "Bot".into();
    assert_eq!(c.command_number("acme/repo", 0), None);
    assert_eq!(comment(1).command_number("other/repo", 0), None);
    assert_eq!(
        comment(1).command_number(
            "acme/repo",
            gitlab::iso8601_to_epoch_ms("2099-01-01T00:00:00Z").unwrap()
        ),
        None
    );
}

#[test]
fn activation_requires_author_and_persists_queue_and_marker() {
    let f = Fixture::new("draft");
    f.scan(vec![comment(1)], false).unwrap();
    assert!(!f.db.pr_comment_activated(1, 7).unwrap());
    f.scan(vec![comment(2)], true).unwrap();
    assert_eq!(f.db.queued_pr_ids().unwrap(), vec![f.pr_id]);
    let (marker, body) = f.activation_marker_body();
    assert!(marker.contains("ocr-activation:v1"));
    assert!(body.contains("### ⏳ OpenCodeReview is reviewing this merge request"));
    assert!(body.contains("[`abc12345`](https://gitlab.example.com/acme/repo/-/commit/abc1234567)"));
    let reopened = crate::db::Db::for_test_at(f.root.join("test.sqlite")).open().unwrap();
    assert!(reopened.pr_comment_activated(1, 7).unwrap());
    assert_eq!(reopened.queued_pr_ids().unwrap(), vec![f.pr_id]);
    drop(reopened);
    // A fresh command on a decided PR neither re-admits nor rewrites the receipt.
    f.db.set_pr_lane(f.pr_id, "reviewed_cleanly", None).unwrap();
    f.scan(vec![comment(2), comment(3)], true).unwrap();
    assert!(f.db.queued_pr_ids().unwrap().is_empty());
    assert_eq!(f.activation_marker_body(), (marker.clone(), body.clone()));
    f.scan(vec![comment(4)], true).unwrap();
    assert_eq!(f.activation_marker_body(), (marker, body));
}

#[test]
fn activation_never_restarts_existing_admitted_work() {
    for lane in [
        "queued",
        "reviewing",
        "ready",
        "posted",
        "reviewed_cleanly",
        "approved",
        "failed",
        "closed",
        "size_skipped",
    ] {
        let f = Fixture::new(lane);
        f.scan(vec![comment(1)], true).unwrap();
        assert!(!f.db.pr_comment_activated(1, 7).unwrap(), "{lane}");
    }
    let f = Fixture::new("manual_required");
    f.scan(vec![comment(1)], true).unwrap();
    assert!(f.db.pr_comment_activated(1, 7).unwrap());
}

#[test]
fn activation_failed_page_retains_checkpoint_and_replays_without_duplicate_admission() {
    let f = Fixture::new("draft");
    let before = f.db.comment_scan_cursor(1).unwrap();
    let started = crate::db::now_millis() + 120_000;
    let error = scan_activation_pages(
        &f.db,
        &f.repo,
        &f.eligible(),
        started,
        |since, page| {
            assert_eq!(since, before.1);
            if page == 1 {
                Ok((1..=100).map(comment).collect())
            } else {
                Err("page 2 failed".into())
            }
        },
        |_, _| Ok(true),
    );
    assert!(error.is_err());
    assert_eq!(f.db.comment_scan_cursor(1).unwrap(), before);
    assert!(f.db.pr_comment_activated(1, 7).unwrap());
    let mut pages = Vec::new();
    scan_activation_pages(
        &f.db,
        &f.repo,
        &f.eligible(),
        started,
        |_, page| {
            pages.push(page);
            Ok(if page == 1 {
                (1..=100).map(comment).collect()
            } else {
                vec![comment(101)]
            })
        },
        |_, _| Ok(true),
    )
    .unwrap();
    assert_eq!(pages, vec![1, 2]);
    assert_ne!(f.db.comment_scan_cursor(1).unwrap().1, before.1);
    assert!(f.db.pr_comment_activated(1, 7).unwrap());
    assert_eq!(f.db.queued_pr_ids().unwrap(), vec![f.pr_id]);
}

#[test]
fn activation_filters_and_disabled_repositories_keep_queue_gated() {
    let f = Fixture::new("draft");
    scan_activation_pages(
        &f.db,
        &f.repo,
        &HashMap::new(),
        crate::db::now_millis(),
        |_, _| Ok(vec![comment(1)]),
        |_, _| Ok(true),
    )
    .unwrap();
    f.scan(vec![comment(1)], true).unwrap();
    assert!(!f.db.pr_comment_activated(1, 7).unwrap());
    f.scan(vec![comment(2)], true).unwrap();
    f.db.save_repo_subscription(1, false, "manual", true, false).unwrap();
    assert!(f.db.queued_pr_ids().unwrap().is_empty());
    for automatic in [false, true] {
        assert_eq!(
            author_activation_lane(true, true, true, false, automatic),
            "auto_skipped"
        );
        assert_eq!(
            author_activation_lane(true, true, false, true, automatic),
            "size_skipped"
        );
        assert_eq!(author_activation_lane(true, false, false, false, automatic), "draft");
        assert_eq!(
            author_activation_lane(true, true, false, false, automatic),
            if automatic { "open" } else { "manual_required" }
        );
    }
}

#[test]
fn activation_failed_identity_read_does_not_consume_or_checkpoint() {
    let f = Fixture::new("draft");
    let before = f.db.comment_scan_cursor(1).unwrap();
    assert!(scan_activation_pages(
        &f.db,
        &f.repo,
        &f.eligible(),
        crate::db::now_millis() + 120_000,
        |_, _| Ok(vec![comment(1)]),
        |_, _| Err("identity unavailable".into())
    )
    .is_err());
    assert!(!f.db.comment_activation_seen(1, 1).unwrap());
    assert_eq!(f.db.comment_scan_cursor(1).unwrap(), before);
}

#[test]
fn activation_survives_draft_rebase_without_forcing_future_manual_reviews() {
    let f = Fixture::new("draft");
    f.scan(vec![comment(1)], true).unwrap();
    let before = f.activation_marker_body();
    f.db.upsert_pull_request(&crate::db::PullRequestInput {
        repo_id: 1,
        number: 7,
        title: "Rebased feature".into(),
        author: "author".into(),
        author_avatar_url: None,
        is_draft: true,
        head_sha: "rebased-head".into(),
        base_sha: Some("new-base".into()),
        base_ref: Some("stack-parent".into()),
        head_ref: Some("feature".into()),
        default_branch: Some("main".into()),
        url: None,
        lane: "manual_required".into(),
        status_reason: None,
    })
    .unwrap();
    let lane = author_activation_lane(true, f.db.pr_comment_activated(1, 7).unwrap(), false, false, false);
    assert!(poll_scope_needs_reset(
        "abc1234567",
        "rebased-head",
        "queued",
        lane,
        true,
        true
    ));
    reset_polled_review(&f.db, f.pr_id, true, lane, None).unwrap();
    let candidates = HashMap::from([(7, (f.pr_id, "rebased-head".into()))]);
    scan_activation_pages(
        &f.db,
        &f.repo,
        &candidates,
        crate::db::now_millis(),
        |_, _| Ok(vec![comment(2)]),
        |_, _| Ok(true),
    )
    .unwrap();
    assert_eq!(f.db.pr_review_start_state(f.pr_id).unwrap().1, "manual_required");
    assert!(f.db.queued_pr_ids().unwrap().is_empty());
    assert_eq!(f.activation_marker_body().1, before.1);
    let conn = crate::db::Db::for_test_at(f.root.join("test.sqlite")).open().unwrap();
    let values: (bool, bool, bool) = conn
        .query_row(
            "SELECT is_draft, force_full_next_review, reprocess_next_review FROM pull_requests WHERE id = $1",
            [f.pr_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(values, (true, false, false));
    f.db.set_pr_lane(f.pr_id, "closed", None).unwrap();
    assert!(f.db.queued_pr_ids().unwrap().is_empty());
    assert!(f.db.pr_comment_activated(1, 7).unwrap());
}

#[test]
fn activation_reenable_resets_baseline_and_ui_queue_keeps_existing_semantics() {
    let f = Fixture::new("queued");
    let before = f.db.comment_scan_cursor(1).unwrap();
    f.db.finish_comment_scan(1, crate::db::now_millis() + 120_000).unwrap();
    assert_ne!(f.db.comment_scan_cursor(1).unwrap().1, before.1);
    f.db.save_repo_subscription(1, false, "manual", true, false).unwrap();
    assert_eq!(f.db.queued_pr_ids().unwrap(), vec![f.pr_id]);
    assert!(!f.db.comment_queue_requires_eligibility(f.pr_id).unwrap());
    assert!(f.db.consume_activation_command(1, before.0, 1, None).is_err());
    assert!(!f.db.comment_activation_seen(1, 1).unwrap());
    f.db.save_repo_subscription(1, true, "manual", true, false).unwrap();
    assert!(f.db.comment_scan_cursor(1).unwrap().0 >= before.0);
    let conn = crate::db::Db::for_test_at(f.root.join("test.sqlite")).open().unwrap();
    assert!(conn
        .query_row(
            "SELECT enabled_at = checkpoint FROM comment_scan_cursors WHERE repo_id = 1",
            [],
            |row| row.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn activation_poll_reset_keeps_normal_manual_and_automatic_scope_semantics() {
    for automatic in [false, true] {
        for base_changed in [false, true] {
            let f = Fixture::new("draft");
            f.scan(vec![comment(1)], true).unwrap();
            let lane = author_activation_lane(true, true, false, false, automatic);
            let head = if base_changed { "abc1234567" } else { "new-head" };
            assert!(poll_scope_needs_reset(
                "abc1234567",
                head,
                "queued",
                lane,
                base_changed,
                true
            ));
            reset_polled_review(&f.db, f.pr_id, base_changed, lane, None).unwrap();
            assert_eq!(f.db.pr_review_start_state(f.pr_id).unwrap().1, lane);
            assert!(f.db.pr_comment_activated(1, 7).unwrap());
            assert!(f.activation_marker_body().1.contains("abc1234567"));
        }
    }
    let f = Fixture::new("draft");
    f.scan(vec![comment(1)], true).unwrap();
    assert!(!poll_scope_needs_reset(
        "abc1234567",
        "abc1234567",
        "queued",
        "manual_required",
        false,
        true
    ));
    assert!(poll_scope_needs_reset(
        "abc1234567",
        "abc1234567",
        "queued",
        "auto_skipped",
        false,
        true
    ));
    reset_polled_review(&f.db, f.pr_id, false, "auto_skipped", None).unwrap();
    assert!(f.db.queued_pr_ids().unwrap().is_empty());
    assert!(!poll_scope_needs_reset(
        "abc1234567",
        "abc1234567",
        "auto_skipped",
        "open",
        false,
        true
    ));
}

fn fixture_state(f: &Fixture) -> SharedState {
    Arc::new(ServerState::new(ServerConfig {
        bind: "127.0.0.1:0".into(),
        db: crate::db::Db::for_test_at(f.root.join("test.sqlite")),
        data_dir: f.root.to_path_buf(),
        repos_dir: f.root.join("repos"),
        auto_reindex: false,
        poll_interval_seconds: 60,
        api_token: None,
    }))
}

fn snapshot(title: &str, head: &str, files: Option<i64>) -> gitlab::GhPullRequest {
    serde_json::from_value(serde_json::json!({"number":7,"title":title,"author":{"login":"author"},"isDraft":true,"headRefOid":head,"baseRefOid":"base","baseRefName":"main","changedFiles":files})).unwrap()
}

#[test]
fn activation_missing_listing_uses_real_poll_policy_and_keeps_monitoring_new_heads() {
    for draft in [true, false] {
        activation_without_reviewers(draft);
    }
}

fn activation_without_reviewers(draft: bool) {
    let snapshot = |title: &str, head: &str, files: Option<i64>| {
        let mut pr = snapshot(title, head, files);
        pr.is_draft = draft;
        pr
    };
    let f = Fixture::new("draft");
    let state = fixture_state(&f);
    let mut settings = f.db.settings().unwrap();
    settings.review_assigned_only = true;
    settings.review_auto_enabled = false;
    let targets = std::collections::HashSet::from(["author".to_string()]);
    let before = poll_one_pr(
        &state,
        &f.repo,
        &settings,
        &targets,
        &targets,
        &snapshot("Feature", "abc1234567", Some(1)),
        false,
    )
    .unwrap();
    assert!(!before.queue_eligible);
    assert_eq!(
        f.db.pr_state(1, 7).unwrap().unwrap().1,
        if draft { "draft" } else { "auto_skipped" }
    );
    let mut resolved = false;
    scan_activation_pages_with_lookup(
        &f.db,
        &f.repo,
        &HashMap::new(),
        crate::db::now_millis(),
        |_, _| Ok(vec![comment(1)]),
        |_, _| Ok(true),
        |number| {
            assert_eq!(number, 7);
            resolved = true;
            let pr = snapshot("Feature", "abc1234567", Some(1));
            let polled = poll_one_pr(&state, &f.repo, &settings, &targets, &targets, &pr, false)?;
            assert!(polled.activation_eligible);
            Ok(Some((polled.pr_id, polled.head)))
        },
    )
    .unwrap();
    assert!(resolved);
    assert_eq!(f.db.queued_pr_ids().unwrap(), vec![f.pr_id]);
    assert_eq!(f.db.activated_open_pr_numbers(1).unwrap(), vec![7]);
    let queued = poll_one_pr(
        &state,
        &f.repo,
        &settings,
        &targets,
        &targets,
        &snapshot("Feature", "abc1234567", Some(1)),
        false,
    )
    .unwrap();
    assert!(queued.queue_eligible);
    assert_eq!(f.db.queued_pr_ids().unwrap(), vec![f.pr_id]);
    poll_one_pr(
        &state,
        &f.repo,
        &settings,
        &targets,
        &targets,
        &snapshot("Feature", "next-head", Some(1)),
        false,
    )
    .unwrap();
    assert_eq!(f.db.pr_state(1, 7).unwrap().unwrap().1, "manual_required");
    settings.auto_skip_keywords = vec!["ignored".into()];
    let excluded = poll_one_pr(
        &state,
        &f.repo,
        &settings,
        &targets,
        &targets,
        &snapshot("Ignored feature", "next-head", Some(1)),
        false,
    )
    .unwrap();
    assert!(!excluded.activation_eligible && !excluded.queue_eligible);
    assert_eq!(f.db.pr_state(1, 7).unwrap().unwrap().1, "auto_skipped");
}

#[test]
fn queued_and_failed_prs_leave_the_board_once_they_match_an_ignored_keyword() {
    for lane in ["queued", "failed"] {
        let f = Fixture::new(lane);
        let state = fixture_state(&f);
        let mut settings = f.db.settings().unwrap();
        settings.auto_skip_keywords = vec!["chore(deps)".into()];
        let targets = std::collections::HashSet::new();
        let pr: gitlab::GhPullRequest = serde_json::from_value(serde_json::json!({"number":7,"title":"chore(deps): bump undici","author":{"login":"author"},"isDraft":false,"headRefOid":"abc1234567","baseRefOid":"base","baseRefName":"main","changedFiles":1})).unwrap();
        let polled = poll_one_pr(&state, &f.repo, &settings, &targets, &targets, &pr, false).unwrap();
        assert!(!polled.queue_eligible);
        assert_eq!(f.db.pr_state(1, 7).unwrap().unwrap().1, "auto_skipped");
        assert!(!f.db.queued_pr_ids().unwrap().contains(&polled.pr_id));
    }
}

#[test]
fn activation_draft_precedence_does_not_bypass_filters_or_unknown_size() {
    let f = Fixture::new("draft");
    let state = fixture_state(&f);
    let mut settings = f.db.settings().unwrap();
    settings.review_assigned_only = true;
    settings.auto_skip_keywords = vec!["ignored".into()];
    let targets = std::collections::HashSet::new();
    for pr in [
        snapshot("Ignored feature", "abc1234567", Some(1)),
        snapshot("Feature", "abc1234567", Some(settings.review_max_files + 1)),
        snapshot("Feature", "abc1234567", None),
    ] {
        let result = poll_one_pr(&state, &f.repo, &settings, &targets, &targets, &pr, false).unwrap();
        assert!(!result.activation_eligible && !result.queue_eligible);
        assert_eq!(f.db.pr_state(1, 7).unwrap().unwrap().1, "draft");
    }
}

#[test]
fn activation_failed_candidate_does_not_block_later_commands_or_advance_cursor() {
    let f = Fixture::new("draft");
    let before = f.db.comment_scan_cursor(1).unwrap();
    assert!(scan_activation_pages_with_lookup(
        &f.db,
        &f.repo,
        &HashMap::new(),
        crate::db::now_millis() + 120000,
        |_, _| Ok(vec![comment(1), comment(2)]),
        |_, c| if c.id == 1 {
            Err("edit history unavailable".into())
        } else {
            Ok(true)
        },
        |_| Ok(Some((f.pr_id, "abc1234567".into())))
    )
    .is_err());
    assert!(!f.db.comment_activation_seen(1, 1).unwrap());
    assert!(f.db.pr_comment_activated(1, 7).unwrap());
    assert_eq!(f.db.comment_scan_cursor(1).unwrap(), before);
    let g = Fixture::new("draft");
    assert!(scan_activation_pages_with_lookup(
        &g.db,
        &g.repo,
        &HashMap::new(),
        crate::db::now_millis() + 120000,
        |_, _| Ok(vec![comment(1)]),
        |_, _| Ok(true),
        |_| Err("PR fetch failed".into())
    )
    .is_err());
    assert!(!g.db.comment_activation_seen(1, 1).unwrap());
}

#[test]
fn activation_acknowledgement_is_due_only_while_the_pr_is_open() {
    let f = Fixture::new("draft");
    f.scan(vec![comment(1)], true).unwrap();
    f.db.acknowledge_review_cycle(f.pr_id, false, None).unwrap();
    assert_eq!(
        f.db.due_queue_acknowledgements(crate::db::now_millis()).unwrap().len(),
        1
    );
    crate::db::Db::for_test_at(f.root.join("test.sqlite"))
        .open()
        .unwrap()
        .execute("UPDATE pull_requests SET state = 'closed'", [])
        .unwrap();
    assert!(f
        .db
        .due_queue_acknowledgements(crate::db::now_millis())
        .unwrap()
        .is_empty());
    assert!(f.db.activated_open_pr_numbers(1).unwrap().is_empty());
}

#[test]
fn file_limit_counts_only_reviewable_files_and_remembers_them_per_revision() {
    let f = Fixture::new("limit");
    let state = fixture_state(&f);
    let mut files: Vec<String> = (0..3).map(|i| format!("src/f{i}.rs")).collect();
    files.extend((0..5).map(|i| format!("dist/bundle{i}.js")));
    files.extend(["go.sum".to_string(), "assets/logo.png".to_string()]);
    let pr = snapshot("Feature", "abc1234567", Some(files.len() as i64));
    let full_name = f.repo.full_name.clone();
    let calls = std::cell::Cell::new(0);
    let calls = &calls;
    let listed = || {
        let (files, full_name) = (files.clone(), full_name.clone());
        move |name: &str, number: i64| {
            assert_eq!((name, number), (full_name.as_str(), 7));
            calls.set(calls.get() + 1);
            Ok(files)
        }
    };
    let full = |count| {
        Some(FileCount {
            count,
            reviewable: false,
        })
    };
    let reviewable = |count| {
        Some(FileCount {
            count,
            reviewable: true,
        })
    };
    // Under the limit GitHub's count stands and nothing is listed.
    assert_eq!(file_count_for_limit(&state, &f.repo, &pr, 10, listed()), full(10));
    // Over it, only source files count; bundles, images and lockfiles do not.
    assert_eq!(file_count_for_limit(&state, &f.repo, &pr, 3, listed()), reviewable(3));
    assert_eq!(file_count_for_limit(&state, &f.repo, &pr, 3, listed()), reviewable(3));
    assert_eq!(calls.get(), 1, "the same head and base are counted once");
    let moved = snapshot("Feature", "def7654321", Some(files.len() as i64));
    assert_eq!(
        file_count_for_limit(&state, &f.repo, &moved, 3, listed()),
        reviewable(3)
    );
    assert_eq!(calls.get(), 2, "a new head is counted again");
    // A short listing (GitHub stops at 3,000 files) keeps the full count, and is not listed again.
    let short = fixture_state(&f);
    let short_calls = std::cell::Cell::new(0);
    for _ in 0..2 {
        let result = file_count_for_limit(&short, &f.repo, &pr, 3, |_, _| {
            short_calls.set(short_calls.get() + 1);
            Ok(vec!["src/a.rs".into()])
        });
        assert_eq!(result, full(10));
    }
    assert_eq!(short_calls.get(), 1);
    // A failed listing keeps the full count until it is retried.
    let failed = fixture_state(&f);
    assert_eq!(
        file_count_for_limit(&failed, &f.repo, &pr, 3, |_, _| Err("offline".into())),
        full(10)
    );
    assert_eq!(
        file_count_for_limit(&failed, &f.repo, &pr, 3, |_, _| unreachable!()),
        full(10)
    );
    failed
        .reviewable_file_counts
        .lock()
        .unwrap()
        .values_mut()
        .for_each(|(_, listing)| {
            *listing = FileListing::Failed(std::time::Instant::now() - FILE_LISTING_RETRY);
        });
    assert_eq!(file_count_for_limit(&failed, &f.repo, &pr, 3, listed()), reviewable(3));
    let unknown = snapshot("Feature", "abc1234567", None);
    assert_eq!(
        file_count_for_limit(&failed, &f.repo, &unknown, 3, |_, _| unreachable!()),
        None
    );
}
