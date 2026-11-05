use super::*;

fn fixture_repo() -> (std::path::PathBuf, impl Fn(&[&str]) -> String) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "ocr-coverage-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    let root_for_git = root.clone();
    let git = move |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&root_for_git)
            .args(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    (root, git)
}

fn legacy_fixture() -> (std::path::PathBuf, crate::db::Database, i64, i64, String) {
    let (root, git) = fixture_repo();
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("code.rs"), "before\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let merge_base = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-qb", "feature"]);
    std::fs::write(root.join("code.rs"), "after\n").unwrap();
    git(&["commit", "-qam", "feature"]);
    let head = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "main"]);
    std::fs::write(root.join("main-only.rs"), "unreviewed base update\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "advance base"]);
    let base = git(&["rev-parse", "HEAD"]);
    let db = crate::db::Db::for_test_at(root.join("state.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    let pr = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "Legacy PR".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: head.clone(),
            base_sha: Some(base.clone()),
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "posted".into(),
            status_reason: None,
        })
        .unwrap();
    let conn = db.handle().open().unwrap();
    // Run ids start apart from the repository id 1, so a check that mixes them up shows.
    conn.execute("SELECT setval(pg_get_serial_sequence('review_runs', 'id'), 41)", [])
        .unwrap();
    let run = db.start_review_skeleton_with(pr, Some("fixture")).unwrap();
    conn.execute("UPDATE review_runs SET status='running',review_body='Reviewed',reviewed_head_sha=$2,reviewed_base_sha=$3,stacked_base_ref='main',finished_at=1 WHERE id=$1", crate::pg_params![run, head, base]).unwrap();
    let plan = serde_json::json!({"repoId":1,"baseRef":"main","reviewedBaseSha":base,"mergeBaseSha":merge_base,"headSha":head,"baseSha":head,"priorSha":null,"incremental":false,"range":format!("{base}...{head}"),"changedFiles":["code.rs"],"relatedFiles":[],"testFiles":[],"conventionFiles":[],"packages":[],"dependentPackages":[],"dependencyEdges":[],"omittedCount":0});
    db.save_review_context_snapshot(1, pr, Some(run), &head, Some(&head), None, &plan.to_string(), 1, 0)
        .unwrap();
    let mut audit = crate::review::findings::seed_audit(&serde_json::json!({"comments":[]}), &head, "");
    audit.complete = true;
    db.store_finding_audit(run, &audit).unwrap();
    conn.execute("UPDATE review_runs SET status='posted' WHERE id=$1", [run])
        .unwrap();
    (root, db, pr, run, head)
}

