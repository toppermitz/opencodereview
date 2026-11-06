use super::*;

#[cfg(unix)]
#[test]
fn claude_cli_version_reads_the_release_without_exposing_probe_errors() {
    use std::os::unix::process::ExitStatusExt;
    let output = |success, text: &[u8]| {
        Ok(Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 256 }),
            stdout: text.to_vec(),
            stderr: b"probe diagnostic".to_vec(),
        })
    };
    assert_eq!(
        claude_cli_version(output(true, b"2.1.292 (Claude Code)\n")),
        Some("2.1.292".into())
    );
    for raw in [b"".as_slice(), b"unexpected response", &[255]] {
        assert_eq!(claude_cli_version(output(true, raw)), None);
    }
    assert_eq!(claude_cli_version(output(false, b"2.1.292 (Claude Code)\n")), None);
    assert_eq!(claude_cli_version(Err("CLI unavailable or timed out".into())), None);
}

#[test]
fn transport_contract_preserves_public_routes_and_json_errors() {
    assert!(is_public_route("GET", "/health"));
    assert!(is_public_route("POST", "/auth/gitlab/start"));
    assert!(!is_public_route("GET", "/analytics"));

    let mut request = Request {
        method: "POST".to_string(),
        path: "/settings".to_string(),
        query: String::new(),
        origin: None,
        authorization: None,
        content_type: Some("application/json; charset=utf-8".to_string()),
        flow_secret: None,
        body: Vec::new(),
    };
    assert!(has_json_content_type(&request));
    request.query = "days=90&repo=12&tz=-180&name=a%2Fb+c&bad=%zz".into();
    assert_eq!(query_param(&request, "days").as_deref(), Some("90"));
    assert_eq!(query_param(&request, "tz").as_deref(), Some("-180"));
    assert_eq!(query_param(&request, "name").as_deref(), Some("a/b c"));
    assert_eq!(query_param(&request, "bad").as_deref(), Some("%zz"));
    assert_eq!(query_param(&request, "missing"), None);
    request.query.clear();
    request.content_type = Some("text/plain".to_string());
    assert!(!has_json_content_type(&request));

    let response = json_error(415, "Content-Type must be application/json");
    assert_eq!(response.status, 415);
    let body: serde_json::Value = serde_json::from_slice(&response.body).expect("JSON error");
    assert_eq!(body["error"], "Content-Type must be application/json");
}

