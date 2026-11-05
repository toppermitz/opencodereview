use super::*;

#[test]
fn phase_progress_preserves_index_details_and_bounds_logs() {
    let initial = merge_review_phase_progress(None, &crate::review::index_phase_json("running"), "Fetching", 10);
    let ready = merge_review_phase_progress(
        Some(&initial),
        &crate::review::index_phase_json("ok"),
        "Index updated",
        20,
    );
    let deep = r#"[{"id":"index","status":"ok"},{"id":"context","status":"ok"},{"id":"chunks","status":"running"}]"#;
    let merged = merge_review_phase_progress(Some(&ready), deep, &"x".repeat(20_000), 30);
    let phases: Vec<serde_json::Value> = serde_json::from_str(&merged).unwrap();
    assert_eq!(phases[0]["detail"], "Index updated");
    assert_eq!(phases[0]["startedAt"], 10);
    assert_eq!(phases[0]["finishedAt"], 20);
    assert_eq!(phases[0]["events"].as_array().unwrap().len(), 2);
    assert_eq!(phases[0]["events"][0]["message"], "Fetching");
    let duplicate = merge_review_phase_progress(Some(&merged), deep, &"x".repeat(20_000), 40);
    let repeated: Vec<serde_json::Value> = serde_json::from_str(&duplicate).unwrap();
    assert_eq!(repeated[2]["events"].as_array().unwrap().len(), 1);
    assert_eq!(phases[2]["startedAt"], 30);
    assert_eq!(phases[2]["log"].as_str().unwrap().len(), 8_192);
    assert!(phases[1].get("log").is_none());
}

#[test]
fn failed_phase_keeps_its_diagnostic_and_does_not_overwrite_completed_context() {
    let previous = r#"[{"id":"context","status":"ok","detail":"Context ready","startedAt":10,"finishedAt":20},{"id":"chunks","status":"running","detail":"Creating chunks","startedAt":20}]"#;
    let next = r#"[{"id":"context","status":"ok"},{"id":"chunks","status":"failed","inferenceSkipped":true}]"#;
    let merged: serde_json::Value = serde_json::from_str(&merge_review_phase_progress(
        Some(previous),
        next,
        "Chunk preparation failed before model execution",
        30,
    ))
    .unwrap();
    assert_eq!(merged[0]["detail"], "Context ready");
    assert_eq!(merged[1]["detail"], "Chunk preparation failed before model execution");
    assert_eq!(merged[1]["inferenceSkipped"], true);
    assert_eq!(merged[1]["finishedAt"], 30);
}

#[test]
fn phase_history_is_bounded_and_survives_empty_heartbeats() {
    let phase = crate::review::index_phase_json("running");
    let mut progress = phase.clone();
    for index in 0..100 {
        progress = merge_review_phase_progress(Some(&progress), &phase, &format!("Step {index}"), index);
    }
    progress = merge_review_phase_progress(Some(&progress), &phase, "", 101);
    let phases: Vec<serde_json::Value> = serde_json::from_str(&progress).unwrap();
    assert_eq!(phases[0]["events"].as_array().unwrap().len(), 40);
    assert_eq!(phases[0]["detail"], "Step 99");
    assert_eq!(phases[0]["events"][0]["message"], "Step 60");
}

#[test]
fn run_context_does_not_leak_a_previous_same_head_snapshot() {
    let db = Db::for_test().open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/api").unwrap();
    db.conn.execute("INSERT INTO pull_requests (id, repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at) VALUES (1, 1, 1, 'PR', 'a', 'abc', 1, 1, 1)", []).unwrap();
    let first = db.start_review_skeleton_with(1, None).unwrap();
    db.conn
        .execute("UPDATE review_runs SET status = 'superseded' WHERE id = $1", [first])
        .unwrap();
    let second = db.start_review_skeleton_with(1, None).unwrap();
    let plan = serde_json::json!({ "repoId": 1, "changedFiles": ["old.ts"], "relatedFiles": [], "packages": [], "dependentPackages": [], "testFiles": [], "conventionFiles": [], "omittedCount": 0 });
    db.save_review_context_snapshot(1, 1, Some(first), "abc", None, None, &plan.to_string(), 1, 0)
        .unwrap();
    assert!(db.context_plan_for_review_run(second).unwrap().is_none());
    assert_eq!(
        db.context_plan_for_review_run(first).unwrap().unwrap().changed_files,
        vec!["old.ts"]
    );
}

#[test]
fn force_full_next_review_is_consumed_once() {
    let db = Db::for_test().open().expect("open db");
    db.conn.execute("INSERT INTO repos (owner, name, full_name, is_private, created_at, updated_at) VALUES ('acme','api','acme/api',1,1,1)", []).expect("repo");
    let repo_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO pull_requests (repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at) VALUES ($1, 1, 'PR', 'a', 'abc', 1, 1, 1)", params![repo_id]).expect("pr");
    let pr_id = db.conn.last_insert_rowid();
    assert!(!db.take_force_full_next_review(pr_id).expect("initial"));
    db.set_force_full_next_review(pr_id, true).expect("set");
    assert!(db.take_force_full_next_review(pr_id).expect("take"));
    assert!(!db.take_force_full_next_review(pr_id).expect("consumed"));
}

#[test]
fn superseded_review_run_preserves_pending_body() {
    let db = Db::for_test().open().expect("open db");
    db.conn.execute("INSERT INTO repos (owner, name, full_name, is_private, created_at, updated_at) VALUES ('acme','api','acme/api',1,1,1)", []).expect("repo");
    let repo_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO pull_requests (repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at) VALUES ($1, 1, 'PR', 'a', 'newhead', 1, 1, 1)", params![repo_id]).expect("pr");
    let pr_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO review_runs (pr_id, head_sha, status, created_at, updated_at, started_at, model) VALUES ($1, 'oldhead', 'running', 1, 1, 1, 'gpt')", params![pr_id]).expect("run");
    let run_id = db.conn.last_insert_rowid();
    let body = "**[Achado] Stale but valid finding**\n\nDetails";
    let comments = r#"{"comments":[]}"#;

    let finished = db
        .finish_review_run_only(run_id, body, Some(comments), "[]")
        .expect("finish superseded");
    assert!(!finished);
    let pending = db
        .latest_unresolved_review(pr_id)
        .expect("pending query")
        .expect("superseded review retained");
    assert_eq!(pending.0, "oldhead");
    assert_eq!(pending.1, body);
}

#[test]
fn unresolved_review_context_keeps_inline_bodies_and_anchor_metadata() {
    let db = Db::for_test().open().unwrap();
    db.conn.execute("INSERT INTO repos (owner,name,full_name,is_private,created_at,updated_at) VALUES ('acme','api','acme/api',1,1,1)", []).unwrap();
    let repo = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO pull_requests (repo_id,number,title,author,head_sha,last_synced_at,created_at,updated_at) VALUES ($1,1,'PR','a','head',1,1,1)", [repo]).unwrap();
    let pr = db.conn.last_insert_rowid();
    let long_detail = "x".repeat(25_000);
    let first = format!("**[Achado] First concern**\n\n{long_detail}");
    let second = "**[Pergunta] Second concern**\n\nFull confirmation request.";
    let comments = serde_json::json!({"comments":[{"path":"src/first.rs","line":12,"side":"RIGHT","start_line":10,"start_side":"RIGHT","body":first.clone()},{"path":"src/second.rs","line":7,"side":"LEFT","body":second}]}).to_string();
    db.conn.execute("INSERT INTO review_runs (pr_id,head_sha,status,review_body,comments_json,created_at,updated_at) VALUES ($1,'head','superseded','1 achado(s), 1 pergunta(s).',$2,1,1)", params![pr,comments]).unwrap();
    let run = db.conn.last_insert_rowid();
    let fallback = db.latest_unresolved_review(pr).unwrap().unwrap();
    assert_eq!(fallback.0, "head");
    assert!(fallback.1.starts_with("1 achado(s), 1 pergunta(s)."));
    for expected in [
        "**[Achado] First concern**",
        second,
        "src/first.rs",
        "src/second.rs",
        "\"start_line\": 10",
        "\"line\": 12",
        "\"side\": \"LEFT\"",
    ] {
        assert!(
            fallback.1.contains(expected),
            "Missing pending finding content: {expected}"
        );
    }
    assert!(fallback.1.replace('\n', "").contains(&long_detail));
    assert!(fallback.1.lines().all(|line| line.len() < 2_000));
    db.reserve_review_result(&ReviewResultInput {
        pr_id: pr,
        run_id: run,
        actionable_run_id: Some(run),
        observed_head_sha: "head",
        reviewed_head_sha: "head",
        prior_result_state: None,
        prior_result_head_sha: None,
        observation: "findings",
        effective_state: "awaiting_action",
        publication_status: "disabled",
        body: None,
        github_review_id: None,
        github_review_url: None,
        reviewer_version: "fixture",
        skill_version: "fixture",
    })
    .unwrap();
    db.conn.execute("INSERT INTO review_runs (pr_id,head_sha,status,review_body,created_at,updated_at) VALUES ($1,'newer','preview','Unrelated newer summary',2,2)", [pr]).unwrap();
    assert_eq!(db.latest_unresolved_review(pr).unwrap().unwrap(), fallback);
    db.conn
        .execute("UPDATE review_runs SET comments_json='invalid' WHERE id=$1", [run])
        .unwrap();
    assert!(db.latest_unresolved_review(pr).is_err());
}