#[test]
fn limited_reviews_never_supply_incremental_or_recovered_coverage() {
    let (root, db, pr, run, head) = legacy_fixture();
    let cancel = AtomicBool::new(false);
    let coverage = snapshot_for_reuse(&root, &db, 1, pr, &head, &cancel).unwrap().unwrap();
    assert_eq!(db.last_reviewed_sha(pr).unwrap(), Some(head.clone()));
    let mut audit = db.load_finding_audit(run).unwrap().unwrap();
    audit.evidence_warnings = Some(vec![crate::review::warnings::EvidenceWarning {
        reason: "insufficient_evidence".into(),
        path: Some("code.rs".into()),
        reference: None,
    }]);
    let conn = db.handle().open().unwrap();
    conn.execute(
        "UPDATE review_finding_audits SET audit_json=$1 WHERE run_id=$2",
        crate::pg_params![serde_json::to_string(&audit).unwrap(), run],
    )
    .unwrap();
    assert!(db.last_reviewed_sha(pr).unwrap().is_none());
    assert!(db.last_review_scope(pr).unwrap().is_none());
    assert!(db
        .historical_review_coverage_for_head(1, pr, &head, false)
        .unwrap()
        .is_none());
    assert!(db.historical_review_coverage_candidates(1).unwrap().is_empty());
    assert!(snapshot_for_reuse(&root, &db, 1, pr, &head, &cancel).unwrap().is_none());
    let patch_ids = vec!["verified-patch".to_string()];
    let input = crate::db::ReviewCoverageInput {
        repo_id: 1,
        pr_id: pr,
        run_id: run,
        base_ref: &coverage.scope.base_ref,
        base_sha: &coverage.scope.base_sha,
        merge_base_sha: &coverage.merge_base_sha,
        head_sha: &head,
        changed_files: &coverage.changed_files,
        covered_patch_ids: &patch_ids,
        diff_hash: &coverage.diff_hash,
        diff_text: &coverage.diff_text,
        source_json: Some("{}"),
    };
    db.begin_review_coverage(&input).unwrap();
    db.finalize_review_coverage(run).unwrap();
    assert!(!db.review_coverage_exists(run).unwrap());
    db.begin_review_coverage(&input).unwrap();
    conn.execute("UPDATE review_coverage SET status='valid' WHERE run_id=$1", [run])
        .unwrap();
    assert!(db.review_coverage_snapshot(pr, &head).unwrap().is_none());
    assert!(db.review_coverage_patch_ids_for_pr(pr).unwrap().unwrap().is_empty());
    assert!(db.review_coverage_patch_ids(1).unwrap().unwrap().is_empty());
    drop(conn);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_full_coverage_is_recovered_read_only_using_the_recorded_merge_base() {
    let (root, db, pr, run, head) = legacy_fixture();
    let cancel = AtomicBool::new(false);
    let candidate = db
        .historical_review_coverage_for_head(1, pr, &head, false)
        .unwrap()
        .expect("Legacy candidate required");
    let plan: crate::db::ReviewContextPlan = serde_json::from_str(candidate.manifest_json.as_deref().unwrap()).unwrap();
    assert_eq!(plan.repo_id, 1);
    assert!(!plan.incremental);
    assert_eq!(plan.head_sha.as_deref(), Some(head.as_str()));
    assert!(db.load_finding_audit(run).unwrap().unwrap().complete);
    let coverage = snapshot_for_reuse(&root, &db, 1, pr, &head, &cancel)
        .unwrap()
        .expect("Completed full legacy review must be recoverable");
    assert_eq!(coverage.run_id, run);
    assert_eq!(coverage.changed_files, ["code.rs"]);
    assert!(!coverage.diff_text.contains("main-only.rs"));
    crate::review::scope::verify_coverage(&root, &coverage, &cancel).unwrap();
    assert!(!db.review_coverage_exists(run).unwrap());
    let readonly = crate::db::Db::for_test_at(root.join("state.sqlite"))
        .open_read()
        .unwrap();
    assert!(snapshot_for_reuse(&root, &readonly, 1, pr, &head, &cancel)
        .unwrap()
        .is_some());
    assert!(snapshot_for_reuse(&root, &db, 2, pr, &head, &cancel).unwrap().is_none());
    assert!(snapshot_for_reuse(&root, &db, 1, pr + 1, &head, &cancel)
        .unwrap()
        .is_none());
    assert!(snapshot_for_reuse(&root, &db, 1, pr, &head, &AtomicBool::new(true)).is_err());
    backfill_historical(&root, &db.handle(), 1, &cancel).unwrap();
    let backfilled = db.review_coverage_snapshot(pr, &head).unwrap().unwrap();
    assert_eq!(backfilled.diff_text, coverage.diff_text);
    assert_eq!(backfilled.changed_files, coverage.changed_files);
    drop(readonly);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_two_dot_backfill_is_recovered_only_after_verifying_both_snapshots() {
    let (root, db, pr, run, head) = legacy_fixture();
    let cancel = AtomicBool::new(false);
    let scope = db.review_run_scope(run).unwrap().unwrap();
    let range = format!("{}..{head}", scope.base_sha);
    let diff = crate::review::diff_local(&root, &range, &cancel).unwrap();
    let files = crate::review::changed_files_local(&root, &range, &cancel).unwrap();
    let merge_base = crate::review::scope::merge_base(&root, &scope.base_sha, &head, &cancel).unwrap();
    let source = serde_json::json!({"backfilled":true,"sourceRunId":run}).to_string();
    db.begin_review_coverage(&crate::db::ReviewCoverageInput {
        repo_id: 1,
        pr_id: pr,
        run_id: run,
        base_ref: "main",
        base_sha: &scope.base_sha,
        head_sha: &head,
        merge_base_sha: &merge_base,
        changed_files: &files,
        diff_hash: &diff_hash(&diff),
        diff_text: &diff,
        covered_patch_ids: &[],
        source_json: Some(&source),
    })
    .unwrap();
    db.finalize_review_coverage(run).unwrap();
    let repaired = snapshot_for_reuse(&root, &db, 1, pr, &head, &cancel).unwrap().unwrap();
    assert_eq!(repaired.changed_files, ["code.rs"]);
    crate::review::scope::verify_coverage(&root, &repaired, &cancel).unwrap();
    assert_eq!(db.review_coverage_snapshot(pr, &head).unwrap().unwrap().diff_text, diff);
    let conn = db.handle().open().unwrap();
    for mutation in 0..3 {
        conn.execute(
            "UPDATE review_coverage SET source_json=$2,diff_text=$3,diff_hash=$4,changed_files_json=$5 WHERE run_id=$1",
            crate::pg_params![
                run,
                source,
                diff,
                diff_hash(&diff),
                serde_json::to_string(&files).unwrap()
            ],
        )
        .unwrap();
        match mutation {
            0 => {
                conn.execute("UPDATE review_coverage SET source_json='{}' WHERE run_id=$1", [run])
                    .unwrap();
            }
            1 => {
                let altered = format!("{diff}tampered\n");
                conn.execute(
                    "UPDATE review_coverage SET diff_text=$2,diff_hash=$3 WHERE run_id=$1",
                    crate::pg_params![run, altered, diff_hash(&altered)],
                )
                .unwrap();
            }
            _ => {
                conn.execute(
                    "UPDATE review_coverage SET changed_files_json='[]' WHERE run_id=$1",
                    [run],
                )
                .unwrap();
            }
        }
        assert!(snapshot_for_reuse(&root, &db, 1, pr, &head, &cancel).is_err());
    }
    drop(conn);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_recovery_rejects_partial_malformed_and_uncompleted_proofs() {
    for mutation in 0..7 {
        let (root, db, pr, run, head) = legacy_fixture();
        let conn = db.handle().open().unwrap();
        match mutation {
            0 => {
                conn.execute("UPDATE review_runs SET status='failed' WHERE id=$1", [run])
                    .unwrap();
            }
            1 => {
                conn.execute("UPDATE review_finding_audits SET audit_json=jsonb_set(audit_json::jsonb,'{complete}','false')::text WHERE run_id=$1", [run]).unwrap();
            }
            2 => {
                conn.execute("UPDATE review_context_snapshots SET manifest_json=jsonb_set(manifest_json::jsonb,'{incremental}','true')::text WHERE review_run_id=$1", [run]).unwrap();
            }
            3 => {
                conn.execute("UPDATE review_context_snapshots SET manifest_json=jsonb_set(manifest_json::jsonb,'{changedFiles}','[]')::text WHERE review_run_id=$1", [run]).unwrap();
            }
            4 => {
                conn.execute("UPDATE review_context_snapshots SET manifest_json=jsonb_set(manifest_json::jsonb,'{range}','\"wrong\"')::text WHERE review_run_id=$1", [run]).unwrap();
            }
            5 => {
                conn.execute(
                    "UPDATE review_context_snapshots SET manifest_json='broken' WHERE review_run_id=$1",
                    [run],
                )
                .unwrap();
            }
            _ => {
                conn.execute("UPDATE review_finding_audits SET audit_json=jsonb_set(audit_json::jsonb,'{headSha}','\"wrong\"')::text WHERE run_id=$1", [run]).unwrap();
            }
        }
        assert!(
            snapshot_for_reuse(&root, &db, 1, pr, &head, &AtomicBool::new(false))
                .unwrap()
                .is_none(),
            "mutation {mutation}"
        );
        drop(conn);
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn a_push_with_only_content_excluded_files_keeps_the_previous_outcome() {
    let (root, db, pr, previous_run, previous_head) = legacy_fixture();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let prior = db.review_run_scope(previous_run).unwrap().unwrap();
    git(&["checkout", "-q", "feature"]);
    git(&["merge", "--no-ff", "-qm", "sync main", "main"]);
    // An oversized fixture is only known to be excluded once the archive is frozen.
    std::fs::write(root.join("fixture.txt"), "x".repeat(1_000_001)).unwrap();
    git(&["add", "fixture.txt"]);
    git(&["commit", "-qm", "large fixture"]);
    let head = git(&["rev-parse", "HEAD"]);
    git(&["remote", "add", "origin", root.to_str().unwrap()]);
    git(&["update-ref", "refs/merge-requests/1/head", &head]);
    let conn = db.handle().open().unwrap();
    conn.execute(
        "UPDATE pull_requests SET head_sha=$2 WHERE id=$1",
        crate::pg_params![pr, head],
    )
    .unwrap();
    let run = db.start_review_skeleton_with(pr, Some("fixture")).unwrap();
    db.set_review_run_running(run, "[]").unwrap();
    // The freeze needs the repository at `<repos>/acme/fixture`.
    let repos = std::env::temp_dir().join(format!("ocr-layout-{}", crate::sandbox::secret().unwrap()));
    let layout = repos.join("acme/fixture");
    std::fs::create_dir_all(layout.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&root, &layout).unwrap();
    let input = crate::review::ReviewInputs {
        frozen: None,
        run_id: run,
        repo_id: 1,
        pr_id: pr,
        repo_path: layout.clone(),
        full_name: "acme/fixture".into(),
        number: 1,
        base_ref: "main".into(),
        fixed_head_sha: None,
        offline: false,
        expected_scope: None,
        prior_scope: Some(prior),
        last_reviewed_sha: Some(previous_head),
        force_full: false,
        reprocess: false,
        skill_content: "Review correctness".into(),
        unresolved_review_head: None,
        // With suggestions off (the default) the review keeps none, not even a pending one's.
        unresolved_review_body: Some(format!(
            "Nada a sinalizar.\n\n{}",
            crate::review::evidence::suggestions_section(&[serde_json::json!(
                {"path":"code.rs","line":1,"title":"Stale test name","detail":"Detail","action":"Rename it"}
            )])
        )),
        pr_title: "Sync main".into(),
        pr_body: String::new(),
        model: "fixture".into(),
        review_depth_mode: "auto".into(),
        review_deep_threshold: 20,
        review_chunk_size: 5,
        review_parallel_chunks: 1,
        db_handle: Some(db.handle()),
        archive_dir: std::env::temp_dir().join(format!("ocr-review-archives-{}", crate::sandbox::secret().unwrap())),
        budget: None,
    };
    let output = crate::review::execute(&input, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert!(output.no_new_changes, "{}", output.body);
    assert!(output.body.contains("fixture.txt") && output.body.contains("not a new clean review"));
    assert_eq!(db.review_run_by_id(run).unwrap().unwrap().status, "running");
    let ids: String = conn
        .query_row(
            "SELECT covered_patch_ids_json FROM review_coverage WHERE run_id=$1",
            [run],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ids, "[]", "a run that reviewed nothing covers no patch");
    drop(conn);
    drop(db);
    std::fs::remove_dir_all(&repos).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_merge_with_only_excluded_delta_finishes_without_inference() {
    let (root, db, pr, previous_run, previous_head) = legacy_fixture();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let prior = db.review_run_scope(previous_run).unwrap().unwrap();
    git(&["checkout", "-q", "feature"]);
    git(&["merge", "--no-ff", "-qm", "sync main", "main"]);
    std::fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: 9\n").unwrap();
    git(&["add", "pnpm-lock.yaml"]);
    git(&["commit", "-qm", "lockfile sync"]);
    let head = git(&["rev-parse", "HEAD"]);
    git(&["remote", "add", "origin", root.to_str().unwrap()]);
    git(&["update-ref", "refs/merge-requests/1/head", &head]);
    let conn = db.handle().open().unwrap();
    conn.execute(
        "UPDATE pull_requests SET head_sha=$2 WHERE id=$1",
        crate::pg_params![pr, head],
    )
    .unwrap();
    let run = db.start_review_skeleton_with(pr, Some("fixture")).unwrap();
    db.set_review_run_running(run, "[]").unwrap();
    let input = crate::review::ReviewInputs {
        frozen: None,
        run_id: run,
        repo_id: 1,
        pr_id: pr,
        repo_path: root.clone(),
        full_name: "acme/fixture".into(),
        number: 1,
        base_ref: "main".into(),
        fixed_head_sha: None,
        offline: false,
        expected_scope: None,
        prior_scope: Some(prior),
        last_reviewed_sha: Some(previous_head),
        force_full: false,
        reprocess: false,
        skill_content: "Review correctness".into(),
        unresolved_review_head: None,
        // With suggestions off (the default) the review keeps none, not even a pending one's.
        unresolved_review_body: Some(format!(
            "Nada a sinalizar.\n\n{}",
            crate::review::evidence::suggestions_section(&[serde_json::json!(
                {"path":"code.rs","line":1,"title":"Stale test name","detail":"Detail","action":"Rename it"}
            )])
        )),
        pr_title: "Sync main".into(),
        pr_body: String::new(),
        model: "fixture".into(),
        review_depth_mode: "auto".into(),
        review_deep_threshold: 20,
        review_chunk_size: 5,
        review_parallel_chunks: 1,
        db_handle: Some(db.handle()),
        archive_dir: std::env::temp_dir().join(format!("ocr-review-archives-{}", crate::sandbox::secret().unwrap())),
        budget: None,
    };
    let output = crate::review::execute(&input, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert!(output.no_new_changes);
    assert!(output.body.contains("pnpm-lock.yaml") && output.body.contains("excluded, not reviewed"));
    assert!(output.body.contains("not a new clean review"));
    assert!(!output.body.contains("Suggestions"), "{}", output.body);
    let audit = db.load_finding_audit(run).unwrap().unwrap();
    assert!(!audit.original_body.contains("Suggestions"));
    let plan = db.context_plan_for_review_run(run).unwrap().unwrap();
    assert!(plan.changed_files.is_empty());
    let selection = plan.scope_selection.unwrap();
    assert_eq!(selection.source_run_id, Some(previous_run));
    assert_eq!(selection.reused_files, ["code.rs"]);
    assert_eq!(selection.excluded_files.len(), 1);
    let ids: String = conn
        .query_row(
            "SELECT covered_patch_ids_json FROM review_coverage WHERE run_id=$1",
            [run],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ids, "[]");
    assert!(!db.review_coverage_exists(previous_run).unwrap());
    drop(conn);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

// One test per integration, so the three run in parallel.
#[test]
fn stacked_child_merge_finishes_without_inference() {
    stacked_child_integration_finishes_without_inference("merge");
}

#[test]
fn stacked_child_squash_finishes_without_inference() {
    stacked_child_integration_finishes_without_inference("squash");
}

#[test]
fn stacked_child_cherry_pick_finishes_without_inference() {
    stacked_child_integration_finishes_without_inference("cherry-pick");
}

fn stacked_child_integration_finishes_without_inference(integration: &str) {
    let (root, db, pr, previous_run, previous_head) = legacy_fixture();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.test"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["checkout", "-qb", "child", &previous_head]);
    std::fs::write(root.join("child.rs"), "reviewed child\n").unwrap();
    std::fs::write(root.join("code.rs"), "reviewed child update\n").unwrap();
    git(&["add", "child.rs", "code.rs"]);
    git(&["commit", "-qm", "child"]);
    let child_head = git(&["rev-parse", "HEAD"]);
    let child_pr = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 2,
            title: "Child".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: child_head.clone(),
            base_sha: Some(previous_head.clone()),
            base_ref: Some("feature".into()),
            head_ref: Some("child".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "posted".into(),
            status_reason: None,
        })
        .unwrap();
    let child_run = db.start_review_skeleton_with(child_pr, Some("fixture")).unwrap();
    let cancel = AtomicBool::new(false);
    let range = format!("{previous_head}...{child_head}");
    let diff = crate::review::diff_local(&root, &range, &cancel).unwrap();
    let files = crate::review::changed_files_local(&root, &range, &cancel).unwrap();
    db.begin_review_coverage(&crate::db::ReviewCoverageInput {
        repo_id: 1,
        pr_id: child_pr,
        run_id: child_run,
        base_ref: "feature",
        base_sha: &previous_head,
        merge_base_sha: &previous_head,
        head_sha: &child_head,
        changed_files: &files,
        covered_patch_ids: &current_patch_ids(&root, &previous_head, &child_head, &diff, &cancel).unwrap(),
        diff_hash: &diff_hash(&diff),
        diff_text: &diff,
        source_json: None,
    })
    .unwrap();
    db.finalize_review_coverage(child_run).unwrap();
    let conn = db.handle().open().unwrap();
    conn.execute(
        "UPDATE review_runs SET status='posted',review_body='Reviewed child',finished_at=1 WHERE id=$1",
        [child_run],
    )
    .unwrap();
    git(&["checkout", "-q", "feature"]);
    match integration {
        "merge" => {
            git(&["merge", "--no-ff", "-qm", "integrate child", "child"]);
        }
        "squash" => {
            git(&["merge", "--squash", "child"]);
            git(&["commit", "-qm", "squash child"]);
        }
        _ => {
            git(&["cherry-pick", &child_head]);
        }
    }
    let head = git(&["rev-parse", "HEAD"]);
    git(&["remote", "add", "origin", root.to_str().unwrap()]);
    git(&["update-ref", "refs/merge-requests/1/head", &head]);
    conn.execute(
        "UPDATE pull_requests SET head_sha=$2 WHERE id=$1",
        crate::pg_params![pr, head],
    )
    .unwrap();
    let run = db.start_review_skeleton_with(pr, Some("fixture")).unwrap();
    db.set_review_run_running(run, "[]").unwrap();
    let input = crate::review::ReviewInputs {
        frozen: None,
        run_id: run,
        repo_id: 1,
        pr_id: pr,
        repo_path: root.clone(),
        full_name: "acme/fixture".into(),
        number: 1,
        base_ref: "main".into(),
        fixed_head_sha: None,
        offline: false,
        expected_scope: None,
        prior_scope: db.review_run_scope(previous_run).unwrap(),
        last_reviewed_sha: Some(previous_head),
        force_full: false,
        reprocess: false,
        skill_content: "Review correctness".into(),
        unresolved_review_head: None,
        unresolved_review_body: None,
        pr_title: "Integrate reviewed child".into(),
        pr_body: String::new(),
        model: "fixture".into(),
        review_depth_mode: "auto".into(),
        review_deep_threshold: 20,
        review_chunk_size: 5,
        review_parallel_chunks: 1,
        db_handle: Some(db.handle()),
        archive_dir: std::env::temp_dir().join(format!("ocr-review-archives-{}", crate::sandbox::secret().unwrap())),
        budget: None,
    };
    let output = crate::review::execute(&input, &cancel, &mut |_, _| {})
        .expect("Reviewed child must be skipped before model preparation");
    assert!(output.no_new_changes, "{integration}");
    let plan = db.context_plan_for_review_run(run).unwrap().unwrap();
    assert!(plan.changed_files.is_empty());
    assert_eq!(plan.scope_selection.unwrap().reused_files, ["child.rs", "code.rs"]);
    let source_json: String = conn
        .query_row(
            "SELECT source_json FROM review_coverage WHERE run_id=$1",
            [run],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&source_json).unwrap()["stackedSourceRunIds"],
        serde_json::json!([child_run])
    );
    let previous = snapshot_for_reuse(&root, &db, 1, pr, input.last_reviewed_sha.as_deref().unwrap(), &cancel)
        .unwrap()
        .unwrap();
    let current = crate::review::scope::ReviewScope {
        head_sha: head,
        base_ref: "main".into(),
        base_sha: git(&["rev-parse", "main"]),
    };
    let select = |current: &crate::review::scope::ReviewScope, force_full: bool| {
        let full =
            crate::review::changed_files_local(&root, &format!("{}...{}", current.base_sha, current.head_sha), &cancel)
                .unwrap();
        let (mut changed, _) = crate::review::incremental_review_changes(
            &root,
            &previous.scope.head_sha,
            &current.head_sha,
            &full,
            &cancel,
        )
        .unwrap();
        let mut selection = crate::review::scope::ScopeSelection {
            mode: "incremental".into(),
            requested_full: force_full,
            source_run_id: Some(previous_run),
            full_pr_files: full,
            reused_files: vec![],
            excluded_files: vec![],
        };
        let sources = reuse_stacked_scope(
            &root,
            &db,
            1,
            pr,
            Some(&previous),
            current,
            &mut changed,
            &mut selection,
            &cancel,
        )
        .unwrap();
        (changed, sources)
    };
    assert_eq!(select(&current, true).0, ["child.rs", "code.rs"]);
    for status in ["failed", "cancelled", "pending", "running"] {
        conn.execute(
            "UPDATE review_runs SET status=$2 WHERE id=$1",
            crate::pg_params![child_run, status],
        )
        .unwrap();
        assert_eq!(select(&current, false).0, ["child.rs", "code.rs"], "{status}");
    }
    conn.execute("UPDATE review_runs SET status='posted' WHERE id=$1", [child_run])
        .unwrap();
    conn.execute(
        "UPDATE review_coverage SET diff_hash='invalid' WHERE run_id=$1",
        [child_run],
    )
    .unwrap();
    assert_eq!(select(&current, false).0, ["child.rs", "code.rs"]);
    conn.execute(
        "UPDATE review_coverage SET diff_hash=$2 WHERE run_id=$1",
        crate::pg_params![child_run, diff_hash(&diff)],
    )
    .unwrap();
    assert!(select(&current, false).0.is_empty());
    std::fs::write(root.join("child.rs"), "reviewed child \n").unwrap();
    std::fs::write(root.join("new.rs"), "unreviewed\n").unwrap();
    git(&["add", "child.rs", "new.rs"]);
    git(&["commit", "-qm", "new author changes"]);
    let current = crate::review::scope::ReviewScope {
        head_sha: git(&["rev-parse", "HEAD"]),
        ..current
    };
    let (changed, sources) = select(&current, false);
    assert_eq!(changed, ["child.rs", "new.rs"]);
    assert_eq!(sources, [child_run]);
    drop(conn);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "Requires read-only operational history and Git objects; no inference or database writes"]
fn persisted_scope_recovery_without_inference() {
    use std::io::Read;
    let mut raw = String::new();
    std::io::stdin().take(8_388_609).read_to_string(&mut raw).unwrap();
    assert!(raw.len() <= 8_388_608);
    let packet: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let db = crate::db::Db::diagnostic_from_env().open_read().unwrap();
    let repo = std::path::PathBuf::from(std::env::var("OCR_DIAGNOSTIC_REPO").unwrap());
    let run = db.review_run_by_id(packet["runId"].as_i64().unwrap()).unwrap().unwrap();
    assert_eq!(run.status, "failed");
    assert!(run.finished_at.is_some());
    let source_run = packet["sourceRunId"].as_i64().unwrap();
    let source = db.review_run_by_id(source_run).unwrap().unwrap();
    assert_eq!(source.pr_id, run.pr_id);
    let (repo_id, repository, number, _) = db.pr_review_coords(run.pr_id).unwrap();
    let prior = db.review_run_scope(source_run).unwrap().unwrap();
    let current: crate::review::scope::ReviewScope = serde_json::from_value(packet["scope"].clone()).unwrap();
    assert_eq!(current.head_sha, run.head_sha);
    let cancel = AtomicBool::new(false);
    let persisted = db.review_coverage_exists(source_run).unwrap();
    let stored_hash = db
        .review_coverage_snapshot(run.pr_id, &prior.head_sha)
        .unwrap()
        .map(|coverage| coverage.diff_hash);
    let coverage = snapshot_for_reuse(&repo, &db, repo_id, run.pr_id, &prior.head_sha, &cancel)
        .unwrap()
        .unwrap();
    assert_eq!(coverage.run_id, source_run);
    let mut selected = crate::review::scope::prepare_scope_reuse(
        &repo,
        Some(&prior),
        Some(&prior.head_sha),
        &current,
        Some(&coverage),
        false,
        &cancel,
    )
    .unwrap()
    .unwrap();
    crate::review::scope::exclude_policy_paths(&mut selected.changed_files, &mut selected.selection).unwrap();
    assert_eq!(
        serde_json::to_value(&selected.changed_files).unwrap(),
        packet["expectedChangedFiles"]
    );
    assert_eq!(
        selected.selection.reused_files.len(),
        packet["expectedReusedCount"].as_u64().unwrap() as usize
    );
    assert_eq!(
        selected.selection.excluded_files.len(),
        packet["expectedExcludedCount"].as_u64().unwrap() as usize
    );
    assert!(!selected.selection.requested_full);
    assert_eq!(db.review_coverage_exists(source_run).unwrap(), persisted);
    let recorded = db.context_plan_for_review_run(source_run).unwrap().unwrap();
    let mut plan = recorded.clone();
    plan.prior_sha = Some(prior.head_sha.clone());
    plan.merge_base_sha =
        Some(crate::review::scope::merge_base(&repo, &current.base_sha, &current.head_sha, &cancel).unwrap());
    let settings = db.settings().unwrap();
    let unresolved = db.latest_unresolved_review(run.pr_id).unwrap();
    println!(
        "SCOPE_RECOVERY_RESULT {}",
        serde_json::json!({"runId":run.id,"sourceRunId":source_run,"legacyRecovered":stored_hash.as_deref()!=Some(coverage.diff_hash.as_str()),"changedFiles":selected.changed_files,"selection":selected.selection,"modelCalls":0,"databaseWrites":false})
    );
    println!(
        "RECOVERED_SCOPE_FIXTURE {}",
        serde_json::json!({"repository":repository,"number":number,"prId":run.pr_id,"repoId":repo_id,"runId":run.id,"sourcePlanRun":source_run,"recordedSourcePlan":recorded,"model":run.model,"scope":current,"priorScope":prior,"priorCoverage":coverage,"plan":plan,"chunkSize":settings.review_chunk_size,"skill":db.active_skill_content(),"unresolved":unresolved,"authorizedDependencies":db.monitored_repos().unwrap().into_iter().map(|repo|repo.full_name).collect::<Vec<_>>() })
    );
}

#[test]
fn interleaved_unknown_and_known_patches_use_file_content_proofs() {
    let (root, git) = fixture_repo();
    let cancel = AtomicBool::new(false);
    git(&["init", "-q", "-b", "main"]);
    for path in ["a.rs", "b.rs"] {
        std::fs::write(root.join(path), "base\n").unwrap();
    }
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-qb", "feature"]);
    for path in ["a.rs", "a.md", "b.rs", "b.md"] {
        std::fs::write(root.join(path), "reviewed\n").unwrap();
        git(&["add", path]);
        git(&["commit", "-qm", path]);
    }
    let head = git(&["rev-parse", "HEAD"]);
    let diff = crate::review::diff_local(&root, &format!("{base}...{head}"), &cancel).unwrap();
    let known = current_patch_ids(&root, &base, &head, &diff, &cancel).unwrap();
    let coverage = crate::review::scope::CoverageSnapshot {
        run_id: 1,
        scope: crate::review::scope::ReviewScope {
            head_sha: head,
            base_ref: "main".into(),
            base_sha: base.clone(),
        },
        merge_base_sha: base.clone(),
        changed_files: vec!["a.md".into(), "a.rs".into(), "b.md".into(), "b.rs".into()],
        diff_hash: diff_hash(&diff),
        diff_text: diff,
    };
    git(&["checkout", "-q", "main"]);
    std::fs::write(root.join("main-only.md"), "base change\n").unwrap();
    git(&["add", "main-only.md"]);
    git(&["commit", "-qm", "advance main"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-qb", "rewritten"]);
    for path in ["a.rs", "a.md", "b.rs", "b.md"] {
        std::fs::write(
            root.join(path),
            if path.ends_with(".rs") {
                "new behavior\n"
            } else {
                "reviewed\n"
            },
        )
        .unwrap();
        git(&["add", path]);
        git(&["commit", "-qm", path]);
    }
    let current = crate::review::scope::ReviewScope {
        head_sha: git(&["rev-parse", "HEAD"]),
        base_ref: "main".into(),
        base_sha: base,
    };
    let patches = commit_patches(&root, &format!("{}..{}", current.base_sha, current.head_sha), &cancel).unwrap();
    assert_eq!(
        patches
            .iter()
            .map(|patch| known.contains(&patch.patch_id))
            .collect::<Vec<_>>(),
        [false, true, false, true]
    );
    let selected = crate::review::scope::prepare_scope_reuse(
        &root,
        Some(&coverage.scope),
        Some(&coverage.scope.head_sha),
        &current,
        Some(&coverage),
        false,
        &cancel,
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.changed_files, ["a.rs", "b.rs"]);
    assert_eq!(selected.selection.reused_files, ["a.md", "b.md"]);
    assert_eq!(selected.selection.mode, "content_reuse");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn patch_ids_survive_a_cherry_pick_with_rewritten_commit_ids() {
    let (root, git) = fixture_repo();
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-qb", "source"]);
    std::fs::write(root.join("source.txt"), "source\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "source"]);
    let source_head = git(&["rev-parse", "HEAD"]);
    let source_diff = crate::review::diff_local(
        &root,
        &format!("{base}...{source_head}"),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let source_ids = current_patch_ids(
        &root,
        &base,
        &source_head,
        &source_diff,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();

    git(&["checkout", "-q", "-b", "target", &base]);
    git(&["cherry-pick", "--no-edit", &source_head]);
    let target_head = git(&["rev-parse", "HEAD"]);
    let target_diff = crate::review::diff_local(
        &root,
        &format!("{base}...{target_head}"),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    let target_ids = current_patch_ids(
        &root,
        &base,
        &target_head,
        &target_diff,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert!(source_ids.iter().any(|id| target_ids.contains(id)));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stacked_rebase_reuses_verified_patch_coverage_and_reviews_only_new_commit() {
    let (root, git) = fixture_repo();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);

    git(&["checkout", "-qb", "parent"]);
    std::fs::write(root.join("parent.txt"), "parent\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "parent"]);
    let parent_head = git(&["rev-parse", "HEAD"]);

    git(&["checkout", "-qb", "child"]);
    std::fs::write(root.join("child.txt"), "child\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "child"]);
    let child_head = git(&["rev-parse", "HEAD"]);

    let db_root = root.with_extension("db-dir");
    let db_path = db_root.join("opencodereview.sqlite");
    let db = crate::db::Db::for_test_at(db_path.clone()).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    let parent_pr = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "Parent".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: parent_head.clone(),
            base_sha: Some(base.clone()),
            base_ref: Some("main".into()),
            head_ref: Some("parent".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "manual_required".into(),
            status_reason: None,
        })
        .unwrap();
    let child_pr = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 2,
            title: "Child".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: child_head.clone(),
            base_sha: Some(parent_head.clone()),
            base_ref: Some("parent".into()),
            head_ref: Some("child".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "manual_required".into(),
            status_reason: None,
        })
        .unwrap();

    let save_coverage = |pr_id: i64, base_ref: &str, base_sha: &str, head_sha: &str| {
        let range = format!("{base_sha}...{head_sha}");
        let diff = crate::review::diff_local(&root, &range, &cancel).unwrap();
        let changed_files = crate::review::changed_files_local(&root, &range, &cancel).unwrap();
        let patch_ids = current_patch_ids(&root, base_sha, head_sha, &diff, &cancel).unwrap();
        let merge_base = crate::review::scope::merge_base(&root, base_sha, head_sha, &cancel).unwrap();
        let run_id = db.start_review_skeleton_with(pr_id, Some("fixture-model")).unwrap();
        db.begin_review_coverage(&crate::db::ReviewCoverageInput {
            repo_id: 1,
            pr_id,
            run_id,
            base_ref,
            base_sha,
            merge_base_sha: &merge_base,
            head_sha,
            changed_files: &changed_files,
            covered_patch_ids: &patch_ids,
            diff_hash: &diff_hash(&diff),
            diff_text: &diff,
            source_json: None,
        })
        .unwrap();
        db.finalize_review_coverage(run_id).unwrap();
    };

    save_coverage(parent_pr, "main", &base, &parent_head);
    let child_diff = crate::review::diff_local(&root, &format!("{parent_head}...{child_head}"), &cancel).unwrap();
    let first = resolve(
        &root,
        &crate::db::Db::for_test_at(db_path.clone()),
        1,
        &parent_head,
        &child_head,
        &child_diff,
        None,
        &cancel,
    )
    .unwrap()
    .unwrap();
    assert_eq!(first.review_base.as_deref(), Some(parent_head.as_str()));
    assert!(!first.fully_covered);

    save_coverage(child_pr, "parent", &parent_head, &child_head);
    git(&["checkout", "-q", "parent"]);
    std::fs::write(root.join("parent-follow-up.txt"), "follow-up\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "parent follow-up"]);
    let rebased_parent = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "child"]);
    git(&["rebase", "-q", "parent"]);
    let rebased_child = git(&["rev-parse", "HEAD"]);

    let rebased_diff =
        crate::review::diff_local(&root, &format!("{rebased_parent}...{rebased_child}"), &cancel).unwrap();
    let rebased = resolve(
        &root,
        &crate::db::Db::for_test_at(db_path.clone()),
        1,
        &rebased_parent,
        &rebased_child,
        &rebased_diff,
        None,
        &cancel,
    )
    .unwrap()
    .unwrap();
    assert_eq!(rebased.review_base.as_deref(), Some(rebased_parent.as_str()));
    assert!(rebased.fully_covered);

    std::fs::write(root.join("child-follow-up.txt"), "new child change\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "child follow-up"]);
    let child_with_new_change = git(&["rev-parse", "HEAD"]);
    let new_diff =
        crate::review::diff_local(&root, &format!("{rebased_parent}...{child_with_new_change}"), &cancel).unwrap();
    let incremental = resolve(
        &root,
        &crate::db::Db::for_test_at(db_path.clone()),
        1,
        &rebased_parent,
        &child_with_new_change,
        &new_diff,
        None,
        &cancel,
    )
    .unwrap()
    .unwrap();
    assert_eq!(incremental.review_base.as_deref(), Some(rebased_child.as_str()));
    assert!(!incremental.fully_covered);

    drop(db);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(db_root).unwrap();
}

#[test]
fn malformed_persisted_coverage_is_quarantined_before_fresh_reuse() {
    let (root, git) = fixture_repo();
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-qb", "feature"]);
    std::fs::write(root.join("change.txt"), "change\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "change"]);
    let head = git(&["rev-parse", "HEAD"]);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let diff = crate::review::diff_local(&root, &format!("{base}...{head}"), &cancel).unwrap();
    let changed_files = crate::review::changed_files_local(&root, &format!("{base}...{head}"), &cancel).unwrap();
    let patch_ids = current_patch_ids(&root, &base, &head, &diff, &cancel).unwrap();
    let merge_base = crate::review::scope::merge_base(&root, &base, &head, &cancel).unwrap();
    let db_root = root.with_extension("db-dir");
    let db_path = db_root.join("opencodereview.sqlite");
    let db = crate::db::Db::for_test_at(db_path.clone()).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    let pr_id = db
        .upsert_pull_request(&crate::db::PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "PR".into(),
            author: "author".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: head.clone(),
            base_sha: Some(base.clone()),
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "open".into(),
            status_reason: None,
        })
        .unwrap();
    let run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    // A second pending run of the PR, as an older version could leave behind; starting one now
    // is refused while the first is active.
    let second_malformed_run_id: i64 = db
        .query_row(
            "INSERT INTO review_runs (pr_id, head_sha, status, created_at, updated_at, model) SELECT pr_id, head_sha, 'pending', created_at, updated_at, model FROM review_runs WHERE id = $1 RETURNING id",
            [run_id],
            |row| row.get(0),
        )
        .unwrap();
    drop(db);
    let conn = crate::db::Db::for_test_at(&db_path).open().unwrap();
    for malformed_run_id in [run_id, second_malformed_run_id] {
        conn.execute(
            "INSERT INTO review_coverage (repo_id, pr_id, run_id, status, base_ref, base_sha, merge_base_sha, head_sha, changed_files_json, covered_patch_ids_json, diff_hash, diff_text, created_at)
             VALUES (1, $1, $2, 'valid', 'main', $3, $4, $5, '[]', 'not-json', 'hash', '', 1)",
            crate::pg_params![pr_id, malformed_run_id, base, merge_base, head],
        ).unwrap();
    }
    drop(conn);

    let db = crate::db::Db::for_test_at(db_path.clone()).open().unwrap();
    assert!(db.review_coverage_patch_ids_for_pr(pr_id).unwrap().is_none());
    assert!(!db.review_coverage_exists(run_id).unwrap());
    assert!(!db.review_coverage_exists(second_malformed_run_id).unwrap());
    // The malformed runs ended; a fresh review of the PR may start.
    db.execute(
        "UPDATE review_runs SET status = 'failed' WHERE id = ANY($1)",
        crate::pg_params![vec![run_id, second_malformed_run_id]],
    )
    .unwrap();
    let fresh_run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    db.begin_review_coverage(&crate::db::ReviewCoverageInput {
        repo_id: 1,
        pr_id,
        run_id: fresh_run_id,
        base_ref: "main",
        base_sha: &base,
        merge_base_sha: &merge_base,
        head_sha: &head,
        changed_files: &changed_files,
        covered_patch_ids: &patch_ids,
        diff_hash: &diff_hash(&diff),
        diff_text: &diff,
        source_json: None,
    })
    .unwrap();
    db.finalize_review_coverage(fresh_run_id).unwrap();
    drop(db);

    let resolution = resolve(
        &root,
        &crate::db::Db::for_test_at(db_path.clone()),
        1,
        &base,
        &head,
        &diff,
        None,
        &cancel,
    )
    .unwrap()
    .unwrap();
    assert!(resolution.usable);
    assert!(resolution.fully_covered);
    assert_eq!(resolution.review_base.as_deref(), Some(base.as_str()));
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(db_root).unwrap();
}

#[test]
fn changed_file_limit_counts_a_rename_once() {
    let (root, git) = fixture_repo();
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("old.txt"), "content\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-qb", "feature"]);
    git(&["mv", "old.txt", "new.txt"]);
    git(&["commit", "-qm", "rename"]);
    let head = git(&["rev-parse", "HEAD"]);
    let targets = crate::review::changed_file_targets_local(
        &root,
        &format!("{base}...{head}"),
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(targets, ["new.txt"]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_scope_stored_with_its_run_id_as_repository_is_still_recovered() {
    let (root, db, pr, run, head) = legacy_fixture();
    let cancel = AtomicBool::new(false);
    let conn = db.handle().open().unwrap();
    let store = |repo_id: i64| {
        conn.execute(
            "UPDATE review_context_snapshots SET manifest_json = jsonb_set(manifest_json::jsonb, '{repoId}', to_jsonb($1::bigint))::text WHERE review_run_id = $2",
            crate::pg_params![repo_id, run],
        )
        .unwrap();
    };
    // Scopes stored before the fix carry their run's temporary index id: the run id, which here
    // differs from the repository id 1.
    assert_ne!(run, 1);
    store(run);
    assert!(snapshot_for_reuse(&root, &db, 1, pr, &head, &cancel).unwrap().is_some());
    // Another repository's scope is still never recovered.
    store(run + 1000);
    assert!(snapshot_for_reuse(&root, &db, 1, pr, &head, &cancel).unwrap().is_none());
    drop(conn);
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}