#[test]
fn review_preflight_refreshes_and_reuses_a_shared_index() {
    let root = std::env::temp_dir().join(format!(
        "ocr-preflight-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let repo_path = root.join("repos/acme/repo");
    std::fs::create_dir_all(&repo_path).unwrap();
    let git = |args: &[&str]| indexer::git(&repo_path, args, None).unwrap();
    git(&["init", "--initial-branch=main"]);
    std::fs::write(repo_path.join("index.ts"), "export const value = 1;").unwrap();
    git(&["add", "index.ts"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "commit",
        "-m",
        "initial",
    ]);
    git(&["remote", "add", "origin", &repo_path.to_string_lossy()]);
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/repo").unwrap();
    let repo = db.save_repo_default_branch(1, "main").unwrap();
    let state = Arc::new(ServerState::new(ServerConfig {
        bind: "127.0.0.1:0".to_string(),
        db: crate::db::Db::for_test_at(root.join("test.sqlite")),
        data_dir: root.to_path_buf(),
        repos_dir: root.join("repos"),
        auto_reindex: false,
        poll_interval_seconds: 300,
        api_token: None,
    }));
    let new_run = |number| {
        let pr = db
            .upsert_pull_request(&crate::db::PullRequestInput {
                repo_id: 1,
                number,
                title: "Fixture".to_string(),
                author: "fixture".to_string(),
                author_avatar_url: None,
                is_draft: false,
                head_sha: "head".to_string(),
                base_sha: None,
                base_ref: Some("main".to_string()),
                head_ref: Some("feature".to_string()),
                default_branch: Some("main".to_string()),
                url: None,
                lane: "manual_required".to_string(),
                status_reason: None,
            })
            .unwrap();
        let run = db.start_review_skeleton_with(pr, Some("fixture")).unwrap();
        db.set_review_run_running(run, &review::index_phase_json("running"))
            .unwrap();
        run
    };
    let first = new_run(1);
    let second = new_run(2);
    let worker_state = state.clone();
    let worker_repo = repo.clone();
    let worker =
        thread::spawn(move || prepare_review_index(&worker_state, &worker_repo, first, &AtomicBool::new(false)));
    prepare_review_index(&state, &repo, second, &AtomicBool::new(false)).unwrap();
    worker.join().unwrap().unwrap();
    let ready = db.repo_index_status(1).unwrap();
    assert_eq!(ready.status, "ready");
    assert!(state.index_jobs.lock().unwrap().is_empty());
    prepare_review_index(&state, &repo, new_run(3), &AtomicBool::new(false)).unwrap();
    assert_eq!(ready.indexed_at, db.repo_index_status(1).unwrap().indexed_at);
    std::fs::write(repo_path.join("index.ts"), "export const value = 2;").unwrap();
    git(&["add", "index.ts"]);
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "commit",
        "-m",
        "changed",
    ]);
    prepare_review_index(&state, &repo, new_run(4), &AtomicBool::new(false)).unwrap();
    assert_ne!(ready.base_sha, db.repo_index_status(1).unwrap().base_sha);
    db.fail_repo_index(1, "Previous failure").unwrap();
    prepare_review_index(&state, &repo, new_run(5), &AtomicBool::new(false)).unwrap();
    assert_eq!(db.repo_index_status(1).unwrap().status, "ready");
    let shared_cancel = Arc::new(AtomicBool::new(false));
    state.index_jobs.lock().unwrap().insert(1, shared_cancel.clone());
    assert_eq!(
        prepare_review_index(&state, &repo, new_run(6), &AtomicBool::new(true)).unwrap_err(),
        "cancelled"
    );
    assert!(!shared_cancel.load(Ordering::Relaxed));
    state.index_jobs.lock().unwrap().clear();
    let mut old_spec = ready.clone();
    old_spec.spec_version -= 1;
    assert!(!index_matches_remote(
        &old_spec,
        "main",
        ready.base_sha.as_deref().unwrap()
    ));
    assert!(!index_matches_remote(
        &ready,
        "other",
        ready.base_sha.as_deref().unwrap()
    ));
    db.save_repo_default_branch(1, "missing-branch").unwrap();
    assert!(prepare_review_index(&state, &repo, new_run(7), &AtomicBool::new(false))
        .unwrap_err()
        .contains("Remote default branch"));
    db.save_repo_default_branch(1, "main").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    db.finish_review_run(second, "Fixture result", None, &review_done_phase())
        .unwrap();
    let second_pr = db.review_run_by_id(second).unwrap().unwrap().pr_id;
    for id in 100..100 + db.settings().unwrap().review_concurrency {
        state.review_jobs.lock().unwrap().insert(
            id,
            ReviewJob {
                force_full: false,
                reprocess: false,
                cancel_by_run: HashMap::new(),
                superseded: Arc::new(AtomicBool::new(false)),
            },
        );
    }
    start_review_job(&state, second_pr, true).unwrap();
    assert!(db.take_force_full_next_review(second_pr).unwrap());
    db.handle()
        .open()
        .unwrap()
        .execute(
            "UPDATE pull_requests SET last_decision_sha=head_sha WHERE id=$1",
            [second_pr],
        )
        .unwrap();
    start_review_job_with_intent(&state, second_pr, false, true).unwrap();
    start_review_job(&state, second_pr, false).unwrap();
    assert!(db.take_reprocess_next_review(second_pr).unwrap());
    assert!(!db.take_reprocess_next_review(second_pr).unwrap());
    assert!(db
        .list_review_board()
        .unwrap()
        .iter()
        .any(|group| group.lane == "queued" && group.items.iter().any(|card| card.id == second_pr)));
    state.review_jobs.lock().unwrap().clear();
    drop(db);
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clean_reviews_are_completed_without_finding_selection_even_with_auto_post_enabled() {
    let root = std::env::temp_dir().join(format!(
        "ocr-finding-gate-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_setting("auto_post_mode", "comment").unwrap();
    db.save_setting("pr_summary_comment_enabled", "false").unwrap();
    db.save_setting("review_result_mode", "silent").unwrap();
    let pr_id = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "Fixture".to_string(),
            author: "fixture".to_string(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "abc".to_string(),
            base_sha: None,
            base_ref: Some("main".to_string()),
            head_ref: None,
            default_branch: Some("main".to_string()),
            url: None,
            lane: "manual_required".to_string(),
            status_reason: None,
        })
        .unwrap();
    let run_id = db.start_review_skeleton_with(pr_id, None).unwrap();
    db.set_review_run_running(run_id, "[]").unwrap();
    db.finish_review_run(run_id, "Nada a sinalizar", None, "[]").unwrap();
    let inputs = review::ReviewInputs {
        frozen: None,
        run_id,
        repo_id: 1,
        pr_id,
        repo_path: root.clone(),
        full_name: "acme/fixture".to_string(),
        number: 1,
        base_ref: "main".to_string(),
        fixed_head_sha: None,
        offline: false,
        expected_scope: None,
        prior_scope: None,
        last_reviewed_sha: None,
        force_full: false,
        reprocess: false,
        skill_content: String::new(),
        unresolved_review_head: None,
        unresolved_review_body: None,
        pr_title: "Fixture".to_string(),
        pr_body: String::new(),
        model: "fixture".to_string(),
        review_depth_mode: "economic".to_string(),
        review_deep_threshold: 5,
        review_chunk_size: 5,
        review_parallel_chunks: 2,
        db_handle: Some(db.handle()),
        archive_dir: std::env::temp_dir().join(format!("ocr-review-archives-{}", crate::sandbox::secret().unwrap())),
        budget: None,
    };
    for no_changes in [false, true] {
        handle_completed_review(&db, &inputs, no_changes, false);
        assert_eq!(db.review_run_by_id(run_id).unwrap().unwrap().status, "approved");
        let result = db.review_result(pr_id).unwrap().unwrap();
        assert_eq!(result.effective_state, "clean");
        assert_ne!(result.publication_status, "pending");
        let expected_lane = if no_changes { "approved" } else { "reviewed_cleanly" };
        assert_eq!(db.pr_review_start_state(pr_id).unwrap().1, expected_lane);
        assert!(!db.has_pending_action("review_ready", pr_id).unwrap());
    }
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

fn review_run(body: Option<&str>, comments_json: Option<&str>) -> crate::db::ReviewRun {
    crate::db::ReviewRun {
        id: 1,
        pr_id: 1,
        status: "preview".to_string(),
        head_sha: "abc".to_string(),
        review_body: body.map(str::to_string),
        comments_json: comments_json.map(str::to_string),
        risk_score: None,
        risk_level: None,
        risk_json: None,
        error_message: None,
        phase_json: None,
        tail: None,
        started_at: None,
        finished_at: None,
        created_at: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        usage_model: None,
        model: None,
        context_index_sha: None,
        reviewed_head_sha: None,
    }
}

#[test]
fn loopback_origins_require_the_configured_scheme_and_port() {
    let trusted = "http://localhost:1421";
    for origin in [
        trusted,
        "http://127.0.0.1:1421",
        "http://[::1]:1421",
        "http://localhost:1421/",
    ] {
        assert!(origins_match(origin, trusted), "{origin}");
    }
    for origin in [
        "http://127.0.0.1:55073",
        "https://localhost:1421",
        "http://localhost",
        "https://example.com",
        "http://localhost.evil:1421",
        "http://user@localhost:1421",
        "http://localhost:1421/path",
        "http://[::1]:1421?query",
        "null",
    ] {
        assert!(!origins_match(origin, trusted), "{origin}");
    }
    assert!(origins_match(
        "https://app.example.test",
        "https://app.example.test:443"
    ));
    assert!(!origins_match("https://other.example.test", "https://app.example.test"));
}

#[test]
fn review_progress_path_requires_the_exact_sse_route() {
    assert_eq!(parse_review_progress_path("/reviews/42/run/events"), Some(42));
    assert_eq!(parse_review_progress_path("/reviews/42/run/events?x=1"), None);
    assert_eq!(parse_review_progress_path("/reviews/42/run"), None);
    assert_eq!(parse_review_progress_path("/reviews/nope/run/events"), None);
}

#[test]
fn review_progress_terminal_statuses_stop_streaming() {
    assert!(!review_status_is_terminal("pending"));
    assert!(!review_status_is_terminal("running"));
    assert!(review_status_is_terminal("preview"));
    assert!(review_status_is_terminal("failed"));
    assert!(review_status_is_terminal("cancelled"));
}

#[test]
fn manually_cancelled_auto_review_stays_paused_until_head_changes() {
    assert!(manual_review_pause_applies(
        "head",
        "manual_required",
        Some("Review cancelled"),
        "head",
        "open",
    ));
    assert!(manual_review_pause_applies(
        "head",
        "manual_required",
        Some("Review cancelled"),
        "head",
        "manual_required",
    ));
    assert!(!manual_review_pause_applies(
        "head",
        "manual_required",
        Some("Review cancelled"),
        "new-head",
        "open",
    ));
    assert!(!manual_review_pause_applies(
        "head",
        "manual_required",
        Some("Manual AI review configured"),
        "head",
        "open",
    ));
    assert!(!manual_review_pause_applies(
        "head",
        "manual_required",
        Some("Review cancelled"),
        "head",
        "auto_skipped",
    ));
}

#[test]
fn changed_review_target_is_not_a_review_failure() {
    let current = gitlab::LivePrState {
        state: "OPEN".to_string(),
        head_sha: "head".to_string(),
        base_ref: "main".to_string(),
        base_sha: "base".to_string(),
        merged_at: None,
    };
    let changed = gitlab::LivePrState {
        head_sha: "new-head".to_string(),
        ..current.clone()
    };
    let merged = gitlab::LivePrState {
        state: "MERGED".to_string(),
        ..current.clone()
    };
    assert_eq!(classify_review_target(&current, "head"), ReviewTargetChange::Current);
    assert_eq!(
        classify_review_target(&changed, "head"),
        ReviewTargetChange::HeadChanged
    );
    assert_eq!(classify_review_target(&merged, "head"), ReviewTargetChange::Closed);
}

#[test]
fn translated_target_change_errors_still_supersede_the_run() {
    for error in [
        "PR changed during preparation",
        "PR state/head changed before preparation",
        "PR head changed before preparation. Refresh the PR and try again.",
        "PR base/head scope changed while fetching the snapshot. Run a full review.",
        "Head do MR mudou antes da preparação. Atualize o MR e tente de novo.",
        "Escopo base/head do MR mudou ao buscar o snapshot. Atualize o MR e rode uma revisão completa.",
        "Escopo base/head do MR mudou durante a revisão. Atualize o MR e rode uma revisão completa.",
    ] {
        assert!(is_review_target_changed_error(error), "{error}");
    }
    assert!(!is_review_target_changed_error("Index preparation failed: git failed"));
    assert!(!is_review_target_changed_error(
        "A branch base imediata do MR está ausente."
    ));

    let root = std::env::temp_dir().join(format!(
        "ocr-translated-target-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    let pr_id = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 7,
            title: "Fixture".to_string(),
            author: "fixture".to_string(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".to_string(),
            base_sha: Some("base".to_string()),
            base_ref: Some("main".to_string()),
            head_ref: Some("feature".to_string()),
            default_branch: Some("main".to_string()),
            url: None,
            lane: "manual_required".to_string(),
            status_reason: None,
        })
        .unwrap();
    for (error, force_full, reprocess) in [
        (
            "Head do MR mudou antes da preparação. Atualize o MR e tente de novo.",
            true,
            false,
        ),
        (
            "Escopo base/head do MR mudou ao buscar o snapshot. Atualize o MR e rode uma revisão completa.",
            false,
            true,
        ),
    ] {
        let run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
        db.set_review_run_running(run_id, &review::index_phase_json("running"))
            .unwrap();
        settle_preparation_error(&db, run_id, pr_id, 1, true, force_full, reprocess, error, false, false).unwrap();
        let run = db.review_run_by_id(run_id).unwrap().unwrap();
        assert_eq!(run.status, "superseded", "{error}");
        assert!(run.error_message.is_none(), "{error}");
        assert_eq!(db.take_force_full_next_review(pr_id).unwrap(), force_full, "{error}");
        assert_eq!(db.take_reprocess_next_review(pr_id).unwrap(), reprocess, "{error}");
    }
    let run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    db.set_review_run_running(run_id, &review::index_phase_json("running"))
        .unwrap();
    settle_preparation_error(&db, run_id, pr_id, 1, true, true, true, "git failed", false, false).unwrap();
    let run = db.review_run_by_id(run_id).unwrap().unwrap();
    assert_eq!(run.status, "failed");
    assert_eq!(db.take_force_full_next_review(pr_id).unwrap(), false);
    assert_eq!(db.take_reprocess_next_review(pr_id).unwrap(), false);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_review_target_supersedes_run_without_failure_notification() {
    let root = std::env::temp_dir().join(format!(
        "ocr-stale-review-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    let pr_id = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "Fixture".to_string(),
            author: "fixture".to_string(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".to_string(),
            base_sha: Some("base".to_string()),
            base_ref: Some("main".to_string()),
            head_ref: Some("feature".to_string()),
            default_branch: Some("main".to_string()),
            url: None,
            lane: "manual_required".to_string(),
            status_reason: None,
        })
        .unwrap();
    let run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    db.set_review_run_running(run_id, &review::index_phase_json("running"))
        .unwrap();

    for (force_full, reprocess) in [(false, true), (true, false), (true, true)] {
        let cancel = Arc::new(AtomicBool::new(false));
        let job = ReviewJob {
            force_full,
            reprocess,
            cancel_by_run: HashMap::from([(run_id, cancel.clone())]),
            superseded: Arc::new(AtomicBool::new(false)),
        };
        supersede_active_review(&db, pr_id, &job).unwrap();
        assert!(job.superseded.load(Ordering::Relaxed));
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(db.take_force_full_next_review(pr_id).unwrap(), force_full);
        assert_eq!(db.take_reprocess_next_review(pr_id).unwrap(), reprocess);
        db.restore_review_intent(pr_id, force_full, reprocess).unwrap();
        record_review_target_change(&db, run_id, pr_id, 1, true, ReviewTargetChange::HeadChanged).unwrap();
        assert_eq!(db.take_force_full_next_review(pr_id).unwrap(), force_full);
        assert_eq!(db.take_reprocess_next_review(pr_id).unwrap(), reprocess);
    }
    record_review_target_change(&db, run_id, pr_id, 1, true, ReviewTargetChange::Closed).unwrap();

    let run = db.review_run_by_id(run_id).unwrap().unwrap();
    assert_eq!(run.status, "superseded");
    assert!(run.error_message.is_none());
    assert_eq!(db.pr_state(1, 1).unwrap().unwrap().1, "closed");
    assert!(db.list_pending_actions().unwrap().is_empty());
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_phase_json_preserves_deep_topology() {
    let raw = "[{\"id\":\"context\",\"status\":\"ok\"},{\"id\":\"chunks\",\"status\":\"running\"},{\"id\":\"final\",\"status\":\"pending\"}]";
    let completed = complete_phase_json(raw, "fallback");
    let phases: Vec<serde_json::Value> = serde_json::from_str(&completed).expect("phases");
    assert_eq!(phases.len(), 3);
    assert!(phases.iter().all(|phase| phase["status"] == "ok"));
}

#[test]
fn review_body_finding_counts_as_finding() {
    assert!(review_has_findings(None, Some("[Achado] Body-only issue\n\nDetails")));
    assert!(review_has_findings(
        Some("{\"comments\":[]}"),
        Some("**[Achado] Markdown body-only issue**\n\nDetails")
    ));
    assert!(review_has_findings(
        Some("{\"comments\":[]}"),
        Some("> **[Pergunta] Markdown body-only question**\n\nDetails")
    ));
    assert!(!review_has_findings(
        Some("{\"comments\":[]}"),
        Some("No issues found.")
    ));
}

#[test]
fn mark_reviewed_findings_stay_local() {
    assert_eq!(
        effective_review_result_for_decision("skip", false),
        ("clean", "skipped", false)
    );
    assert_eq!(
        effective_review_result_for_decision("mark_reviewed", true),
        ("findings", "findings_local", false)
    );
    assert_eq!(
        effective_review_result_for_decision("comment", true),
        ("findings", "findings_posted", true)
    );
}

#[test]
fn fresh_findings_replace_the_prior_actionable_run() {
    let findings_a = actionable_run_id_for_result("findings", "awaiting_action", 1, None);
    let findings_b = actionable_run_id_for_result("findings", "awaiting_action", 2, findings_a);
    let no_change = actionable_run_id_for_result("no_new_changes", "awaiting_action", 3, findings_b);

    assert_eq!(findings_b, Some(2));
    assert_eq!(no_change, Some(2));
}

#[test]
fn legacy_review_outcomes_preserve_effective_state() {
    let clean = crate::db::LegacyReviewOutcome {
        run_id: 1,
        status: "approved".to_string(),
        decision: Some("mark_reviewed".to_string()),
        head_sha: "head-one".to_string(),
        reviewed_head_sha: None,
        comments_json: Some("{\"comments\":[]}".to_string()),
        review_body: Some("No issues found.".to_string()),
    };
    let findings = crate::db::LegacyReviewOutcome {
        run_id: 2,
        status: "superseded".to_string(),
        decision: None,
        head_sha: "head-two".to_string(),
        reviewed_head_sha: Some("reviewed-two".to_string()),
        comments_json: Some("{\"comments\":[]}".to_string()),
        review_body: Some("[Achado] Still needs action".to_string()),
    };
    let superseded_clean = crate::db::LegacyReviewOutcome {
        run_id: 3,
        status: "superseded".to_string(),
        decision: None,
        head_sha: "head-three".to_string(),
        reviewed_head_sha: Some("reviewed-three".to_string()),
        comments_json: Some("{\"comments\":[]}".to_string()),
        review_body: Some("No issues found.".to_string()),
    };

    let clean = legacy_effective_result(&clean).expect("clean legacy result");
    assert_eq!(clean.effective_state, "clean");
    assert_eq!(clean.reviewed_head_sha, "head-one");
    let findings = legacy_effective_result(&findings).expect("findings legacy result");
    assert_eq!(findings.effective_state, "awaiting_action");
    assert_eq!(findings.actionable_run_id, Some(2));
    let superseded_clean = legacy_effective_result(&superseded_clean).expect("superseded clean legacy result");
    assert_eq!(superseded_clean.effective_state, "awaiting_action");
    assert_eq!(superseded_clean.reviewed_head_sha, "reviewed-three");
    assert_eq!(superseded_clean.actionable_run_id, Some(3));
}

#[test]
fn unstructured_body_is_not_posted_as_comment() {
    let run = review_run(
        Some("Found one correctness issue: automatic recycler creation can fail."),
        Some("{\"comments\":[]}"),
    );
    let draft = compose_review_post("owner/repo", 1, &run, "COMMENT", false);
    assert!(draft.body.is_empty());
    assert_eq!(draft.comments.as_array().map(Vec::len), Some(0));
}

#[test]
fn body_only_finding_is_posted_as_generated_review_body() {
    let run = review_run(Some("[Achado] Body-only issue\n\nDetails"), Some("{\"comments\":[]}"));
    let draft = compose_review_post("owner/repo", 1, &run, "COMMENT", false);
    assert!(draft.body.contains("[Achado] Body-only issue"));
    assert!(!draft.body.contains("<sub>🤖"));
    assert_eq!(draft.comments.as_array().map(Vec::len), Some(0));
}

#[test]
fn request_changes_uses_body_only_finding_body() {
    let run = review_run(Some("[Achado] Body-only issue\n\nDetails"), Some("{\"comments\":[]}"));
    let draft = compose_review_post("owner/repo", 1, &run, "REQUEST_CHANGES", false);
    assert!(draft.body.contains("[Achado] Body-only issue"));
}

#[test]
fn result_comment_links_to_ocr_repository() {
    let clean = review_result_body(
        "owner/repo",
        7,
        "installation",
        42,
        "abcdef123456",
        "abcdef123456",
        "clean",
        "clean",
        None,
        "0.1.0",
        "v0",
        &[],
    );
    assert!(clean.contains(
        "### ✅ Não encontrei problemas\n\nRevisei [`abcdef12`](https://gitlab.example.com/owner/repo/-/commit/abcdef123456). Nada aqui precisa de ação."
    ));
    assert!(clean.ends_with("<sub>Revisado com [OpenCodeReview](https://github.com/BridgLabs/opencodereview).</sub>"));
    assert!(clean.contains("ocr-result-key:v1 repo=owner/repo pr=7"));
    assert!(clean.contains("<!-- ocr-status-publication:v1 run=42 -->"));
    assert!(clean.contains("head=abcdef123456 reviewed_head=abcdef123456"));
}

#[test]
fn result_comments_are_reserved_only_for_clean_reviews() {
    assert!(should_publish_review_result_comment(true, "clean"));
    assert!(!should_publish_review_result_comment(true, "findings_posted"));
    assert!(!should_publish_review_result_comment(true, "awaiting_action"));
    assert!(!should_publish_review_result_comment(false, "clean"));
}

#[test]
fn findings_review_payload_uses_result_summary_and_keeps_recovery_marker() {
    let payload = build_review_payload(
        "owner/repo",
        "abcdef",
        "COMMENT",
        "",
        Some("{\"comments\":[]}"),
        Some("<!-- ocr-review-post:v1 run=7 -->"),
        &[],
    )
    .expect("payload");
    let value: serde_json::Value = serde_json::from_str(&payload).expect("json");
    let body = value.get("body").and_then(|body| body.as_str()).unwrap_or_default();
    assert!(body.starts_with(
        "### ⚠️ Publiquei achados\n\nRevisei [`abcdef`](https://gitlab.example.com/owner/repo/-/commit/abcdef)."
    ));
    assert!(body.contains("Revisado com [OpenCodeReview](https://github.com/BridgLabs/opencodereview)"));
    assert!(body.ends_with("\n\n<!-- ocr-review-post:v1 run=7 -->"));
}

#[test]
fn findings_review_payload_lists_each_inline_comment_in_a_table() {
    let point = |kind: &str, title: &str| format!("**[{kind}] {title}**\n\nDetails.");
    let comments = serde_json::json!({ "comments": [
        { "path": "db/migrate/0042_billing_schema.sql", "line": 18, "body": format!("{}\n\n<!-- ocr:severity=high -->", point("Finding", "Rollback drops invoices written after deploy")) },
        { "path": "src/billing/invoices.ts", "line": 57, "body": point("Question", "Should the old schema stay readable during deploy?") },
        { "path": "src/billing/total.ts", "body": point("Finding", "Totals use `a | b` instead of `a || b`") },
        { "body": format!("{}\n\n<!-- ocr:severity=low -->", point("Finding", "Missing rollout note")) },
    ]});
    let payload = build_review_payload(
        "owner/repo",
        "5c540aa1234567",
        "REQUEST_CHANGES",
        "",
        Some(&comments.to_string()),
        Some("<!-- ocr-review-post:v1 run=7 -->"),
        &[],
    )
    .unwrap();
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert_eq!(value["comments"].as_array().map(Vec::len), Some(4));
    assert_eq!(value["body"].as_str().unwrap(), "### ⚠️ Encontrei 3 problemas e 1 pergunta\n\n\
Revisei [`5c540aa1`](https://gitlab.example.com/owner/repo/-/commit/5c540aa1234567). Cada problema abaixo tem os detalhes e uma correção sugerida.\n\n\
| | Problema | Onde |\n|---|---|---|\n\
| 🔴 | Rollback drops invoices written after deploy | `db/migrate/0042_billing_schema.sql:18` |\n\
| 🟠 | Totals use `a \\| b` instead of `a \\|\\| b` | `src/billing/total.ts` |\n\
| 🟡 | Missing rollout note | Geral |\n\
| ❓ | Should the old schema stay readable during deploy? | `src/billing/invoices.ts:57` |\n\n\
<sub>Revisado com [OpenCodeReview](https://github.com/BridgLabs/opencodereview).</sub>\n\n\
<!-- ocr-review-post:v1 run=7 -->");
}

#[test]
fn findings_summary_headings_count_questions_and_truncate_long_titles() {
    let point = |kind: &str, title: &str| serde_json::json!({ "path": "a.rs", "line": 1, "body": format!("**[{kind}] {title}**\n\nDetails.") });
    let questions = posted_findings_review_body(
        "owner/repo",
        "abcdef1234",
        &[point("Question", "One?"), point("Question", "Two?")],
    );
    assert!(questions.starts_with("### ❓ Tenho 2 perguntas\n\n"));
    let single = posted_findings_review_body("owner/repo", "abcdef1234", &[point("Finding", &"x".repeat(300))]);
    assert!(single.starts_with("### ⚠️ Encontrei 1 problema\n\n"));
    assert!(single.contains(&format!("| 🟠 | {}… | `a.rs:1` |", "x".repeat(119))));
}

#[test]
fn evidence_warning_publication_preserves_findings_and_recovery_markers() {
    let warnings = vec![crate::review::warnings::EvidenceWarning {
        reason: "insufficient_evidence".into(),
        path: Some("docs/IDENTITIES.md".into()),
        reference: None,
    }];
    let comments = r#"{"comments":[{"path":"code.rs","line":1,"body":"retained finding"}]}"#;
    for event in ["COMMENT", "REQUEST_CHANGES", "APPROVE"] {
        let payload = build_review_payload(
            "owner/repo",
            "head",
            event,
            "",
            Some(comments),
            Some("<!-- publication-marker -->"),
            &warnings,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["event"], event);
        assert_eq!(value["comments"][0]["body"], "retained finding");
        let body = value["body"].as_str().unwrap();
        assert!(body.contains("[!WARNING]") && body.contains("docs/IDENTITIES.md"));
        assert!(body.contains("reference was not specified"));
        assert!(body.ends_with("<!-- publication-marker -->"));
        assert!(!body.contains("infrastructure"));
    }
    let limited = review_result_body(
        "owner/repo",
        1,
        "install",
        1,
        "head",
        "head",
        "clean",
        "clean",
        None,
        "reviewer",
        "skill",
        &warnings,
    );
    assert!(limited.contains("Não encontrei problemas") && limited.contains("[!WARNING]"));
    assert!(limited.contains("ocr-result-data:v1"));
    assert!(limited.find("[!WARNING]") < limited.find("<sub>Revisado com [OpenCodeReview]"));
    let empty = build_review_payload(
        "owner/repo",
        "head",
        "COMMENT",
        "",
        Some(r#"{"comments":[]}"#),
        None,
        &warnings,
    )
    .unwrap();
    let empty: serde_json::Value = serde_json::from_str(&empty).unwrap();
    let body = empty["body"].as_str().unwrap();
    assert!(body.starts_with("### ⚠️ Terminei com avisos\n\nRevisei [`head`](https://gitlab.example.com/owner/repo/-/commit/head).\n\n> [!WARNING]"));
    assert!(
        body.contains("[!WARNING]")
            && body.ends_with("Revisado com [OpenCodeReview](https://github.com/BridgLabs/opencodereview).</sub>")
    );
    assert!(!body.contains("posted findings"));
}

#[test]
fn skip_decision_leaves_attention_and_survives_restore() {
    let root = std::env::temp_dir().join(format!(
        "ocr-skip-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    let pr_id = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "Fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: None,
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "manual_required".into(),
            status_reason: None,
        })
        .unwrap();
    let run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    db.set_review_run_running(run_id, "[]").unwrap();
    db.store_finding_audit(
        run_id,
        &crate::review::findings::FindingAudit {
            evidence_warnings: None,
            head_sha: "head".into(),
            complete: true,
            original_body: "Nada a sinalizar.".into(),
            diff: None,
            candidates: vec![],
        },
    )
    .unwrap();
    db.finish_review_run(run_id, "Nada a sinalizar.", None, "[]").unwrap();
    let selection = db
        .save_finding_selection(pr_id, run_id, db.finding_audit(pr_id, run_id).unwrap().revision, &[])
        .unwrap();
    let state = Arc::new(ServerState::new(ServerConfig {
        bind: "127.0.0.1:0".into(),
        db: crate::db::Db::for_test_at(root.join("test.sqlite")),
        data_dir: root.to_path_buf(),
        repos_dir: root.join("repos"),
        auto_reindex: false,
        poll_interval_seconds: 300,
        api_token: None,
    }));
    let request = Request {
        method: "POST".into(),
        path: format!("/reviews/{pr_id}/decide"),
        query: String::new(),
        origin: None,
        authorization: None,
        content_type: Some("application/json".into()),
        flow_secret: None,
        body: serde_json::to_vec(
            &serde_json::json!({"decision":"skip","runId":run_id,"selectionRevision":selection.revision}),
        )
        .unwrap(),
    };
    decide_review_http(&request, &state, pr_id).unwrap();
    assert_eq!(
        db.pr_review_start_state(pr_id).unwrap(),
        ("head".into(), "skipped".into(), Some("head".into()))
    );
    let run = db.review_run_by_id(run_id).unwrap().unwrap();
    assert_eq!(run.status, "skipped");
    assert!(db.review_run_is_decided(run_id).unwrap());
    assert_eq!(db.review_result(pr_id).unwrap().unwrap().effective_state, "skipped");
    db.sync_review_attention_pendings().unwrap();
    assert!(!db.has_pending_action("manual_review_needed", pr_id).unwrap());
    assert!(!db.has_pending_action("review_ready", pr_id).unwrap());
    assert!(db
        .list_review_board()
        .unwrap()
        .iter()
        .any(|group| group.lane == "skipped" && group.items.iter().any(|card| card.id == pr_id)));
    db.set_pr_lane(pr_id, "queued", None).unwrap();
    db.restore_decided_pr_lane(pr_id).unwrap();
    assert_eq!(db.pr_review_start_state(pr_id).unwrap().1, "skipped");
    db.set_pr_lane(pr_id, "manual_required", None).unwrap();
    finalize_inherited_result_without_comment(&db, &db.review_result(pr_id).unwrap().unwrap()).unwrap();
    assert_eq!(db.pr_review_start_state(pr_id).unwrap().1, "skipped");
    db.sync_review_attention_pendings().unwrap();
    assert!(!db.has_pending_action("manual_review_needed", pr_id).unwrap());
    drop(state);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn relocated_selected_findings_survive_comment_publication() {
    let run = review_run(
        Some("1 review comments selected for publication."),
        Some(r#"{"comments":[{"body":"[Achado] issue","path":"a.rs","line":312}]}"#),
    );
    for suppressed in [0, 1] {
        let draft = finish_review_post(
            &run,
            "COMMENT",
            true,
            gitlab::DedupeResult {
                post_comments: serde_json::json!([]),
                annotated_comments_json: "{\"comments\":[]}".into(),
                suppressed_count: suppressed,
            },
            "[Achado] issue relocated from a.rs:312",
        );
        assert!(draft.body.contains("[Achado] issue"));
        assert!(validate_review_post_draft(&run, "COMMENT", &draft).unwrap());
    }
}

#[test]
fn suggestions_post_with_the_review_but_never_on_their_own_after_duplicates() {
    // As the review writes it: a collapsed block, not just its opening.
    let suggestions = crate::review::evidence::suggestions_section(&[serde_json::json!(
        {"path":"a.rs","line":3,"title":"Stale test name","detail":"Detail","action":"Rename it"}
    )]);
    let dedupe = |comments: serde_json::Value, suppressed| gitlab::DedupeResult {
        post_comments: comments,
        annotated_comments_json: "{\"comments\":[]}".into(),
        suppressed_count: suppressed,
    };
    let only = review_run(
        Some(&format!("Nada a sinalizar.\n\n{suggestions}")),
        Some(r#"{"comments":[]}"#),
    );
    assert!(review_has_findings(
        only.comments_json.as_deref(),
        only.review_body.as_deref()
    ));
    assert!(!crate::review::findings::actionable(
        only.comments_json.as_deref(),
        only.review_body.as_deref()
    ));
    for event in ["COMMENT", "APPROVE"] {
        let draft = finish_review_post(&only, event, false, dedupe(serde_json::json!([]), 0), "");
        // The collapsed block arrives whole, closed, as the review wrote it.
        assert!(draft.body.contains(suggestions.trim()), "{event}: {}", draft.body);
        assert!(draft.body.contains("</details>"), "{event}");
        assert!(!draft.body.contains("Nada a sinalizar."), "{event}");
    }
    let inline = review_run(
        Some(&format!("Produzi 1 comentário(s) inline.\n\n{suggestions}")),
        Some(r#"{"comments":[{"body":"[Achado] issue","path":"a.rs","line":3}]}"#),
    );
    let draft = finish_review_post(
        &inline,
        "COMMENT",
        false,
        dedupe(serde_json::json!([{"body":"[Achado] issue","path":"a.rs","line":3}]), 0),
        "",
    );
    assert!(draft.body.contains(suggestions.trim()), "{}", draft.body);
    let draft = finish_review_post(&inline, "COMMENT", false, dedupe(serde_json::json!([]), 1), "");
    assert!(draft.body.is_empty());
    // A review stored before the section was collapsed still posts its suggestions.
    let legacy = review_run(
        Some(
            "Nada a sinalizar.\n\n### 💡 Sugestões\n\n- `a.rs:3` **Stale test name**. Detail Alteração sugerida: Rename it",
        ),
        Some(r#"{"comments":[]}"#),
    );
    let draft = finish_review_post(&legacy, "COMMENT", false, dedupe(serde_json::json!([]), 0), "");
    assert!(draft.body.contains("### 💡 Sugestões"), "{}", draft.body);
    assert!(draft.body.contains("**Stale test name**"));
}

#[test]
fn a_review_keeps_its_suggestions_only_when_the_setting_is_on() {
    let db = crate::db::Db::for_test().open().unwrap();
    let suggestions = crate::review::evidence::suggestions_section(&[serde_json::json!(
        {"path":"a.rs","line":3,"title":"Stale test name","detail":"Detail","action":"Rename it"}
    )]);
    let deferred = crate::review::deferred_files_note(&["b.rs".to_string()]);
    assert!(!deferred.is_empty());
    let body = format!("Nada a sinalizar.\n\n{suggestions}{deferred}");
    // Off by default: the stored body has no suggestions, so a suggestions-only review is clean,
    // and what follows the section stays.
    let kept = crate::review::kept_review_body(&db, &body);
    assert_eq!(kept, format!("Nada a sinalizar.{deferred}"));
    assert!(review_suggestions(Some(&kept)).is_empty());
    assert!(!review_has_findings(Some(r#"{"comments":[]}"#), Some(&kept)));
    assert_eq!(
        crate::review::kept_review_body(&db, "Nada a sinalizar."),
        "Nada a sinalizar."
    );
    // On: the body is kept exactly as the review wrote it.
    db.save_setting("review_suggestions_enabled", "true").unwrap();
    let kept = crate::review::kept_review_body(&db, &body);
    assert_eq!(kept, body);
    assert!(review_has_findings(Some(r#"{"comments":[]}"#), Some(&kept)));
}

#[test]
fn empty_selected_publication_fails_unless_all_comments_are_proven_duplicates() {
    let run = review_run(
        Some("2 review comments selected for publication."),
        Some(r#"{"comments":[{"body":"[Achado] first"},{"body":"[Achado] second"}]}"#),
    );
    let mut draft = ReviewPostDraft {
        body: String::new(),
        comments: serde_json::json!([]),
        annotated_comments_json: "{}".into(),
        suppressed_count: 0,
    };
    assert!(validate_review_post_draft(&run, "COMMENT", &draft).is_err());
    draft.suppressed_count = 1;
    assert!(validate_review_post_draft(&run, "COMMENT", &draft).is_err());
    draft.suppressed_count = 2;
    assert!(!validate_review_post_draft(&run, "COMMENT", &draft).unwrap());
}

#[test]
fn review_publication_preparation_failure_unlocks_without_github_reconciliation() {
    review_publication_recovery_cases(true, false);
}

#[test]
fn review_publication_post_failure_preserves_acceptance_recovery() {
    review_publication_recovery_cases(false, false);
}

#[test]
fn review_publication_payload_write_failure_unlocks_without_github_reconciliation() {
    review_publication_recovery_cases(false, true);
}

fn review_publication_recovery_cases(snapshot_fails: bool, payload_fails: bool) {
    let preparation_fails = snapshot_fails || payload_fails;
    use std::cell::Cell;
    let root = std::env::temp_dir().join(format!(
        "ocr-publication-recovery-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    let pr_id = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "Fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: None,
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "ready".into(),
            status_reason: None,
        })
        .unwrap();
    let run_id = db.start_review_skeleton_with(pr_id, None).unwrap();
    db.set_review_run_running(run_id, "[]").unwrap();
    db.store_finding_audit(
        run_id,
        &crate::review::findings::FindingAudit {
            evidence_warnings: None,
            head_sha: "head".into(),
            complete: true,
            original_body: "Review body".into(),
            diff: None,
            candidates: vec![],
        },
    )
    .unwrap();
    db.finish_review_run(run_id, "Review body", None, "[]").unwrap();
    let selection = db
        .save_finding_selection(pr_id, run_id, db.finding_audit(pr_id, run_id).unwrap().revision, &[])
        .unwrap();
    let begin = || {
        db.try_begin_review_post(
            run_id,
            pr_id,
            "head",
            "COMMENT",
            "comment",
            "you",
            Some(selection.revision),
        )
        .unwrap()
    };
    assert!(begin());

    // Even if marker lookup would fail in the same outage, a failed preparation
    // must release the real database lock without invoking either GitHub step.
    let outcomes = if preparation_fails {
        vec![Err("GitHub unavailable".to_string())]
    } else {
        vec![Err("GitHub unavailable".to_string()), Ok(None), Ok(Some(42))]
    };
    for recovery in outcomes {
        let post_calls = Cell::new(0);
        let recovery_calls = Cell::new(0);
        let expected_id = recovery.clone().ok().flatten();
        let ambiguous = !preparation_fails && recovery.is_err();
        let result = publish_review_with_recovery(
            || {
                if snapshot_fails {
                    return Err("Snapshot unavailable".into());
                }
                // A regular file cannot be used as a directory, even under root.
                let directory = if payload_fails {
                    root.join("test.sqlite")
                } else {
                    root.clone()
                };
                gitlab::PreparedReview::in_directory("{}", &directory)
            },
            |_prepared| {
                post_calls.set(post_calls.get() + 1);
                Err("POST timed out".into())
            },
            || {
                recovery_calls.set(recovery_calls.get() + 1);
                recovery
            },
            || db.reset_review_posting(run_id),
        );
        assert_eq!(post_calls.get(), usize::from(!preparation_fails));
        assert_eq!(recovery_calls.get(), usize::from(!preparation_fails));
        if snapshot_fails {
            assert_eq!(result.unwrap_err(), "Snapshot unavailable");
        } else if payload_fails {
            assert!(result.is_err());
        } else if ambiguous {
            assert!(result.unwrap_err().contains("acceptance is ambiguous"));
        } else if let Some(id) = expected_id {
            assert_eq!(result.unwrap(), Some(id));
        } else {
            assert_eq!(result.unwrap_err(), "POST timed out");
        }
        let retains_lock = ambiguous || expected_id.is_some();
        assert_eq!(
            db.review_run_by_id(run_id).unwrap().unwrap().status,
            if retains_lock { "posting" } else { "preview" }
        );
        assert_eq!(begin(), !retains_lock);
        assert!(!std::fs::read_dir(&root).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("ocr-review-")));
        // Start each case with the same acquired publication lock.
    }
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retry_failed_clean_results_republishes_due_failures_only() {
    let root = std::env::temp_dir().join(format!(
        "ocr-retry-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.save_setting("review_result_mode", "silent").unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    let pr_id = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "Fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: None,
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "open".into(),
            status_reason: None,
        })
        .unwrap();
    let run_id = db.start_review_skeleton_with(pr_id, None).unwrap();
    let result = db
        .reserve_review_result(&crate::db::ReviewResultInput {
            pr_id,
            run_id,
            actionable_run_id: Some(run_id),
            observed_head_sha: "head",
            reviewed_head_sha: "head",
            prior_result_state: None,
            prior_result_head_sha: None,
            observation: "clean",
            effective_state: "clean",
            publication_status: "pending",
            body: Some("clean body"),
            github_review_id: None,
            github_review_url: None,
            reviewer_version: "0.1.0",
            skill_version: "v0",
        })
        .unwrap();
    db.fail_review_result_publication(result.id, "boom").unwrap();
    let now = crate::db::now_millis();
    let due_now = now + CLEAN_RESULT_RETRY_INTERVAL_MS + 1;

    // Silent mode never republishes.
    let mut silent_calls = 0usize;
    retry_failed_clean_results_with(&db, &root, due_now, |_, _, _, _, _| {
        silent_calls += 1;
        Ok(())
    });
    assert_eq!(silent_calls, 0);

    // A recent failure is not due until the interval elapses.
    db.save_setting("review_result_mode", "result_comment").unwrap();
    let mut recent_calls = 0usize;
    retry_failed_clean_results_with(&db, &root, now, |_, _, _, _, _| {
        recent_calls += 1;
        Ok(())
    });
    assert_eq!(recent_calls, 0);

    let mut published = Vec::new();
    retry_failed_clean_results_with(&db, &root, due_now, |_, result, repo, number, _| {
        published.push((result.id, result.publication_status.clone(), repo.to_string(), number));
        Ok(())
    });
    assert_eq!(published.len(), 1);
    assert_eq!(
        published[0],
        (result.id, "pending".to_string(), "acme/fixture".to_string(), 1)
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

/// Two due failed clean results on sibling PRs of one repo, publication mode
/// already set to result comments. Returns the temp root, the db, and the
/// two PR ids.
fn two_due_failed_clean_results(label: &str) -> (PathBuf, Database, Vec<i64>) {
    let root = std::env::temp_dir().join(format!(
        "ocr-retry-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_setting("review_result_mode", "result_comment").unwrap();
    let mut pr_ids = Vec::new();
    for number in 1..=2i64 {
        let pr_id = db
            .upsert_pull_request(&crate::db::PullRequestInput {
                repo_id: 1,
                number,
                title: "Fixture".into(),
                author: "fixture".into(),
                author_avatar_url: None,
                is_draft: false,
                head_sha: "head".into(),
                base_sha: None,
                base_ref: Some("main".into()),
                head_ref: Some("feature".into()),
                default_branch: Some("main".into()),
                url: None,
                lane: "open".into(),
                status_reason: None,
            })
            .unwrap();
        let run_id = db.start_review_skeleton_with(pr_id, None).unwrap();
        let result = db
            .reserve_review_result(&crate::db::ReviewResultInput {
                pr_id,
                run_id,
                actionable_run_id: Some(run_id),
                observed_head_sha: "head",
                reviewed_head_sha: "head",
                prior_result_state: None,
                prior_result_head_sha: None,
                observation: "clean",
                effective_state: "clean",
                publication_status: "pending",
                body: Some("clean body"),
                github_review_id: None,
                github_review_url: None,
                reviewer_version: "0.1.0",
                skill_version: "v0",
            })
            .unwrap();
        db.fail_review_result_publication(result.id, "boom").unwrap();
        pr_ids.push(pr_id);
    }
    (root, db, pr_ids)
}

/// Drives one retry pass over the fixture while `invalidate` runs inside the
/// first publish call against the OTHER PR. The pass must publish only once
/// and leave the invalidated result failed.
fn retry_failed_clean_results_mid_pass_invalidation(invalidate: impl Fn(&Database, i64)) {
    let (root, db, pr_ids) = two_due_failed_clean_results("invalidation");
    let due_now = crate::db::now_millis() + CLEAN_RESULT_RETRY_INTERVAL_MS + 1;
    let mut published = Vec::new();
    let mut invalidated = false;
    retry_failed_clean_results_with(&db, &root, due_now, |db, result, _, _, _| {
        published.push(result.pr_id);
        if !invalidated {
            invalidated = true;
            invalidate(
                db,
                if result.pr_id == pr_ids[0] {
                    pr_ids[1]
                } else {
                    pr_ids[0]
                },
            );
        }
        Ok(())
    });
    assert_eq!(published.len(), 1);
    let other = if published[0] == pr_ids[0] {
        pr_ids[1]
    } else {
        pr_ids[0]
    };
    assert_eq!(db.review_result(other).unwrap().unwrap().publication_status, "failed");
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retry_failed_clean_results_skips_result_invalidated_mid_pass() {
    // A re-review requested while the sibling is being republished.
    retry_failed_clean_results_mid_pass_invalidation(|db, other| {
        db.register_review_intent(other, false, true).unwrap();
    });
    // A newer run started while the sibling is being republished.
    retry_failed_clean_results_mid_pass_invalidation(|db, other| {
        db.start_review_skeleton_with(other, None).unwrap();
    });
}

#[test]
fn retry_failed_clean_results_keeps_failed_when_silent_enabled_mid_pass() {
    let (root, db, pr_ids) = two_due_failed_clean_results("silent");
    let due_now = crate::db::now_millis() + CLEAN_RESULT_RETRY_INTERVAL_MS + 1;
    let mut published = Vec::new();
    let mut silenced = false;
    retry_failed_clean_results_with(&db, &root, due_now, |db, result, _, _, _| {
        published.push(result.pr_id);
        if !silenced {
            silenced = true;
            db.save_setting("review_result_mode", "silent").unwrap();
        }
        Ok(())
    });
    assert_eq!(published.len(), 1);
    let other = if published[0] == pr_ids[0] {
        pr_ids[1]
    } else {
        pr_ids[0]
    };
    let leftover = db.review_result(other).unwrap().unwrap();
    assert_eq!(leftover.publication_status, "failed");
    assert_eq!(leftover.error_message.as_deref(), Some("boom"));

    // Re-enabling result comments makes the skipped result retryable again.
    db.save_setting("review_result_mode", "result_comment").unwrap();
    let mut republished = Vec::new();
    retry_failed_clean_results_with(&db, &root, due_now, |_, result, _, _, _| {
        republished.push(result.pr_id);
        Ok(())
    });
    assert_eq!(republished, vec![other]);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

static SINGLE_FLIGHT_GATE: AtomicBool = AtomicBool::new(false);

#[test]
fn spawn_single_flight_runs_once_and_recovers() {
    fn wait_gate() {
        for _ in 0..200 {
            if !SINGLE_FLIGHT_GATE.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("single-flight gate was not released");
    }
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    assert!(spawn_single_flight(&SINGLE_FLIGHT_GATE, move || {
        started_tx.send(()).unwrap();
        rx.recv().unwrap();
    }));
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let ran = Arc::new(AtomicBool::new(false));
    let ran_task = ran.clone();
    assert!(!spawn_single_flight(&SINGLE_FLIGHT_GATE, move || {
        ran_task.store(true, Ordering::Relaxed);
    }));
    tx.send(()).unwrap();
    wait_gate();
    assert!(!ran.load(Ordering::Relaxed));
    assert!(spawn_single_flight(&SINGLE_FLIGHT_GATE, || {}));
    wait_gate();
    assert!(spawn_single_flight(&SINGLE_FLIGHT_GATE, || panic!("boom")));
    wait_gate();
    assert!(spawn_single_flight(&SINGLE_FLIGHT_GATE, || {}));
    wait_gate();
}

#[test]
fn a_review_with_only_suggestions_auto_posts_without_a_person() {
    let root = std::env::temp_dir().join(format!(
        "ocr-suggestions-auto-post-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_setting("auto_post_mode", "comment").unwrap();
    let pr_id = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "Fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: None,
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "ready".into(),
            status_reason: None,
        })
        .unwrap();
    let section = crate::review::evidence::suggestions_section(&[
        serde_json::json!({"path":"src/a.rs","line":2,"title":"Stale test name","detail":"Detail","action":"Rename it"}),
    ]);
    let body = format!("Nada a sinalizar.\n\n{section}");
    let run_id = db.start_review_skeleton_with(pr_id, None).unwrap();
    db.set_review_run_running(run_id, "[]").unwrap();
    db.store_finding_audit(
        run_id,
        &crate::review::findings::FindingAudit {
            evidence_warnings: None,
            head_sha: "head".into(),
            complete: true,
            original_body: body.clone(),
            diff: None,
            candidates: vec![],
        },
    )
    .unwrap();
    db.finish_review_run(run_id, &body, None, "[]").unwrap();
    let revision = db
        .auto_confirm_generated_finding_selection(pr_id, run_id)
        .unwrap()
        .expect("a review with only suggestions has nothing for a person to confirm");
    let run = db.review_run_by_id(run_id).unwrap().unwrap();
    assert!(run.review_body.unwrap().ends_with(section.trim()));
    assert!(db
        .try_begin_review_post(run_id, pr_id, "head", "COMMENT", "comment", "auto", Some(revision))
        .unwrap());
}