#[test]
fn no_change_review_restores_superseded_awaiting_action_run() {
    let db = Db::for_test().open().expect("open db");
    db.conn.execute("INSERT INTO repos (owner, name, full_name, is_private, created_at, updated_at) VALUES ('acme','api','acme/api',1,1,1)", []).expect("repo");
    let repo_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO pull_requests (repo_id, number, title, author, head_sha, review_lane, last_synced_at, created_at, updated_at) VALUES ($1, 1, 'PR', 'a', 'newhead', 'open', 1, 1, 1)", params![repo_id]).expect("pr");
    let pr_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO review_runs (pr_id, head_sha, status, review_body, created_at, updated_at, model) VALUES ($1, 'oldhead', 'preview', 'finding', 1, 1, 'gpt')", params![pr_id]).expect("prior run");
    let prior_run_id = db.conn.last_insert_rowid();
    db.supersede_review_runs(pr_id).expect("supersede prior preview");
    db.conn.execute("INSERT INTO review_runs (pr_id, head_sha, status, review_body, created_at, updated_at, model) VALUES ($1, 'newhead', 'preview', 'no new changes', 2, 2, 'gpt')", params![pr_id]).expect("no-change run");
    let no_change_run_id = db.conn.last_insert_rowid();

    db.preserve_awaiting_action_after_no_changes(no_change_run_id, pr_id, prior_run_id)
        .expect("restore prior findings");

    let prior: (String, String, Option<String>) = db
        .conn
        .query_row(
            "SELECT status, head_sha, reviewed_head_sha FROM review_runs WHERE id = $1",
            params![prior_run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("prior run state");
    assert_eq!(prior.0, "preview");
    assert_eq!(prior.1, "newhead");
    assert_eq!(prior.2.as_deref(), Some("oldhead"));
    let current_status: String = db
        .conn
        .query_row(
            "SELECT status FROM review_runs WHERE id = $1",
            params![no_change_run_id],
            |row| row.get(0),
        )
        .expect("no-change run state");
    assert_eq!(current_status, "superseded");
    let lane: String = db
        .conn
        .query_row(
            "SELECT review_lane FROM pull_requests WHERE id = $1",
            params![pr_id],
            |row| row.get(0),
        )
        .expect("pr lane");
    assert_eq!(lane, "ready");
    assert!(db
        .has_pending_action("review_ready", pr_id)
        .expect("restored findings are actionable"));
    db.reserve_review_result(&ReviewResultInput {
        pr_id,
        run_id: no_change_run_id,
        actionable_run_id: Some(prior_run_id),
        observed_head_sha: "newhead",
        reviewed_head_sha: "oldhead",
        prior_result_state: Some("awaiting_action"),
        prior_result_head_sha: Some("oldhead"),
        observation: "no_new_changes",
        effective_state: "awaiting_action",
        publication_status: "disabled",
        body: None,
        github_review_id: None,
        github_review_url: None,
        reviewer_version: "v1",
        skill_version: "v1",
    })
    .expect("store effective result");
    let unresolved = db
        .latest_unresolved_review(pr_id)
        .expect("unresolved query")
        .expect("restored finding body");
    assert_eq!(unresolved, ("newhead".to_string(), "finding".to_string()));

    db.supersede_review_runs(pr_id).expect("supersede restored findings");
    db.conn.execute("INSERT INTO review_runs (pr_id, head_sha, status, review_body, created_at, updated_at, model) VALUES ($1, 'latesthead', 'preview', 'no new changes again', 3, 3, 'gpt')", params![pr_id]).expect("second no-change run");
    let second_no_change_run_id = db.conn.last_insert_rowid();

    db.preserve_awaiting_action_after_no_changes(second_no_change_run_id, pr_id, prior_run_id)
        .expect("restore the original findings again");

    let restored: (String, String, String) = db
        .conn
        .query_row(
            "SELECT status, head_sha, review_body FROM review_runs WHERE id = $1",
            params![prior_run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("restored finding state");
    assert_eq!(
        restored,
        ("preview".to_string(), "latesthead".to_string(), "finding".to_string())
    );
    let first_no_change_status: String = db
        .conn
        .query_row(
            "SELECT status FROM review_runs WHERE id = $1",
            params![no_change_run_id],
            |row| row.get(0),
        )
        .expect("first no-change run state");
    assert_eq!(first_no_change_status, "superseded");
}

#[test]
fn result_finalization_requires_the_current_pr_head() {
    let db = Db::for_test().open().expect("open db");
    db.conn.execute("INSERT INTO repos (owner, name, full_name, is_private, created_at, updated_at) VALUES ('acme','api','acme/api',1,1,1)", []).expect("repo");
    let repo_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO pull_requests (repo_id, number, title, author, head_sha, review_lane, last_synced_at, created_at, updated_at) VALUES ($1, 1, 'PR', 'a', 'newhead', 'open', 1, 1, 1)", params![repo_id]).expect("pr");
    let pr_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO review_runs (pr_id, head_sha, status, created_at, updated_at, model) VALUES ($1, 'oldhead', 'preview', 1, 1, 'gpt')", params![pr_id]).expect("run");
    let run_id = db.conn.last_insert_rowid();

    assert!(!db
        .finalize_review_decision_if_current_head(
            run_id,
            pr_id,
            "clean_result",
            "approved",
            "reviewed_cleanly",
            "Reviewed cleanly — result posted",
            None,
            "oldhead",
            Some("auto"),
        )
        .expect("stale result rejected"));
    let state: (String, String) = db
        .conn
        .query_row(
            "SELECT pr.review_lane, rr.status FROM pull_requests pr JOIN review_runs rr ON rr.pr_id = pr.id WHERE pr.id = $1",
            params![pr_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("unchanged state");
    assert_eq!(state, ("open".to_string(), "preview".to_string()));

    assert!(!db
        .finalize_review_decision_or_supersede_if_stale(
            run_id,
            pr_id,
            "comment",
            "posted",
            "auto_posted",
            "Commented",
            Some("COMMENT"),
            "oldhead",
            Some("auto"),
            "Review superseded by a new PR head",
        )
        .expect("stale review superseded"));
    let state: (String, String) = db
        .conn
        .query_row(
            "SELECT pr.review_lane, rr.status FROM pull_requests pr JOIN review_runs rr ON rr.pr_id = pr.id WHERE pr.id = $1",
            params![pr_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("new PR state preserved");
    assert_eq!(state, ("open".to_string(), "superseded".to_string()));
}

#[test]
fn base_only_scope_changes_invalidate_decisions_and_allow_a_new_scope_to_post() {
    let db = Db::for_test().open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/repo").unwrap();
    db.conn.execute("INSERT INTO pull_requests (repo_id,number,title,author,head_sha,base_ref,base_sha,last_decision_sha,review_lane,last_synced_at,created_at,updated_at) VALUES (1,1,'PR','author','head','parent','parent-sha','head','posted',1,1,1)", []).unwrap();
    let pr_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO review_runs (pr_id,head_sha,status,review_body,stacked_base_ref,reviewed_base_sha,created_at,updated_at) VALUES ($1,'head','posted','old finding','parent','parent-sha',1,1)", params![pr_id]).unwrap();
    let old = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO review_runs (pr_id,head_sha,status,review_body,stacked_base_ref,reviewed_base_sha,created_at,updated_at) VALUES ($1,'head','preview','pending finding','parent','parent-sha',2,2)", params![pr_id]).unwrap();
    let pending = db.conn.last_insert_rowid();
    db.invalidate_review_scope(pr_id, None, "Retargeted").unwrap();
    assert!(db.pr_review_start_state(pr_id).unwrap().2.is_none());
    assert!(!db.take_force_full_next_review(pr_id).unwrap());
    assert_eq!(db.review_run_by_id(pending).unwrap().unwrap().status, "superseded");
    let scope = crate::review::scope::ReviewScope {
        head_sha: "head".into(),
        base_ref: "main".into(),
        base_sha: "main-sha".into(),
    };
    assert!(db.refresh_review_base(pr_id, &scope).unwrap());
    assert!(!db
        .finalize_review_decision_if_current_head(
            old,
            pr_id,
            "approve",
            "approved",
            "approved",
            "old scope",
            None,
            "head",
            Some("you")
        )
        .unwrap());
    db.conn.execute("INSERT INTO review_runs (pr_id,head_sha,status,review_body,stacked_base_ref,reviewed_base_sha,created_at,updated_at,review_scope_revision) SELECT $1,'head','preview','new finding','main','main-sha',3,3,review_scope_revision FROM pull_requests WHERE id=$1", params![pr_id]).unwrap();
    let new = db.conn.last_insert_rowid();
    assert!(db
        .try_begin_review_post(new, pr_id, "head", "COMMENT", "comment", "you", None)
        .unwrap());
    db.invalidate_review_scope(pr_id, None, "Changed during posting")
        .unwrap();
    assert!(!db
        .finalize_review_decision_if_current_head(
            new,
            pr_id,
            "comment",
            "posted",
            "posted",
            "old scope",
            Some("COMMENT"),
            "head",
            Some("you")
        )
        .unwrap());
    assert!(db.pr_review_start_state(pr_id).unwrap().2.is_none());
    assert_eq!(
        db.review_run_by_id(old).unwrap().unwrap().review_body.as_deref(),
        Some("old finding")
    );
}

#[test]
fn base_sync_scope_revision_blocks_old_publication_without_forcing_full() {
    let db = Db::for_test().open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/repo").unwrap();
    db.conn.execute("INSERT INTO pull_requests (repo_id,number,title,author,head_sha,base_ref,base_sha,last_synced_at,created_at,updated_at) VALUES (1,1,'PR','author','head','main','base',1,1,1)", []).unwrap();
    let pr_id = db.conn.last_insert_rowid();
    let old = db.start_review_skeleton_with(pr_id, None).unwrap();
    db.conn.execute("UPDATE review_runs SET status='posting',review_body='finding',stacked_base_ref='main',reviewed_base_sha='base',human_selection_required=0 WHERE id=$1", params![old]).unwrap();
    db.invalidate_review_scope(pr_id, None, "Same ref, changed base snapshot")
        .unwrap();
    assert!(!db.take_force_full_next_review(pr_id).unwrap());
    assert!(!db
        .finalize_review_decision_if_current_head(
            old,
            pr_id,
            "comment",
            "posted",
            "posted",
            "old scope",
            Some("COMMENT"),
            "head",
            Some("you")
        )
        .unwrap());
    db.conn
        .execute("UPDATE review_runs SET status='preview' WHERE id=$1", params![old])
        .unwrap();
    assert!(!db
        .try_begin_review_post(old, pr_id, "head", "COMMENT", "comment", "you", None)
        .unwrap());
    let fresh = db.start_review_skeleton_with(pr_id, None).unwrap();
    db.invalidate_review_scope(pr_id, Some(fresh), "Fresh preparation")
        .unwrap();
    db.conn
        .execute(
            "UPDATE review_runs SET status='preview',human_selection_required=0,stacked_base_ref='main' WHERE id=$1",
            params![fresh],
        )
        .unwrap();
    assert!(db
        .try_begin_review_post(fresh, pr_id, "head", "COMMENT", "comment", "you", None)
        .unwrap());
    db.set_force_full_next_review(pr_id, true).unwrap();
    db.invalidate_review_scope(pr_id, None, "Preserve explicit full request")
        .unwrap();
    assert!(db.take_force_full_next_review(pr_id).unwrap());
}

#[test]
fn base_sync_coverage_snapshot_excludes_incomplete_failed_and_foreign_runs() {
    let db = Db::for_test().open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/repo").unwrap();
    db.conn.execute("INSERT INTO pull_requests (repo_id,number,title,author,head_sha,last_synced_at,created_at,updated_at) VALUES (1,1,'PR','author','head',1,1,1)", []).unwrap();
    let pr_id = db.conn.last_insert_rowid();
    let run_id = db.start_review_skeleton_with(pr_id, None).unwrap();
    db.begin_review_coverage(&ReviewCoverageInput {
        repo_id: 1,
        pr_id,
        run_id,
        base_ref: "main",
        base_sha: "base",
        merge_base_sha: "base",
        head_sha: "head",
        changed_files: &["file.rs".into()],
        covered_patch_ids: &["patch".into()],
        diff_hash: "hash",
        diff_text: "diff",
        source_json: None,
    })
    .unwrap();
    db.finalize_review_coverage(run_id).unwrap();
    assert!(db.review_coverage_snapshot(pr_id, "head").unwrap().is_none());
    db.set_review_run_running(run_id, "[]").unwrap();
    assert!(db.finish_review_run_only(run_id, "Reviewed", None, "[]").unwrap());
    assert_eq!(
        db.review_coverage_snapshot(pr_id, "head").unwrap().unwrap().run_id,
        run_id
    );
    assert!(db.review_coverage_snapshot(pr_id + 1, "head").unwrap().is_none());
    assert!(db.review_coverage_snapshot(pr_id, "other").unwrap().is_none());
    db.conn
        .execute("UPDATE review_runs SET status='failed' WHERE id=$1", params![run_id])
        .unwrap();
    assert!(db.review_coverage_snapshot(pr_id, "head").unwrap().is_none());
}

#[test]
fn chunk_snapshots_survive_phase_changes_and_new_snapshots_replace_old_ones() {
    let queued = r#"[{"id":"chunks","status":"running","chunks":[{"id":1,"status":"queued"}]},{"id":"final","status":"pending"}]"#;
    let first = merge_review_phase_progress(None, queued, "Queued", 10);
    let cached = queued.replace("\"status\":\"queued\"", "\"status\":\"cached\"");
    let updated = merge_review_phase_progress(Some(&first), &cached, "Cached", 20);
    let next = merge_review_phase_progress(
        Some(&updated),
        r#"[{"id":"chunks","status":"ok"},{"id":"final","status":"queued"}]"#,
        "Waiting for a slot",
        30,
    );
    let parsed: serde_json::Value = serde_json::from_str(&next).unwrap();
    assert_eq!(parsed[0]["chunks"][0]["status"], "cached");
    assert_eq!(parsed[1]["queuedAt"], 30);
    assert!(parsed[1].get("startedAt").is_none());
}

#[test]
fn upsert_pull_request_clears_stacked_status_on_retarget_to_default_branch() {
    let db = Db::for_test().open().expect("open db");
    db.upsert_workspaces(&[WorkspaceInput {
        login: "acme".to_string(),
        kind: "org".to_string(),
    }])
    .expect("workspace");
    let repo = db
        .upsert_repos(&[RepoInput {
            workspace_login: "acme".to_string(),
            owner: "acme".to_string(),
            name: "web".to_string(),
            full_name: "acme/web".to_string(),
            description: None,
            is_private: true,
            default_branch: Some("main".to_string()),
        }])
        .expect("repo")
        .remove(0);
    let mut input = PullRequestInput {
        repo_id: repo.id,
        number: 7665,
        title: "Show unified SOP Library names".to_string(),
        author: "alice".to_string(),
        author_avatar_url: None,
        is_draft: false,
        head_sha: "head".to_string(),
        base_sha: Some("base".to_string()),
        base_ref: Some("sprk-7638-skills-direct-training-launch".to_string()),
        head_ref: Some("sprk-7665-sop-library-unified-display".to_string()),
        default_branch: Some("main".to_string()),
        url: Some("https://github.com/acme/web/pull/7665".to_string()),
        lane: "manual_required".to_string(),
        status_reason: Some("Stacked PR: reviewing `head` → `base` with immediate base context".to_string()),
    };
    db.upsert_pull_request(&input).expect("insert stacked");
    input.base_ref = Some("main".to_string());
    input.status_reason = Some("Manual AI review configured".to_string());
    db.upsert_pull_request(&input).expect("retarget main");
    let (base_ref, is_stacked, status_reason): (Option<String>, i64, Option<String>) = db
        .conn
        .query_row(
            "SELECT base_ref, is_stacked, status_reason FROM pull_requests WHERE repo_id = $1 AND number = $2",
            params![repo.id, 7665_i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query pr");
    assert_eq!(base_ref.as_deref(), Some("main"));
    assert_eq!(is_stacked, 0);
    assert_eq!(status_reason.as_deref(), Some("Manual AI review configured"));
}

#[test]
fn poll_upsert_preserves_manual_cancellation_for_the_same_head() {
    let db = Db::for_test().open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/api").unwrap();
    let mut input = PullRequestInput {
        repo_id: 1,
        number: 1,
        title: "Fixture".into(),
        author: "author".into(),
        author_avatar_url: None,
        is_draft: false,
        head_sha: "head".into(),
        base_sha: Some("base".into()),
        base_ref: Some("main".into()),
        head_ref: Some("feature".into()),
        default_branch: Some("main".into()),
        url: None,
        lane: "manual_required".into(),
        status_reason: Some("Review cancelled".into()),
    };
    let pr_id = db.upsert_pull_request(&input).unwrap();
    input.lane = "open".into();
    input.status_reason = None;
    db.upsert_pull_request(&input).unwrap();
    let state: (String, Option<String>) = db
        .conn
        .query_row(
            "SELECT review_lane, status_reason FROM pull_requests WHERE id = $1",
            params![pr_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(state.0, "manual_required");
    assert_eq!(state.1.as_deref(), Some("Review cancelled"));
    input.head_sha = "new-head".into();
    db.upsert_pull_request(&input).unwrap();
    let reason: Option<String> = db
        .conn
        .query_row(
            "SELECT status_reason FROM pull_requests WHERE id = $1",
            params![pr_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(reason, None);
}

#[test]
fn lifecycle_candidates_paginate_past_two_hundred_open_rows() {
    let db = Db::for_test().open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/api").unwrap();
    for number in 1..=201_i64 {
        db.conn.execute(
            "INSERT INTO pull_requests (repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at)
             VALUES (1, $1, 'PR', 'author', $2, $3, $3, $3)",
            params![number, format!("head-{number}"), number],
        ).unwrap();
    }

    let first = db.lifecycle_candidates(1, None, 200).unwrap();
    assert_eq!(first.len(), 200);
    assert_eq!(first.last().unwrap().1, 200);
    let cursor = first.last().map(|candidate| (candidate.4, candidate.0));
    let second = db.lifecycle_candidates(1, cursor, 200).unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].1, 201);
}

#[test]
fn pending_review_coverage_does_not_block_historical_backfill() {
    let db = Db::for_test().open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/api").unwrap();
    let pr_id = db
        .upsert_pull_request(&PullRequestInput {
            repo_id: 1,
            number: 1,
            title: "PR".into(),
            author: "author".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: Some("base".into()),
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "open".into(),
            status_reason: None,
        })
        .unwrap();
    let run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    let empty: Vec<String> = Vec::new();
    db.begin_review_coverage(&ReviewCoverageInput {
        repo_id: 1,
        pr_id,
        run_id,
        base_ref: "main",
        base_sha: "base",
        merge_base_sha: "base",
        head_sha: "head",
        changed_files: &empty,
        covered_patch_ids: &empty,
        diff_hash: "diff",
        diff_text: "",
        source_json: None,
    })
    .unwrap();
    assert!(!db.review_coverage_exists(run_id).unwrap());
    db.finalize_review_coverage(run_id).unwrap();
    assert!(db.review_coverage_exists(run_id).unwrap());
}

#[test]
fn obsolete_model_routing_is_ignored_and_not_exposed() {
    let db = Db::for_test().open().unwrap();
    db.conn
        .execute(
            "INSERT INTO settings (key,value,updated_at) VALUES ('review_routing','invalid obsolete config',1)",
            [],
        )
        .unwrap();
    let settings = db.settings().expect("obsolete routing must not break settings");
    assert!(serde_json::to_value(&settings).unwrap().get("reviewRouting").is_none());
    assert!(db.save_setting("review_routing", "{}").is_err());
    assert!(db.save_setting("review_model", "claude-sonnet-5-5-high").is_err());
}

#[test]
fn review_stage_breakdown_tracks_cache_candidates() {
    let db = Db::for_test().open().expect("open db");
    db.conn.execute("INSERT INTO repos (owner, name, full_name, is_private, created_at, updated_at) VALUES ('acme','api','acme/api',1,1,1)", []).expect("repo");
    let repo_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO pull_requests (repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at) VALUES ($1, 1, 'PR', 'a', 'abc', 1, 1, 1)", params![repo_id]).expect("pr");
    let pr_id = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO review_runs (pr_id, head_sha, status, created_at, updated_at, started_at, model) VALUES ($1, 'abc', 'posted', 1, 1, 1, 'gpt-5-5-medium')", params![pr_id]).expect("run");
    let run_id = db.conn.last_insert_rowid();
    let stage = ReviewRunStageInput {
        run_id,
        pr_id,
        stage: "chunk".to_string(),
        chunk_id: Some(1),
        model: Some("gpt-5-5-medium".to_string()),
        status: "done".to_string(),
        cache_key: Some("k".to_string()),
        cache_hit: false,
        potential_cache_hit: true,
        potential_savings_tokens: 12,
        input_tokens: 10,
        output_tokens: 2,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        prompt_bytes: 100,
        diff_bytes: 50,
        context_bytes: 200,
        file_count: 2,
        related_file_count: 3,
        started_at: 1,
        finished_at: Some(2),
    };
    db.insert_review_run_stage(&stage).expect("stage");
    assert!(!db.review_chunk_cache_exists("k").expect("exists before"));
    db.upsert_review_chunk_cache(&ReviewChunkCacheInput {
        cache_key: "k".to_string(),
        repo_id,
        pr_id,
        run_id,
        chunk_id: 1,
        head_sha: "abc".to_string(),
        prior_sha: None,
        model: "gpt-5-5-medium".to_string(),
        prompt_version: "review-v1".to_string(),
        skill_hash: "s".to_string(),
        index_spec_version: INDEX_SPEC_VERSION,
        chunk_json: "{}".to_string(),
        summary: "ok".to_string(),
        comments_json: None,
        usage_json: "{}".to_string(),
        prompt_bytes: 100,
        diff_bytes: 50,
        context_bytes: 200,
        now: 2,
    })
    .expect("cache");
    assert!(db.review_chunk_cache_exists("k").expect("exists after"));
    assert!(db.cached_review_chunk("k").unwrap().is_none());
    db.conn
        .execute(
            "UPDATE review_chunk_cache SET comments_json = '{}', last_seen_at = $1 WHERE cache_key = 'k'",
            params![now_millis()],
        )
        .unwrap();
    assert_eq!(db.cached_review_chunk("k").unwrap(), Some(("ok".into(), "{}".into())));
    db.conn
        .execute(
            "UPDATE review_chunk_cache SET last_seen_at = 1 WHERE cache_key = 'k'",
            [],
        )
        .unwrap();
    assert!(db.cached_review_chunk("k").unwrap().is_none());
    let breakdown = db.review_stage_breakdown(run_id).expect("breakdown");
    assert_eq!(breakdown.stages.len(), 1);
    assert_eq!(breakdown.potential_cache_hits, 1);
    assert_eq!(breakdown.potential_cache_savings_tokens, 12);
    db.insert_review_run_stage(&ReviewRunStageInput {
        stage: "final".to_string(),
        status: "failed".to_string(),
        chunk_id: None,
        output_tokens: 3,
        potential_cache_hit: false,
        potential_savings_tokens: 0,
        ..stage
    })
    .unwrap();
    let failed = db.review_stage_breakdown(run_id).unwrap();
    db.sync_review_stage_usage(run_id).unwrap();
    let recorded = db.review_run_by_id(run_id).unwrap().unwrap();
    assert_eq!((recorded.input_tokens, recorded.output_tokens), (20, 5));
    assert_eq!(failed.total_tokens, 25);
    assert!(failed
        .stages
        .iter()
        .any(|stage| stage.status == "failed" && stage.output_tokens == 3));
}

#[test]
fn skill_versions_snapshot_and_rollback() {
    let db = Db::for_test().open().expect("open db");
    // v0 baseline = the built-in embedded skill; nothing stored yet.
    assert_eq!(db.active_skill_version_id(), 0);
    let baseline = db.active_skill_content();
    assert!(baseline.contains("Universal Review Skill"));

    // Apply → immutable snapshot, becomes active, never overwrites the baseline.
    let v1 = db
        .create_skill_version("SKILL TEXT v1", Some("from run 7"))
        .expect("v1");
    assert_eq!(v1.version, 1);
    assert!(v1.active);
    assert_eq!(db.active_skill_version_id(), v1.id);
    assert_eq!(db.active_skill_content(), "SKILL TEXT v1");

    let list = db.list_skill_versions().expect("list");
    assert_eq!(list.len(), 2, "default built-in + v1");
    assert!(
        list.iter().any(|v| v.id == 0 && !v.active),
        "default baseline present, not active"
    );
    let stored = list.iter().find(|v| v.id == v1.id).expect("v1 row");
    assert!(stored.active);

    // Rollback to the default baseline (v0) restores the built-in skill without deleting v1.
    db.set_active_skill_version(0).expect("rollback");
    assert_eq!(db.active_skill_content(), baseline);
    assert_eq!(
        db.list_skill_versions().expect("list2").len(),
        2,
        "v1 still kept alongside the built-in"
    );

    // An install that had the removed generic built-in (-1) active reviews with the default.
    db.save_setting_value("active_skill_version", "-1")
        .expect("old generic pointer");
    assert_eq!(db.active_skill_content(), baseline);
    let list3 = db.list_skill_versions().expect("list3");
    assert_eq!(list3.len(), 2, "no generic built-in listed");
    assert!(
        list3.iter().any(|v| v.id == 0 && v.active),
        "default built-in active in place of the removed generic"
    );
    db.set_active_skill_version(-1).expect("negative id");
    assert_eq!(db.active_skill_version_id(), 0);
}

#[test]
fn settings_have_defaults() {
    let db = Db::for_test().open().expect("open db");
    let settings = db.settings().expect("settings");
    assert_eq!(settings.theme, "system");
    assert_eq!(settings.font_size, 14);
    assert!(!settings.sidebar_collapsed);
    assert!(!settings.mission_workspace_configured);
    assert_eq!(settings.poll_interval_seconds, DEFAULT_POLL_INTERVAL_SECONDS);
    assert_eq!(settings.auto_skip_keywords, ["bump", "deps", "deps-dev"]);
    assert!(!settings.review_assigned_only);
    assert!(settings.review_auto_enabled);
    assert_eq!(settings.review_concurrency, DEFAULT_REVIEW_CONCURRENCY);
    assert_eq!(settings.review_max_files, 100);
    assert_eq!(settings.auto_post_mode, "comment");
    assert_eq!(settings.review_result_mode, "result_comment");
    assert!(settings.pr_summary_comment_enabled);
    assert!(settings.dismiss_stale_approvals);
    assert_eq!(settings.index_strategy, "graph");
    assert!(!settings.review_suggestions_enabled);
}

#[test]
fn review_suggestions_setting_is_off_by_default_and_round_trips() {
    let db = Db::for_test().open().expect("open db");
    assert!(!db.settings().unwrap().review_suggestions_enabled);
    assert!(
        db.save_setting("review_suggestions_enabled", "true")
            .unwrap()
            .review_suggestions_enabled
    );
    assert!(db.settings().unwrap().review_suggestions_enabled);
    // A rejected value leaves the stored one as it was.
    assert!(db.save_setting("review_suggestions_enabled", "yes").is_err());
    assert!(db.settings().unwrap().review_suggestions_enabled);
    let update: SettingsUpdate =
        serde_json::from_value(serde_json::json!({"settings": {"review_suggestions_enabled": "false"}})).unwrap();
    assert!(
        !db.save_settings_batch(&update)
            .unwrap()
            .settings
            .review_suggestions_enabled
    );
    assert!(!db.settings().unwrap().review_suggestions_enabled);
}

#[test]
fn review_max_files_setting_is_persisted_and_validated() {
    let db = Db::for_test().open().expect("open db");
    assert_eq!(
        db.save_setting("review_max_files", "250").unwrap().review_max_files,
        250
    );
    for value in ["0", "-1", "1.5", ""] {
        assert!(
            db.save_setting("review_max_files", value).is_err(),
            "accepted {value:?}"
        );
    }
    assert_eq!(db.settings().unwrap().review_max_files, 250);
}

#[test]
fn review_archive_retention_days_setting_is_persisted_and_validated() {
    let db = Db::for_test().open().expect("open db");
    assert_eq!(db.settings().unwrap().review_archive_retention_days, 0);
    assert_eq!(
        db.save_setting("review_archive_retention_days", "30")
            .unwrap()
            .review_archive_retention_days,
        30
    );
    assert_eq!(
        db.save_setting("review_archive_retention_days", "365")
            .unwrap()
            .review_archive_retention_days,
        365
    );
    assert_eq!(
        db.save_setting("review_archive_retention_days", "1")
            .unwrap()
            .review_archive_retention_days,
        1
    );
    assert_eq!(
        db.save_setting("review_archive_retention_days", "0")
            .unwrap()
            .review_archive_retention_days,
        0
    );
    for value in ["-1", "366", "1.5", "", "abc"] {
        assert!(
            db.save_setting("review_archive_retention_days", value).is_err(),
            "accepted {value:?}"
        );
    }
    assert_eq!(db.settings().unwrap().review_archive_retention_days, 0);

    // Test save_settings_batch validation
    let update = SettingsUpdate {
        settings: HashMap::from([("review_archive_retention_days".into(), "14".into())]),
        repositories: None,
    };
    let snapshot = db.save_settings_batch(&update).expect("batch save");
    assert_eq!(snapshot.settings.review_archive_retention_days, 14);

    let invalid_update = SettingsUpdate {
        settings: HashMap::from([("review_archive_retention_days".into(), "-1".into())]),
        repositories: None,
    };
    assert!(db.save_settings_batch(&invalid_update).is_err());
}

#[test]
fn index_strategy_setting_is_validated() {
    let db = Db::for_test().open().expect("open db");
    let settings = db.save_setting("index_strategy", "graph").expect("save graph");
    assert_eq!(settings.index_strategy, "graph");
    let error = db
        .save_setting("index_strategy", "other")
        .expect_err("invalid strategy rejected");
    assert!(error.contains("graph"));
    assert_eq!(db.settings().expect("settings").index_strategy, "graph");
}

#[test]
fn save_setting_persists_across_reopen() {
    let handle = Db::for_test();
    let db = handle.open().expect("open db");
    let settings = db.save_setting("theme", "dark").expect("save theme");
    assert_eq!(settings.theme, "dark");
    drop(db);
    let db = handle.open().expect("reopen db");
    assert_eq!(db.settings().expect("settings").theme, "dark");
}

#[test]
fn save_setting_rejects_unknown_key() {
    let db = Db::for_test().open().expect("open db");
    let err = db.save_setting("poll_running", "true").unwrap_err();
    assert!(err.contains("unknown setting key"), "got: {err}");
}

#[test]
fn save_setting_rejects_invalid_auto_post_mode() {
    let db = Db::for_test().open().expect("open db");
    let err = db.save_setting("auto_post_mode", "invalid").unwrap_err();
    assert!(err.contains("must be off, comment, or request_changes"), "got: {err}");
    let settings = db.save_setting("auto_post_mode", "comment").expect("valid mode");
    assert_eq!(settings.auto_post_mode, "comment");
}

#[test]
fn review_result_mode_is_validated_and_installation_id_is_stable() {
    let handle = Db::for_test();
    let db = handle.open().expect("open db");
    let installation_id = db.installation_id().expect("installation id");
    assert!(!installation_id.is_empty());
    let error = db
        .save_setting("review_result_mode", "always")
        .expect_err("invalid result mode rejected");
    assert!(error.contains("silent or result_comment"));
    let settings = db
        .save_setting("review_result_mode", "result_comment")
        .expect("enable result comments");
    assert_eq!(settings.review_result_mode, "result_comment");
    drop(db);
    let reopened = handle.open().expect("reopen db");
    assert_eq!(
        reopened.installation_id().expect("reopened installation id"),
        installation_id
    );
}

#[test]
fn failed_result_preserves_a_finalized_review_decision() {
    let db = Db::for_test().open().expect("open db");
    db.conn
        .execute(
            "INSERT INTO repos (owner, name, full_name, is_private, created_at, updated_at)
             VALUES ('acme', 'api', 'acme/api', 1, 1, 1)",
            [],
        )
        .expect("repo");
    let repo_id = db.conn.last_insert_rowid();
    db.conn
        .execute(
            "INSERT INTO pull_requests (
               repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at
             ) VALUES ($1, 7, 'PR', 'alice', 'head-one', 1, 1, 1)",
            params![repo_id],
        )
        .expect("pr");
    let pr_id = db.conn.last_insert_rowid();
    db.conn
        .execute(
            "INSERT INTO review_runs (pr_id, head_sha, status, created_at, updated_at, model) VALUES ($1, 'head-one', 'posting', 1, 1, 'gpt-5.5')",
            params![pr_id],
        )
        .expect("run");
    let run_id = db.conn.last_insert_rowid();
    db.finalize_review_decision(
        run_id,
        pr_id,
        "approve_post",
        "posted",
        "posted",
        "Approved",
        Some("APPROVE"),
        "head-one",
        Some("you"),
    )
    .expect("finalize decision");
    let result = db
        .reserve_review_result(&ReviewResultInput {
            pr_id,
            run_id,
            actionable_run_id: None,
            observed_head_sha: "head-one",
            reviewed_head_sha: "head-one",
            prior_result_state: None,
            prior_result_head_sha: None,
            observation: "clean",
            effective_state: "clean",
            publication_status: "pending",
            body: Some("No issues found"),
            github_review_id: Some(42),
            github_review_url: None,
            reviewer_version: "0.1.0",
            skill_version: "v0",
        })
        .expect("reserve result");

    assert!(!db
        .fail_review_result_publication(result.id, "summary comment failed")
        .expect("record result failure"));
    let state: (String, Option<String>) = db
        .conn
        .query_row(
            "SELECT status, decision FROM review_runs WHERE id = $1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("review state");
    assert_eq!(state, ("posted".to_string(), Some("approve_post".to_string())));
}

#[test]
fn review_result_reservation_is_unique_and_preserves_comment_identity() {
    let db = Db::for_test().open().expect("open db");
    db.save_setting("review_result_mode", "silent").unwrap();
    db.conn
        .execute(
            "INSERT INTO repos (owner, name, full_name, is_private, created_at, updated_at)
         VALUES ('acme', 'api', 'acme/api', 1, 1, 1)",
            [],
        )
        .expect("repo");
    let repo_id = db.conn.last_insert_rowid();
    db.conn
        .execute(
            "INSERT INTO pull_requests (
           repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at
         ) VALUES ($1, 7, 'PR', 'alice', 'head-one', 1, 1, 1)",
            params![repo_id],
        )
        .expect("pr");
    let pr_id = db.conn.last_insert_rowid();
    db.conn
        .execute(
            "INSERT INTO review_runs (pr_id, head_sha, status, created_at, updated_at, model) VALUES ($1, 'head-one', 'preview', 1, 1, 'gpt-5.5')",
            params![pr_id],
        )
        .expect("run");
    let run_id = db.conn.last_insert_rowid();
    let first = db
        .reserve_review_result(&ReviewResultInput {
            pr_id,
            run_id,
            actionable_run_id: Some(run_id),
            observed_head_sha: "head-one",
            reviewed_head_sha: "head-one",
            prior_result_state: None,
            prior_result_head_sha: None,
            observation: "clean",
            effective_state: "clean",
            publication_status: "pending",
            body: Some("body one"),
            github_review_id: None,
            github_review_url: None,
            reviewer_version: "0.1.0",
            skill_version: "v0",
        })
        .expect("reserve");
    assert!(first.marker_key.contains("repo=acme/api pr=7"));
    assert_eq!(first.actionable_run_id, Some(run_id));
    let replacement = db
        .reserve_review_result(&ReviewResultInput {
            pr_id,
            run_id,
            actionable_run_id: None,
            observed_head_sha: "head-one",
            reviewed_head_sha: "head-one",
            prior_result_state: None,
            prior_result_head_sha: None,
            observation: "clean",
            effective_state: "clean",
            publication_status: "pending",
            body: Some("replacement body"),
            github_review_id: None,
            github_review_url: None,
            reviewer_version: "0.1.0",
            skill_version: "v0",
        })
        .expect("replace pending reservation");
    assert!(!db
        .begin_review_result_publication(first.id, &first.observed_head_sha, first.body_hash.as_deref(),)
        .expect("reject stale reservation"));
    assert!(db
        .begin_review_result_publication(
            replacement.id,
            &replacement.observed_head_sha,
            replacement.body_hash.as_deref(),
        )
        .expect("begin"));
    assert!(db
        .reserve_review_result(&ReviewResultInput {
            pr_id,
            run_id,
            actionable_run_id: None,
            observed_head_sha: "head-two",
            reviewed_head_sha: "head-two",
            prior_result_state: Some("clean"),
            prior_result_head_sha: Some("head-one"),
            observation: "clean",
            effective_state: "clean",
            publication_status: "pending",
            body: Some("body two"),
            github_review_id: None,
            github_review_url: None,
            reviewer_version: "0.1.0",
            skill_version: "v0",
        })
        .is_err());
    db.finish_review_result_publication(replacement.id, 88).expect("finish");
    let second = db
        .reserve_review_result(&ReviewResultInput {
            pr_id,
            run_id,
            actionable_run_id: None,
            observed_head_sha: "head-two",
            reviewed_head_sha: "head-two",
            prior_result_state: Some("clean"),
            prior_result_head_sha: Some("head-one"),
            observation: "clean",
            effective_state: "clean",
            publication_status: "disabled",
            body: Some("body two"),
            github_review_id: None,
            github_review_url: None,
            reviewer_version: "0.1.0",
            skill_version: "v0",
        })
        .expect("reserve next result");
    assert_eq!(second.id, first.id);
    assert_eq!(second.github_comment_id, Some(88));
    assert_eq!(second.publication_status, "disabled");

    db.upsert_pending_action(
        "review_ready",
        repo_id,
        pr_id,
        "Review ready",
        "review before publishing the result",
    )
    .expect("record ready action");
    db.fail_review_result_publication(second.id, "retry me")
        .expect("mark result failed");
    assert!(!db
        .has_pending_action("review_ready", pr_id)
        .expect("ready action resolved"));
    db.upsert_pending_action(
        "review_result_failed",
        repo_id,
        pr_id,
        "Review result failed",
        "retry me",
    )
    .expect("record failure action");
    let error = db.retry_review_result(pr_id).expect_err("silent mode rejects retries");
    assert!(error.contains("enable result comments"));
    assert_eq!(
        db.review_result(pr_id)
            .expect("result")
            .expect("stored result")
            .publication_status,
        "failed"
    );
    assert!(db
        .has_pending_action("review_result_failed", pr_id)
        .expect("failure action remains actionable"));
    let replacement = db
        .reserve_review_result(&ReviewResultInput {
            pr_id,
            run_id,
            actionable_run_id: None,
            observed_head_sha: "head-three",
            reviewed_head_sha: "head-three",
            prior_result_state: Some("clean"),
            prior_result_head_sha: Some("head-two"),
            observation: "findings",
            effective_state: "findings_posted",
            publication_status: "disabled",
            body: None,
            github_review_id: None,
            github_review_url: None,
            reviewer_version: "0.1.0",
            skill_version: "v0",
        })
        .expect("reserve replacement result");
    assert!(!db
        .has_pending_action("review_result_failed", pr_id)
        .expect("replaced failure action resolved"));
    db.fail_review_result_publication(replacement.id, "retry me again")
        .expect("record replacement failure");
    db.upsert_pending_action(
        "review_result_failed",
        repo_id,
        pr_id,
        "Review result failed",
        "retry me again",
    )
    .expect("record replacement failure action");
    db.save_setting("review_result_mode", "result_comment")
        .expect("enable result comments");
    let retried = db.retry_review_result(pr_id).expect("retry result");
    assert_eq!(retried.publication_status, "pending");
    db.supersede_review_result_after_stale_head(retried.id, pr_id, "new PR head superseded the retry")
        .expect("supersede retry");
    assert_eq!(
        db.review_result(pr_id)
            .expect("result")
            .expect("stored result")
            .publication_status,
        "superseded"
    );
    assert!(!db
        .has_pending_action("review_result_failed", pr_id)
        .expect("failure action resolved"));
}

#[test]
fn failed_result_does_not_hide_a_new_review_failure() {
    let db = Db::for_test().open().expect("open db");
    db.conn
        .execute(
            "INSERT INTO repos (owner, name, full_name, is_private, created_at, updated_at)
             VALUES ('acme', 'api', 'acme/api', 1, 1, 1)",
            [],
        )
        .expect("repo");
    let repo_id = db.conn.last_insert_rowid();
    db.conn
        .execute(
            "INSERT INTO pull_requests (repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at)
             VALUES ($1, 7, 'PR', 'alice', 'new-head', 1, 1, 1)",
            params![repo_id],
        )
        .expect("pr");
    let pr_id = db.conn.last_insert_rowid();
    let installation_id = db.installation_id().expect("installation id");
    db.conn
        .execute(
            "INSERT INTO review_results (
               pr_id, installation_id, marker_key, observed_head_sha, reviewed_head_sha,
               observation, effective_state, publication_status, reviewer_version, skill_version,
               created_at, updated_at
             ) VALUES ($1, $2, 'marker', 'new-head', 'new-head', 'clean', 'clean', 'failed', 'v1', 'v1', 1, 1)",
            params![pr_id, installation_id],
        )
        .expect("failed result");
    db.conn
        .execute(
            "INSERT INTO review_runs (pr_id, head_sha, status, created_at, updated_at, model) VALUES ($1, 'new-head', 'running', 1, 1, 'gpt')",
            params![pr_id],
        )
        .expect("run");
    let run_id = db.conn.last_insert_rowid();

    db.fail_review_run(run_id, "review crashed")
        .expect("record current review failure");
    db.sync_review_attention_pendings().expect("sync attention pendings");

    assert!(db
        .has_pending_action("review_failed", pr_id)
        .expect("current review failure remains actionable"));
}

fn settings_batch_fixture() -> (Database, i64, i64) {
    let db = Db::for_test().open().expect("open db");
    db.upsert_workspaces(&[WorkspaceInput {
        login: "acme".into(),
        kind: "org".into(),
    }])
    .unwrap();
    db.set_selected_workspaces(&["acme".into()]).unwrap();
    let repos = db
        .upsert_repos(&["api", "web"].map(|name| RepoInput {
            workspace_login: "acme".into(),
            owner: "acme".into(),
            name: name.into(),
            full_name: format!("acme/{name}"),
            description: None,
            is_private: true,
            default_branch: Some("main".into()),
        }))
        .unwrap();
    let first = repos[0].id;
    let second = repos[1].id;
    db.save_repo_subscription(first, true, "automatic", false, false)
        .unwrap();
    (db, first, second)
}

#[test]
fn settings_batch_saves_all_sections_and_queues_new_repositories() {
    let (db, first, second) = settings_batch_fixture();
    let result = db
        .save_settings_batch(&SettingsUpdate {
            settings: HashMap::from([
                ("review_auto_enabled".into(), "true".into()),
                ("poll_interval_seconds".into(), "300".into()),
            ]),
            repositories: Some(RepositorySelection {
                logins: vec!["acme".into()],
                repo_ids: vec![first, second],
                monitor_all: vec![],
            }),
        })
        .unwrap();
    assert!(result.settings.review_auto_enabled);
    assert_eq!(result.settings.poll_interval_seconds, 300);
    assert_eq!(result.selected_repos.len(), 2);
    assert!(result.workspaces[0].selected);
    assert_eq!(db.repo_index_status(second).unwrap().status, "queued");
    let unchanged = db.repo_by_id(first).unwrap();
    assert_eq!(unchanged.review_mode, "automatic");
    assert!(!unchanged.skip_draft_prs);
    assert!(!unchanged.skip_self_prs);
    assert_eq!(db.repo_index_status(first).unwrap().status, "not_indexed");
}

#[test]
fn monitor_all_workspaces_enable_every_repository_including_new_ones() {
    let (db, first, second) = settings_batch_fixture();
    let result = db
        .save_settings_batch(&SettingsUpdate {
            settings: HashMap::new(),
            repositories: Some(RepositorySelection {
                logins: vec!["acme".into()],
                repo_ids: vec![],
                monitor_all: vec!["acme".into()],
            }),
        })
        .unwrap();
    assert!(result.workspaces[0].monitor_all);
    assert_eq!(
        result.selected_repos.iter().map(|repo| repo.id).collect::<HashSet<_>>(),
        HashSet::from([first, second])
    );
    let repos = db
        .upsert_repos(&["api", "web", "new"].map(|name| RepoInput {
            workspace_login: "acme".into(),
            owner: "acme".into(),
            name: name.into(),
            full_name: format!("acme/{name}"),
            description: None,
            is_private: true,
            default_branch: Some("main".into()),
        }))
        .unwrap();
    let added = repos.iter().find(|repo| repo.name == "new").unwrap().id;
    assert!(!db.repo_by_id(added).unwrap().subscription_enabled);
    assert_eq!(db.monitor_all_logins().unwrap(), vec!["acme".to_string()]);
    assert_eq!(db.enable_monitor_all_repos().unwrap(), 1);
    assert!(db.repo_by_id(added).unwrap().subscription_enabled);
    assert_eq!(db.repo_index_status(added).unwrap().status, "queued");
    assert_eq!(db.enable_monitor_all_repos().unwrap(), 0);
    assert!(db
        .save_settings_batch(&SettingsUpdate {
            settings: HashMap::new(),
            repositories: Some(RepositorySelection {
                logins: vec![],
                repo_ids: vec![],
                monitor_all: vec!["acme".into()]
            }),
        })
        .is_err());
    db.save_settings_batch(&SettingsUpdate {
        settings: HashMap::new(),
        repositories: Some(RepositorySelection {
            logins: vec!["acme".into()],
            repo_ids: vec![first],
            monitor_all: vec![],
        }),
    })
    .unwrap();
    assert!(db.monitor_all_logins().unwrap().is_empty());
    assert!(!db.repo_by_id(added).unwrap().subscription_enabled);
}

#[test]
fn settings_batch_rolls_back_settings_subscriptions_and_index_queue() {
    let (db, first, second) = settings_batch_fixture();
    let before = serde_json::to_value(db.settings().unwrap()).unwrap();
    let result = db.save_settings_batch(&SettingsUpdate {
        settings: HashMap::from([
            ("review_auto_enabled".into(), "true".into()),
            ("review_chunk_size".into(), "21".into()),
        ]),
        repositories: Some(RepositorySelection {
            logins: vec!["acme".into()],
            repo_ids: vec![second],
            monitor_all: vec![],
        }),
    });
    assert!(result.is_err());
    assert_eq!(serde_json::to_value(db.settings().unwrap()).unwrap(), before);
    assert!(db.repo_by_id(first).unwrap().subscription_enabled);
    assert!(!db.repo_by_id(second).unwrap().subscription_enabled);
    assert_eq!(db.repo_index_status(second).unwrap().status, "not_indexed");
    assert!(db.list_workspaces().unwrap()[0].selected);
}

#[test]
fn settings_batch_validates_repositories_and_numeric_values() {
    let (db, first, second) = settings_batch_fixture();
    for (logins, repo_ids) in [
        (vec![], vec![second]),
        (vec!["missing".into()], vec![]),
        (vec!["acme".into()], vec![i64::MAX]),
    ] {
        assert!(db
            .save_settings_batch(&SettingsUpdate {
                settings: HashMap::new(),
                repositories: Some(RepositorySelection {
                    logins,
                    repo_ids,
                    monitor_all: vec![]
                })
            })
            .is_err());
    }
    for (key, value) in [
        ("review_concurrency", ""),
        ("review_concurrency", "0"),
        ("review_chunk_size", "21"),
        ("review_parallel_chunks", "1.5"),
        ("review_model", " "),
    ] {
        assert!(db
            .save_settings_batch(&SettingsUpdate {
                settings: HashMap::from([(key.into(), value.into())]),
                repositories: None
            })
            .is_err());
    }
    assert!(db.repo_by_id(first).unwrap().subscription_enabled);
    assert!(!db.repo_by_id(second).unwrap().subscription_enabled);
}

#[test]
fn settings_batch_can_clear_access_and_setting_only_updates_preserve_it() {
    let (db, first, _) = settings_batch_fixture();
    db.save_settings_batch(&SettingsUpdate {
        settings: HashMap::from([("review_auto_enabled".into(), "true".into())]),
        repositories: None,
    })
    .unwrap();
    assert!(db.repo_by_id(first).unwrap().subscription_enabled);
    let result = db
        .save_settings_batch(&SettingsUpdate {
            settings: HashMap::new(),
            repositories: Some(RepositorySelection {
                logins: vec![],
                repo_ids: vec![],
                monitor_all: vec![],
            }),
        })
        .unwrap();
    assert!(result.selected_repos.is_empty());
    assert!(!result.workspaces[0].selected);
}

#[test]
fn workspaces_repos_subscriptions_and_skills_persist() {
    let db = Db::for_test().open().expect("open db");
    db.upsert_workspaces(&[WorkspaceInput {
        login: "acme".to_string(),
        kind: "org".to_string(),
    }])
    .expect("upsert workspaces");
    db.set_selected_workspaces(&["acme".to_string()])
        .expect("select workspace");
    let repos = db
        .upsert_repos(&[RepoInput {
            workspace_login: "acme".to_string(),
            owner: "acme".to_string(),
            name: "api".to_string(),
            full_name: "acme/api".to_string(),
            description: None,
            is_private: true,
            default_branch: Some("main".to_string()),
        }])
        .expect("upsert repos");
    db.save_repo_subscription(repos[0].id, true, "automatic", true, true)
        .expect("subscription");
    db.update_repo_skill("acme/api", "found", Some("docs/skills/review.md"), None)
        .expect("skill");
    let workspace = db.list_workspaces().expect("workspaces").remove(0);
    let repo = db.list_repos("acme").expect("repos").remove(0);
    assert!(workspace.selected);
    assert_eq!(workspace.repo_count, 1);
    assert_eq!(workspace.review_skill_repo_count, 1);
    assert!(repo.subscription_enabled);
    assert_eq!(repo.review_mode, "automatic");
    assert_eq!(repo.skill_path.as_deref(), Some("docs/skills/review.md"));
}

fn file(path: &str, package: Option<&str>) -> RepoIndexFileInput {
    RepoIndexFileInput {
        path: path.to_string(),
        language: Some("typescript".to_string()),
        size: 100,
        blob_sha: "deadbeef".to_string(),
        package_path: package.map(ToString::to_string),
        is_binary: false,
        is_generated: false,
    }
}

#[test]
fn build_review_context_plan_selects_graph_neighbours() {
    let db = Db::for_test().open().expect("open db");
    db.upsert_workspaces(&[WorkspaceInput {
        login: "acme".to_string(),
        kind: "folder".to_string(),
    }])
    .expect("ws");
    let repos = db
        .upsert_repos(&[RepoInput {
            workspace_login: "acme".to_string(),
            owner: "acme".to_string(),
            name: "api".to_string(),
            full_name: "acme/api".to_string(),
            description: None,
            is_private: false,
            default_branch: Some("main".to_string()),
        }])
        .expect("repos");
    let repo_id = repos[0].id;
    db.update_repo_skill("acme/api", "found", Some("docs/skills/review.md"), None)
        .expect("skill");
    db.begin_repo_index(repo_id, Some("main")).expect("begin");

    let write = RepoIndexWrite {
        base_ref: "origin/main".to_string(),
        base_sha: "sha1".to_string(),
        files: vec![
            file("packages/a/src/index.ts", Some("packages/a")),
            file("packages/b/src/index.ts", Some("packages/b")),
            file("packages/b/src/util.ts", Some("packages/b")),
            file("packages/b/src/index.test.ts", Some("packages/b")),
            file("packages/b/src/billing.test.ts", Some("packages/b")),
            file("AGENTS.md", None),
            file(".claude/hooks/check.local.ts", None),
        ],
        packages: vec![
            RepoIndexPackageInput {
                package_path: "packages/a".to_string(),
                name: "@scope/a".to_string(),
                kind: "lib".to_string(),
                manager: "pnpm".to_string(),
                source_roots_json: "[]".to_string(),
                test_globs_json: "[]".to_string(),
            },
            RepoIndexPackageInput {
                package_path: "packages/b".to_string(),
                name: "@scope/b".to_string(),
                kind: "lib".to_string(),
                manager: "pnpm".to_string(),
                source_roots_json: "[]".to_string(),
                test_globs_json: "[]".to_string(),
            },
        ],
        // a depends on @scope/b → a is a dependent of changed package b.
        deps: vec![RepoIndexPkgDepInput {
            from_package_path: "packages/a".to_string(),
            to_package_name: "@scope/b".to_string(),
            dev: false,
        }],
        edges: vec![
            // b/index imports util (forward neighbour of the changed file)
            RepoIndexEdgeInput {
                from_path: "packages/b/src/index.ts".to_string(),
                to_ref: "./util".to_string(),
                kind: "import".to_string(),
                resolved: true,
                to_path: Some("packages/b/src/util.ts".to_string()),
            },
            // a/index imports b/index (reverse: a imports the changed file)
            RepoIndexEdgeInput {
                from_path: "packages/a/src/index.ts".to_string(),
                to_ref: "@scope/b".to_string(),
                kind: "import".to_string(),
                resolved: true,
                to_path: Some("packages/b/src/index.ts".to_string()),
            },
            // Private agent configuration is never snapshot context, even when it imports the change.
            RepoIndexEdgeInput {
                from_path: ".claude/hooks/check.local.ts".to_string(),
                to_ref: "../../packages/b/src/index".to_string(),
                kind: "import".to_string(),
                resolved: true,
                to_path: Some("packages/b/src/index.ts".to_string()),
            },
        ],
        skipped_count: 0,
        total_count: 7,
        duration_ms: 1,
        ..RepoIndexWrite::default()
    };
    db.finish_repo_index(repo_id, &write).expect("finish");

    let plan = db
        .build_review_context_plan(repo_id, &["packages/b/src/index.ts".to_string()], 40)
        .expect("plan");

    assert!(plan.packages.iter().any(|p| p.name == "@scope/b"), "owning package");
    let related: Vec<&str> = plan.related_files.iter().map(|f| f.path.as_str()).collect();
    assert!(related.contains(&"packages/b/src/util.ts"), "forward neighbour");
    assert!(related.contains(&"packages/a/src/index.ts"), "reverse neighbour");
    assert!(
        !related.contains(&"packages/b/src/index.ts"),
        "changed file not in related"
    );
    assert!(
        !related.contains(&".claude/hooks/check.local.ts"),
        "private configuration is not related context"
    );
    assert!(
        plan.test_files.iter().any(|p| p == "packages/b/src/index.test.ts"),
        "same-package test"
    );
    assert!(
        !plan.test_files.iter().any(|p| p == "packages/b/src/billing.test.ts"),
        "tests unrelated to the change are left out"
    );
    assert!(
        plan.dependent_packages.iter().any(|p| p == "packages/a"),
        "dependent package"
    );
    assert!(
        plan.convention_files.is_empty(),
        "conventions are selected from the pinned base, not the index"
    );

    let limited = db
        .build_review_context_plan(repo_id, &["packages/b/src/index.ts".to_string()], 2)
        .expect("limited plan");
    let limited_related: Vec<&str> = limited.related_files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        limited_related,
        vec!["packages/b/src/util.ts", "packages/a/src/index.ts"],
        "scored context keeps import neighbours before tests of the change"
    );
    assert_eq!(limited.omitted_count, 1);
}

#[test]
fn review_board_uses_github_update_time_for_recency() {
    let db = Db::for_test().open().expect("open db");
    db.ensure_temp_repo_for_index(1, "acme/api").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    let pr_id = db
        .upsert_pull_request_with_github_updated_at(
            &PullRequestInput {
                repo_id: 1,
                number: 42,
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
            },
            Some(1_700_000_000_000),
        )
        .unwrap();
    let card = db
        .list_review_board()
        .unwrap()
        .into_iter()
        .flat_map(|group| group.items)
        .find(|card| card.id == pr_id)
        .expect("board card");
    assert_eq!(card.updated_at, 1_700_000_000_000);
}

#[test]
fn size_limit_lane_is_not_needs_you() {
    let db = Db::for_test().open().expect("open db");
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    let pr_id = db
        .upsert_pull_request(&PullRequestInput {
            repo_id: 1,
            number: 41,
            title: "Large fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: Some("base".into()),
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "manual_required".into(),
            status_reason: None,
        })
        .unwrap();
    db.set_pr_changed_file_count(pr_id, Some(101)).unwrap();
    db.upsert_pending_action("manual_review_needed", 1, pr_id, "Manual review needed", "review")
        .unwrap();
    db.mark_pr_size_skipped(pr_id, 100).unwrap();
    let card = db
        .list_review_board()
        .unwrap()
        .into_iter()
        .flat_map(|group| group.items)
        .find(|card| card.id == pr_id)
        .unwrap();
    assert_eq!(card.lane, "size_skipped");
    assert!(db.list_pending_actions().unwrap().is_empty());
}

#[test]
fn late_file_limit_skip_does_not_reopen_terminal_pr() {
    let db = Db::for_test().open().expect("open db");
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    let pr_id = db
        .upsert_pull_request(&PullRequestInput {
            repo_id: 1,
            number: 42,
            title: "Large fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: Some("base".into()),
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "open".into(),
            status_reason: None,
        })
        .unwrap();
    db.set_pr_changed_file_count(pr_id, Some(101)).unwrap();
    let run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    db.mark_pr_terminal(pr_id, "MERGED", "PR merged on GitHub").unwrap();
    db.skip_review_run_for_file_limit(run_id, "review file limit exceeded")
        .unwrap();

    let lane: String = db
        .conn
        .query_row(
            "SELECT review_lane FROM pull_requests WHERE id = $1",
            params![pr_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(lane, "merged");
    assert_eq!(db.review_run_by_id(run_id).unwrap().unwrap().status, "superseded");
}

#[test]
fn review_board_classifies_current_empty_audit_as_clean() {
    let db = Db::for_test().open().expect("open db");
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    let pr_id = db
        .upsert_pull_request(&PullRequestInput {
            repo_id: 1,
            number: 43,
            title: "Clean fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: Some("base".into()),
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
            original_body: "Nada a sinalizar".into(),
            diff: None,
            candidates: vec![],
        },
    )
    .unwrap();
    db.finish_review_run_only(run_id, "Nada a sinalizar", Some("{\"comments\":[]}"), "[]")
        .unwrap();
    db.set_pr_lane(pr_id, "ready", Some("Confirm finding selection before publishing"))
        .unwrap();
    db.reserve_review_result(&ReviewResultInput {
        pr_id,
        run_id,
        actionable_run_id: Some(run_id),
        observed_head_sha: "head",
        reviewed_head_sha: "head",
        prior_result_state: None,
        prior_result_head_sha: None,
        observation: "findings",
        effective_state: "awaiting_action",
        publication_status: "disabled",
        body: None,
        github_review_id: None,
        github_review_url: None,
        reviewer_version: "fixture",
        skill_version: "fixture",
    })
    .unwrap();

    let card = db
        .list_review_board()
        .unwrap()
        .into_iter()
        .flat_map(|group| group.items)
        .find(|card| card.id == pr_id)
        .expect("board card");
    assert_eq!(card.lane, "reviewed_cleanly");
    assert_eq!(card.result_state.as_deref(), Some("clean"));
    assert_eq!(card.status_reason.as_deref(), Some("Nada a sinalizar"));
}

#[test]
fn review_board_keeps_inherited_findings_in_needs_you() {
    let db = Db::for_test().open().expect("open db");
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    let pr_id = db
        .upsert_pull_request(&PullRequestInput {
            repo_id: 1,
            number: 44,
            title: "Inherited findings fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "new-head".into(),
            base_sha: Some("base".into()),
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "ready".into(),
            status_reason: Some("Review ready".into()),
        })
        .unwrap();
    let prior_run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    db.conn.execute(
        "UPDATE review_runs SET status = 'preview', review_body = '[Achado] Prior issue', comments_json = '{\"comments\":[{\"body\":\"[Achado] Prior issue\"}]}' WHERE id = $1",
        params![prior_run_id],
    ).unwrap();
    db.reserve_review_result(&ReviewResultInput {
        pr_id,
        run_id: prior_run_id,
        actionable_run_id: Some(prior_run_id),
        observed_head_sha: "old-head",
        reviewed_head_sha: "old-head",
        prior_result_state: None,
        prior_result_head_sha: None,
        observation: "findings",
        effective_state: "awaiting_action",
        publication_status: "disabled",
        body: None,
        github_review_id: None,
        github_review_url: None,
        reviewer_version: "fixture",
        skill_version: "fixture",
    })
    .unwrap();
    let no_change_run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    db.set_review_run_running(no_change_run_id, "[]").unwrap();
    db.store_finding_audit(
        no_change_run_id,
        &crate::review::findings::FindingAudit {
            evidence_warnings: None,
            head_sha: "new-head".into(),
            complete: true,
            original_body: "No new changes".into(),
            diff: None,
            candidates: vec![],
        },
    )
    .unwrap();
    db.finish_review_run_only(no_change_run_id, "No new changes", Some("{\"comments\":[]}"), "[]")
        .unwrap();
    db.set_pr_lane(pr_id, "ready", Some("Review ready")).unwrap();
    db.reserve_review_result(&ReviewResultInput {
        pr_id,
        run_id: no_change_run_id,
        actionable_run_id: Some(prior_run_id),
        observed_head_sha: "new-head",
        reviewed_head_sha: "old-head",
        prior_result_state: Some("awaiting_action"),
        prior_result_head_sha: Some("old-head"),
        observation: "no_new_changes",
        effective_state: "awaiting_action",
        publication_status: "disabled",
        body: None,
        github_review_id: None,
        github_review_url: None,
        reviewer_version: "fixture",
        skill_version: "fixture",
    })
    .unwrap();

    let card = db
        .list_review_board()
        .unwrap()
        .into_iter()
        .flat_map(|group| group.items)
        .find(|card| card.id == pr_id)
        .expect("board card");
    assert_eq!(card.lane, "ready");
    assert_eq!(card.result_state.as_deref(), Some("awaiting_action"));
}

#[test]
fn review_board_ignores_malformed_current_audit() {
    let db = Db::for_test().open().expect("open db");
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    let pr_id = db
        .upsert_pull_request(&PullRequestInput {
            repo_id: 1,
            number: 45,
            title: "Malformed audit fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: Some("base".into()),
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "ready".into(),
            status_reason: Some("Review ready".into()),
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
            original_body: "Nada a sinalizar".into(),
            diff: None,
            candidates: vec![],
        },
    )
    .unwrap();
    db.finish_review_run_only(run_id, "Nada a sinalizar", Some("{\"comments\":[]}"), "[]")
        .unwrap();
    db.set_pr_lane(pr_id, "ready", Some("Review ready")).unwrap();
    db.conn
        .execute(
            "UPDATE review_finding_audits SET audit_json = 'not-json' WHERE run_id = $1",
            params![run_id],
        )
        .unwrap();
    db.reserve_review_result(&ReviewResultInput {
        pr_id,
        run_id,
        actionable_run_id: Some(run_id),
        observed_head_sha: "head",
        reviewed_head_sha: "head",
        prior_result_state: None,
        prior_result_head_sha: None,
        observation: "findings",
        effective_state: "awaiting_action",
        publication_status: "disabled",
        body: None,
        github_review_id: None,
        github_review_url: None,
        reviewer_version: "fixture",
        skill_version: "fixture",
    })
    .unwrap();

    let card = db
        .list_review_board()
        .unwrap()
        .into_iter()
        .flat_map(|group| group.items)
        .find(|card| card.id == pr_id)
        .expect("board card");
    assert_eq!(card.lane, "ready");
    assert_eq!(card.result_state.as_deref(), Some("awaiting_action"));
}

#[test]
fn terminal_pr_state_removes_needs_you_pending_actions() {
    let db = Db::for_test().open().expect("open db");
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    let pr_id = db
        .upsert_pull_request(&PullRequestInput {
            repo_id: 1,
            number: 42,
            title: "Fixture".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: Some("base".into()),
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "manual_required".into(),
            status_reason: Some("Manual review needed".into()),
        })
        .unwrap();
    db.upsert_pending_action("manual_review_needed", 1, pr_id, "Manual review needed", "review")
        .unwrap();
    db.mark_pr_terminal(pr_id, "MERGED", "PR merged on GitHub").unwrap();
    assert!(db.list_pending_actions().unwrap().is_empty());
    let card = db
        .list_review_board()
        .unwrap()
        .into_iter()
        .flat_map(|group| group.items)
        .find(|card| card.id == pr_id)
        .unwrap();
    assert_eq!(card.state, "merged");
    assert_eq!(card.lane, "merged");
}

#[test]
fn accounts_of_removed_providers_are_dropped() {
    let db = Db::for_test().open().unwrap();
    let claude = db.create_ai_account("claude", "Work").unwrap();
    db.conn
        .execute(
            "INSERT INTO ai_accounts (provider, label, priority, created_at, updated_at) VALUES ('gemini', 'Old', 9, 0, 0)",
            [],
        )
        .unwrap();
    db.save_setting_value("review_engine_gemini", r#"{"model":"pro","effort":""}"#)
        .unwrap();
    db.save_review_engine("claude", "claude-sonnet-5-5", "high").unwrap();
    let removed = db.remove_unsupported_ai_accounts().unwrap();
    assert_eq!(removed.len(), 1);
    assert!(!removed.contains(&claude.id));
    assert!(db.remove_unsupported_ai_accounts().unwrap().is_empty());
    let accounts = db.list_ai_accounts().unwrap();
    assert_eq!(
        accounts.iter().map(|account| account.id).collect::<Vec<_>>(),
        vec![claude.id]
    );
    assert_eq!(db.setting("review_engine_gemini").unwrap(), None);
    assert_eq!(
        db.review_engine("claude").unwrap(),
        ("claude-sonnet-5-5".to_string(), "high".to_string())
    );
}

#[test]
fn ai_accounts_are_picked_by_priority_and_rest_after_limits() {
    let db = Db::for_test().open().unwrap();
    assert!(db.pick_ai_account(&[]).unwrap().is_none());
    let first = db.create_ai_account("claude", "Work").unwrap();
    let second = db.create_ai_account("claude", "Personal").unwrap();
    let codex = db.create_ai_account("codex", "Codex").unwrap();
    assert_eq!(db.pick_ai_account(&[]).unwrap().unwrap().id, first.id);
    assert_eq!(db.pick_ai_account(&[first.id]).unwrap().unwrap().id, second.id);
    // Once every Claude account is used up, reviews fall through to Codex.
    assert_eq!(
        db.pick_ai_account(&[first.id, second.id]).unwrap().unwrap().id,
        codex.id
    );
    // The first account's provider is used up before another provider, whatever their order.
    db.reorder_ai_accounts(&[first.id, codex.id, second.id]).unwrap();
    assert_eq!(db.pick_ai_account(&[first.id]).unwrap().unwrap().id, second.id);
    db.reorder_ai_accounts(&[first.id, second.id, codex.id]).unwrap();

    let until = db.mark_ai_account_limited(first.id, None, "limit").unwrap();
    assert!(until > now_millis());
    assert_eq!(db.pick_ai_account(&[]).unwrap().unwrap().id, second.id);
    assert_eq!(db.next_ai_account_reset().unwrap(), Some(until));
    db.mark_ai_account_auth_failed(second.id, "rejected").unwrap();
    assert_eq!(db.pick_ai_account(&[]).unwrap().unwrap().id, codex.id);
    db.set_ai_account_enabled(codex.id, false).unwrap();
    assert!(db.pick_ai_account(&[]).unwrap().is_none());
    db.set_ai_account_enabled(codex.id, true).unwrap();

    // A rest that has already elapsed makes the account available again.
    db.conn
        .execute("UPDATE ai_accounts SET limited_until = 1 WHERE id = $1", [first.id])
        .unwrap();
    assert_eq!(db.pick_ai_account(&[]).unwrap().unwrap().id, first.id);
    db.mark_ai_account_used(first.id).unwrap();
    assert_eq!(db.ai_account(first.id).unwrap().status, "ok");

    db.set_ai_account_enabled(first.id, false).unwrap();
    db.reset_ai_account_status(second.id).unwrap();
    assert_eq!(db.pick_ai_account(&[]).unwrap().unwrap().id, second.id);

    let order = db.reorder_ai_accounts(&[codex.id, second.id, first.id]).unwrap();
    assert_eq!(
        order.iter().map(|account| account.id).collect::<Vec<_>>(),
        [codex.id, second.id, first.id]
    );
    assert!(db.reorder_ai_accounts(&[codex.id, second.id]).is_err());
    assert!(db.create_ai_account("other", "Nope").is_err());
    assert!(db.create_ai_account("claude", "  ").is_err());
}

#[test]
fn empty_repository_index_is_not_a_failure() {
    let db = Db::for_test().open().unwrap();
    db.upsert_workspaces(&[WorkspaceInput {
        login: "acme".to_string(),
        kind: "folder".to_string(),
    }])
    .unwrap();
    let repo_id = db
        .upsert_repos(&[RepoInput {
            workspace_login: "acme".to_string(),
            owner: "acme".to_string(),
            name: "new".to_string(),
            full_name: "acme/new".to_string(),
            description: None,
            is_private: true,
            default_branch: None,
        }])
        .unwrap()[0]
        .id;
    db.begin_repo_index(repo_id, None).unwrap();
    db.fail_repo_index(repo_id, "earlier failure").unwrap();
    let status = db.mark_repo_index_empty(repo_id).unwrap();
    assert_eq!(status.status, "empty");
    assert_eq!(status.error_message, None);
}

#[test]
fn review_engines_default_per_provider_and_reject_unknown_choices() {
    let db = Db::for_test().open().unwrap();
    assert_eq!(
        db.review_engine("claude").unwrap(),
        ("claude-opus-5-5".to_string(), "medium".to_string())
    );
    db.save_review_engine("codex", "gpt-5.5", "high").unwrap();
    assert_eq!(
        db.review_engine("codex").unwrap(),
        ("gpt-5.5".to_string(), "high".to_string())
    );
    assert!(db.save_review_engine("codex", "gpt-5.5", "ultra").is_err());
    assert!(db.save_review_engine("gemini", "claude-opus-5-5", "").is_err());
    // A choice the app no longer offers falls back to the provider's default.
    db.save_setting_value("review_engine_codex", r#"{"model":"gone","effort":""}"#)
        .unwrap();
    assert_eq!(
        db.review_engine("codex").unwrap(),
        ("default".to_string(), "medium".to_string())
    );
}

#[test]
fn a_repository_leaving_the_workspace_keeps_its_review_history() {
    let db = Db::for_test().open().expect("open db");
    db.upsert_workspaces(&[WorkspaceInput {
        login: "acme".into(),
        kind: "org".into(),
    }])
    .unwrap();
    let repo = |name: &str| RepoInput {
        workspace_login: "acme".into(),
        owner: "acme".into(),
        name: name.into(),
        full_name: format!("acme/{name}"),
        description: None,
        is_private: true,
        default_branch: Some("main".into()),
    };
    let listed = db.upsert_repos(&[repo("api"), repo("web")]).unwrap();
    let api = listed.iter().find(|repo| repo.full_name == "acme/api").unwrap().id;
    db.conn
        .execute(
            "INSERT INTO pull_requests (id, repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at)
             VALUES (1, $1, 1, 'PR', 'a', 'head', 1, 1, 1)",
            [api],
        )
        .unwrap();
    let run = db.start_review_skeleton_with(1, None).unwrap();
    db.upsert_pending_action("manual_review_needed", api, 1, "Manual review needed", "review")
        .unwrap();
    // The workspace no longer lists acme/api: it leaves, its history stays.
    let listed = db.upsert_repos(&[repo("web")]).unwrap();
    assert!(listed.iter().all(|repo| repo.id != api));
    assert!(db.review_run_by_id(run).unwrap().is_some());
    let removed: Option<i64> = db
        .conn
        .query_row("SELECT removed_at FROM repos WHERE id = $1", [api], |row| row.get(0))
        .unwrap();
    assert!(removed.is_some());
    assert!(db.queued_pr_ids().unwrap().is_empty());
    let open_notices: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pending_actions WHERE status != 'resolved'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(open_notices, 0, "a removed repository's notices cannot be acted on");
    // Listed again, it is the same repository with the same history.
    let listed = db.upsert_repos(&[repo("api"), repo("web")]).unwrap();
    assert!(listed.iter().any(|repo| repo.id == api));
    let removed: Option<i64> = db
        .conn
        .query_row("SELECT removed_at FROM repos WHERE id = $1", [api], |row| row.get(0))
        .unwrap();
    assert!(removed.is_none());
}

#[test]
fn the_board_carries_an_auto_post_failure_and_a_suggestions_only_mark_until_the_next_review() {
    let db = Db::for_test().open().unwrap();
    db.conn.execute("INSERT INTO repos (owner,name,full_name,is_private,created_at,updated_at) VALUES ('acme','api','acme/api',1,1,1)", []).unwrap();
    let repo = db.conn.last_insert_rowid();
    db.save_repo_subscription(repo, true, "manual", false, false).unwrap();
    db.conn.execute("INSERT INTO pull_requests (repo_id,number,title,author,head_sha,review_lane,status_reason,last_synced_at,created_at,updated_at) VALUES ($1,1,'PR','a','head','ready','Stacked PR: reviewing a → b',1,1,1)", [repo]).unwrap();
    let pr = db.conn.last_insert_rowid();
    db.conn.execute("INSERT INTO review_runs (pr_id,head_sha,status,review_body,created_at,updated_at) VALUES ($1,'head','preview','**[Achado] Bug**',1,1)", [pr]).unwrap();
    let run = db.conn.last_insert_rowid();
    let input = ReviewResultInput {
        pr_id: pr,
        run_id: run,
        actionable_run_id: Some(run),
        observed_head_sha: "head",
        reviewed_head_sha: "head",
        prior_result_state: None,
        prior_result_head_sha: None,
        observation: "findings",
        effective_state: "awaiting_action",
        publication_status: "disabled",
        body: None,
        github_review_id: None,
        github_review_url: None,
        reviewer_version: "fixture",
        skill_version: "fixture",
    };
    db.reserve_review_result(&input).unwrap();
    let card = |db: &Database| {
        db.list_review_board()
            .unwrap()
            .into_iter()
            .flat_map(|group| group.items)
            .find(|card| card.id == pr)
            .unwrap()
    };
    // Only the run's own result left awaiting a decision takes the reason.
    db.note_auto_post_failure(run + 1000, "Auto-post failed: another run")
        .unwrap();
    assert_eq!(card(&db).result_error, None);
    db.conn
        .execute(
            "UPDATE review_results SET effective_state = 'findings_posted' WHERE run_id = $1",
            [run],
        )
        .unwrap();
    db.note_auto_post_failure(run, "Auto-post failed: already posted")
        .unwrap();
    assert_eq!(card(&db).result_error, None);
    db.conn
        .execute(
            "UPDATE review_results SET effective_state = 'awaiting_action' WHERE run_id = $1",
            [run],
        )
        .unwrap();
    db.note_auto_post_failure(run, "Auto-post failed: PR base/diff scope changed")
        .unwrap();
    // The board shows its own text in status_reason for a run awaiting a decision; the failure
    // travels on the result instead.
    assert_eq!(
        card(&db).result_error.as_deref(),
        Some("Auto-post failed: PR base/diff scope changed")
    );
    assert!(!card(&db).suggestions_only);
    // A review whose only points are summary suggestions is marked so on the board.
    let section = crate::review::evidence::suggestions_section(&[serde_json::json!(
        {"path":"src/a.rs","line":2,"title":"Stale test name","detail":"Detail","action":"Rename it"}
    )]);
    db.conn
        .execute(
            "UPDATE review_runs SET review_body = $1, comments_json = $2 WHERE id = $3",
            params![
                format!("0 review comments selected for publication.\n\n{section}"),
                r#"{"comments":[]}"#,
                run
            ],
        )
        .unwrap();
    assert!(card(&db).suggestions_only);
    // The next review's result clears the failure, and its findings clear the suggestions-only mark.
    // It keeps its suggestions, so only its finding can clear the mark.
    db.conn.execute("INSERT INTO review_runs (pr_id,head_sha,status,review_body,created_at,updated_at) VALUES ($1,'head','preview',$2,2,2)", params![pr, format!("**[Achado] Bug**\n\n{section}")]).unwrap();
    let next = db.conn.last_insert_rowid();
    db.reserve_review_result(&ReviewResultInput {
        run_id: next,
        actionable_run_id: Some(next),
        ..input
    })
    .unwrap();
    assert_eq!(card(&db).result_error, None);
    assert!(!card(&db).suggestions_only);
}

#[test]
fn a_run_with_only_excluded_files_is_skipped_not_failed_or_clean() {
    let db = Db::for_test().open().expect("open db");
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    db.save_repo_subscription(1, true, "manual", false, false).unwrap();
    let pr_id = db
        .upsert_pull_request(&PullRequestInput {
            repo_id: 1,
            number: 43,
            title: "Lockfile refresh".into(),
            author: "fixture".into(),
            author_avatar_url: None,
            is_draft: false,
            head_sha: "head".into(),
            base_sha: Some("base".into()),
            base_ref: Some("main".into()),
            head_ref: Some("feature".into()),
            default_branch: Some("main".into()),
            url: None,
            lane: "open".into(),
            status_reason: None,
        })
        .unwrap();
    let run_id = db.start_review_skeleton_with(pr_id, Some("fixture")).unwrap();
    let reason = format!("{}: every changed file is excluded", crate::review::NOTHING_REVIEWABLE);
    db.skip_review_run_without_reviewable_files(run_id, &reason).unwrap();
    let run = db.review_run_by_id(run_id).unwrap().unwrap();
    assert_eq!(run.status, "skipped");
    let (lane, status): (String, Option<String>) = db
        .conn
        .query_row(
            "SELECT review_lane, status_reason FROM pull_requests WHERE id = $1",
            params![pr_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(lane, "skipped");
    assert!(status.unwrap().starts_with("Not reviewed"));
    assert!(db
        .review_result(pr_id)
        .unwrap()
        .is_none_or(|result| result.effective_state != "clean"));
}
