fn candidate_audit_fixture() -> crate::review::findings::FindingAudit {
    let first = serde_json::json!({ "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": structured_finding() });
    let second = serde_json::json!({ "path": "src/lib.rs", "line": 3, "side": "RIGHT", "body": structured_finding().replace("Example issue", "Another issue") });
    let invalid =
        serde_json::json!({ "path": "src/lib.rs", "line": 99, "side": "RIGHT", "body": structured_finding() });
    let mut sources = serde_json::json!({ "comments": [{ "chunkId": 1, "comment": first }, { "chunkId": 2, "comment": second }, { "chunkId": 3, "comment": invalid }] });
    crate::review::findings::tag_candidates(&mut sources);
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,3 @@\n old\n+new\n+more\n";
    let output = serde_json::json!({ "commit_id": "abc", "comments": [first] }).to_string();
    let decisions = serde_json::json!({ "decisions": [
        { "candidateId": "candidate-1", "decision": "keep", "commentIndex": 0, "reason": "Confirmed by changed code" },
        { "candidateId": "candidate-2", "decision": "discard", "reason": "Caller already validates this" }
    ] })
    .to_string();
    crate::review::findings::finish_audit(
        crate::review::findings::seed_audit(&sources, "abc", diff),
        Some(&output),
        Some(&output),
        "AI summary",
        Some(&decisions),
        diff,
    )
}

#[test]
fn explained_discards_are_auditable_and_reversible() {
    let comment = serde_json::json!({ "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": structured_finding() });
    let mut sources = serde_json::json!({ "comments": [{ "chunkId": 1, "comment": comment }] });
    crate::review::findings::tag_candidates(&mut sources);
    let decisions = serde_json::json!({ "decisions": [{ "candidateId": "candidate-1", "decision": "discard", "reason": "Guard already handles this path" }] }).to_string();
    assert!(crate::review::findings::all_candidates_discarded(
        &sources,
        Some(&decisions)
    ));
    assert!(!crate::review::findings::all_candidates_discarded(&sources, None));
    assert!(!crate::review::findings::all_candidates_discarded(
        &sources,
        Some("{\"decisions\":[]}")
    ));
    let diff =
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,2 @@\n old\n+new\n";
    let empty = "{\"commit_id\":\"abc\",\"comments\":[]}";
    let mut audit = crate::review::findings::finish_audit(
        crate::review::findings::seed_audit(&sources, "abc", diff),
        Some(empty),
        Some(empty),
        "Nada a sinalizar",
        Some(&decisions),
        "",
    );
    assert!(!audit.candidates[0].selected);
    assert!(audit.candidates[0].blocked_reason.is_none());
    assert!(audit.diff.is_none());
    crate::review::findings::select(&mut audit, &["candidate-1".to_string()]).unwrap();
    assert_eq!(audit.candidates[0].ai_decision, "discarded");
    assert!(audit.candidates[0].selected);
}

#[test]
fn body_findings_can_be_excluded_without_leaking_into_publication() {
    let sources = serde_json::json!({ "comments": [] });
    let mut audit = crate::review::findings::finish_audit(
        crate::review::findings::seed_audit(&sources, "abc", ""),
        None,
        None,
        structured_finding(),
        None,
        "",
    );
    assert_eq!(audit.candidates[0].origin, "body");
    assert!(audit.candidates[0].selected);
    crate::review::findings::select(&mut audit, &[]).unwrap();
    let (body, comments) = crate::review::findings::selection_payload(&audit);
    assert!(!body.contains("[Achado]"));
    assert!(!comments.contains("[Achado]"));
    crate::review::findings::select(&mut audit, &["body-1".to_string()]).unwrap();
    assert_eq!(
        crate::review::findings::selection_payload(&audit).0,
        structured_finding()
    );
}

#[test]
fn candidate_selection_preserves_ai_decisions_and_templates() {
    let mut audit = candidate_audit_fixture();
    assert!(audit.candidates[0].selected);
    assert_eq!(audit.candidates[1].ai_decision, "discarded");
    assert!(audit.candidates[2].blocked_reason.is_some());
    assert!(crate::review::findings::select(&mut audit, &["candidate-3".to_string()]).is_err());
    assert!(crate::review::findings::select(&mut audit, &["missing".to_string()]).is_err());
    crate::review::findings::select(&mut audit, &["candidate-2".to_string()]).unwrap();
    assert!(!audit.candidates[0].selected);
    assert_eq!(audit.candidates[0].ai_decision, "kept");
    assert_eq!(
        audit.candidates[1].ai_reason.as_deref(),
        Some("Caller already validates this")
    );
    let (body, raw) = crate::review::findings::selection_payload(&audit);
    assert!(!body.contains("AI summary"));
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["comments"].as_array().unwrap().len(), 1);
    assert_eq!(value["comments"][0]["line"], 3);
    assert!(super::is_structured_review_point(
        value["comments"][0]["body"].as_str().unwrap()
    ));
    assert!(value["comments"][0].get("candidateId").is_none());
}

fn finding_selection_fixture() -> (std::path::PathBuf, crate::db::Database, i64, i64) {
    use crate::db::PullRequestInput;
    let root = std::env::temp_dir().join(format!(
        "ocr-findings-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let db = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/api").unwrap();
    let pr_id = db
        .upsert_pull_request(&PullRequestInput {
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
    assert!(db.review_requires_finding_selection(run_id).unwrap());
    db.set_review_run_running(run_id, "[]").unwrap();
    (root, db, pr_id, run_id)
}

#[test]
fn evidence_warnings_survive_selection_cleanup_and_database_reopen() {
    let (root, db, pr_id, run_id) = finding_selection_fixture();
    let warnings = vec![crate::review::warnings::EvidenceWarning {
        reason: "insufficient_evidence".into(),
        path: Some("src/lib.rs".into()),
        reference: None,
    }];
    let mut audit = candidate_audit_fixture();
    audit.evidence_warnings = Some(warnings.clone());
    db.store_finding_audit(run_id, &audit).unwrap();
    db.finish_review_run(run_id, "AI summary", None, "[]").unwrap();
    let saved = db.save_finding_selection(pr_id, run_id, 0, &[]).unwrap();
    assert!(saved.editable && saved.audit.as_ref().unwrap().complete);
    assert_eq!(saved.audit.unwrap().evidence_warnings, Some(warnings.clone()));
    assert!(!db
        .review_run_by_id(run_id)
        .unwrap()
        .unwrap()
        .review_body
        .unwrap()
        .contains("WARNING"));
    db.clear_finding_evidence(run_id).unwrap();
    let handle = db.handle();
    drop(db);
    let reopened = handle.open().unwrap();
    assert_eq!(reopened.review_evidence_warnings(run_id).unwrap(), warnings);
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

fn question_audit_fixture(body_only: bool, with_finding: bool) -> crate::review::findings::FindingAudit {
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,3 @@\n old\n+new\n+more\n";
    let mut comments = vec![];
    if !body_only {
        comments.push(
            serde_json::json!({ "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": structured_question() }),
        );
    }
    if with_finding {
        comments.push(
            serde_json::json!({ "path": "src/lib.rs", "line": 3, "side": "RIGHT", "body": structured_finding() }),
        );
    }
    let output = serde_json::json!({ "commit_id": "abc", "comments": comments }).to_string();
    crate::review::findings::finish_audit(
        crate::review::findings::seed_audit(&serde_json::json!({ "comments": [] }), "abc", diff),
        Some(&output),
        Some(&output),
        if body_only {
            structured_question()
        } else {
            "Questions for confirmation"
        },
        None,
        diff,
    )
}

#[test]
fn reprocess_intent_blocks_publication_before_and_after_replacement() {
    for publication_first in [false, true] {
        let (root, db, pr_id, run_id) = finding_selection_fixture();
        db.save_setting("auto_post_mode", "comment").unwrap();
        db.store_finding_audit(run_id, &question_audit_fixture(false, false))
            .unwrap();
        db.finish_review_run(run_id, "AI summary", None, "[]").unwrap();
        let revision = db
            .auto_confirm_generated_finding_selection(pr_id, run_id)
            .unwrap()
            .unwrap();
        let publisher = crate::db::Db::for_test_at(root.join("test.sqlite")).open().unwrap();
        let publish = || {
            publisher
                .try_begin_review_post(run_id, pr_id, "abc", "COMMENT", "comment", "auto", Some(revision))
                .unwrap()
        };
        if publication_first {
            assert!(publish());
            assert!(db.register_review_intent(pr_id, false, true).is_err());
        } else {
            assert_eq!(db.register_review_intent(pr_id, false, true).unwrap(), (false, true));
            assert!(!publish());
            assert_eq!(db.register_review_intent(pr_id, false, false).unwrap(), (false, true));
            let replacement = db.start_review_skeleton_with(pr_id, None).unwrap();
            db.set_reprocess_next_review(pr_id, false).unwrap();
            assert!(!publish());
            assert!(replacement > run_id);
        }
        drop(publisher);
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn reprocess_scope_skips_empty_observations_and_rejects_corrupt_history() {
    let (root, db, pr_id, run_id) = finding_selection_fixture();
    let plan = serde_json::json!({"repoId":1,"changedFiles":["docs/programs.md"],"packages":[],"dependentPackages":[],"relatedFiles":[],"testFiles":[],"conventionFiles":[],"omittedCount":0,"incremental":false,"range":"base...abc","headSha":"abc"});
    db.save_review_context_snapshot(1, pr_id, Some(run_id), "abc", None, None, &plan.to_string(), 1, 0)
        .unwrap();
    db.finish_review_run(run_id, "Question", None, "[]").unwrap();
    let empty_run = db.start_review_skeleton_with(pr_id, None).unwrap();
    db.set_review_run_running(empty_run, "[]").unwrap();
    let mut empty = plan.clone();
    empty["changedFiles"] = serde_json::json!([]);
    db.save_review_context_snapshot(1, pr_id, Some(empty_run), "abc", None, None, &empty.to_string(), 0, 0)
        .unwrap();
    assert_eq!(
        db.reprocess_context_plan(pr_id, empty_run + 1).unwrap().unwrap().0,
        run_id
    );
    assert!(db.reprocess_context_plan(pr_id + 1, empty_run + 1).unwrap().is_none());
    db.set_reprocess_next_review(pr_id, true).unwrap();
    db.cancel_review_for_pr(pr_id).unwrap();
    assert!(!db.take_reprocess_next_review(pr_id).unwrap());
    db.save_review_context_snapshot(1, pr_id, Some(empty_run), "abc", None, None, "invalid", 1, 0)
        .unwrap();
    assert!(db.reprocess_context_plan(pr_id, empty_run + 1).is_err());
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn auto_post_questions_preserve_validated_selection_and_use_comment_lock() {
    for mode in ["comment", "request_changes"] {
        for (body_only, with_finding) in [(false, false), (true, false), (false, true), (true, true)] {
            let (root, db, pr_id, run_id) = finding_selection_fixture();
            db.save_setting("auto_post_mode", mode).unwrap();
            let audit = question_audit_fixture(body_only, with_finding);
            assert!(audit.candidates.iter().all(|candidate| candidate.selected
                && candidate.default_selected
                && candidate.blocked_reason.is_none()));
            db.store_finding_audit(run_id, &audit).unwrap();
            db.finish_review_run(run_id, "AI summary", None, "[]").unwrap();
            let revision = db
                .auto_confirm_generated_finding_selection(pr_id, run_id)
                .unwrap()
                .unwrap();
            let saved = db.review_run_by_id(run_id).unwrap().unwrap();
            let (body, comments) = crate::review::findings::selection_payload(&audit);
            assert_eq!(saved.review_body.as_deref(), Some(body.as_str()));
            assert_eq!(saved.comments_json.as_deref(), Some(comments.as_str()));
            assert_eq!(
                crate::review::findings::actionable(Some(&comments), Some(&body)),
                with_finding
            );
            let (event, decision, wrong_event, wrong_decision) = if mode == "request_changes" && with_finding {
                ("REQUEST_CHANGES", "request_changes", "COMMENT", "comment")
            } else {
                ("COMMENT", "comment", "REQUEST_CHANGES", "request_changes")
            };
            assert_eq!(
                crate::review::findings::auto_post_event(mode, Some(&comments), Some(&body)),
                Some(event)
            );
            let payload: serde_json::Value = serde_json::from_str(
                &crate::server::build_review_payload(
                    "acme/api",
                    "abc",
                    event,
                    &body,
                    Some(&comments),
                    Some("<!-- ocr-test -->"),
                    &[],
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(payload["event"], event);
            assert_eq!(payload["commit_id"], "abc");
            assert_eq!(
                payload["comments"],
                serde_json::from_str::<serde_json::Value>(&comments).unwrap()["comments"]
            );
            assert!(payload["body"].as_str().unwrap().ends_with("<!-- ocr-test -->"));
            assert!(!db
                .try_begin_review_post(
                    run_id,
                    pr_id,
                    "abc",
                    wrong_event,
                    wrong_decision,
                    "auto",
                    Some(revision)
                )
                .unwrap());
            db.save_setting("auto_post_mode", "off").unwrap();
            assert!(!db
                .try_begin_review_post(run_id, pr_id, "abc", event, decision, "auto", Some(revision))
                .unwrap());
            db.save_setting("auto_post_mode", mode).unwrap();
            assert!(!db
                .try_begin_review_post(run_id, pr_id, "abc", event, decision, "auto", Some(revision - 1))
                .unwrap());
            assert!(db
                .try_begin_review_post(run_id, pr_id, "abc", event, decision, "auto", Some(revision))
                .unwrap());
            assert!(!db
                .try_begin_review_post(run_id, pr_id, "abc", event, decision, "auto", Some(revision))
                .unwrap());
            assert!(db.review_requires_finding_selection(run_id).unwrap());
            drop(db);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn auto_post_questions_respect_manual_selection_and_unsafe_audits() {
    for case in [
        "manual",
        "human_excluded",
        "discarded",
        "blocked",
        "incomplete",
        "stale",
        "legacy_unselected",
    ] {
        let (root, db, pr_id, run_id) = finding_selection_fixture();
        db.save_setting(
            "auto_post_mode",
            if case == "manual" { "off" } else { "request_changes" },
        )
        .unwrap();
        let mut audit = question_audit_fixture(false, false);
        match case {
            "discarded" | "legacy_unselected" => {
                audit.candidates[0].default_selected = false;
                audit.candidates[0].selected = false;
                if case == "discarded" {
                    audit.candidates[0].ai_decision = "discarded".into();
                }
            }
            "blocked" => audit.candidates[0].blocked_reason = Some("Invalid anchor".into()),
            "incomplete" => audit.complete = false,
            "stale" => audit.head_sha = "old-head".into(),
            _ => {}
        }
        db.store_finding_audit(run_id, &audit).unwrap();
        db.finish_review_run(run_id, "AI summary", None, "[]").unwrap();
        if case == "human_excluded" {
            db.save_finding_selection(pr_id, run_id, 0, &[]).unwrap();
        }
        if case != "manual" {
            assert!(
                !db.auto_confirm_generated_finding_selection(pr_id, run_id)
                    .is_ok_and(|revision| revision.is_some()),
                "{case}"
            );
        }
        let view = db.finding_audit(pr_id, run_id).unwrap();
        for (event, decision) in [("COMMENT", "comment"), ("REQUEST_CHANGES", "request_changes")] {
            assert!(
                !db.try_begin_review_post(run_id, pr_id, "abc", event, decision, "auto", Some(view.revision))
                    .unwrap(),
                "{case}"
            );
        }
        assert_eq!(db.review_run_by_id(run_id).unwrap().unwrap().status, "preview");
        assert_eq!(view.selection_saved, case == "human_excluded");
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn auto_post_acquires_versioned_finding_selection_lock() {
    for (mode, event) in [("comment", "COMMENT"), ("request_changes", "REQUEST_CHANGES")] {
        let (root, db, pr_id, run_id) = finding_selection_fixture();
        db.save_setting("auto_post_mode", mode).unwrap();
        db.store_finding_audit(run_id, &candidate_audit_fixture()).unwrap();
        db.finish_review_run(run_id, "AI summary", None, "[]").unwrap();
        assert!(!db
            .try_begin_review_post(run_id, pr_id, "abc", event, mode, "auto", Some(0))
            .unwrap());
        let revision = db
            .auto_confirm_generated_finding_selection(pr_id, run_id)
            .unwrap()
            .unwrap();
        let view = db.finding_audit(pr_id, run_id).unwrap();
        assert!(view.selection_saved);
        assert_eq!(view.revision, revision);
        assert_eq!(
            db.auto_confirm_generated_finding_selection(pr_id, run_id).unwrap(),
            Some(revision)
        );
        for rejected_revision in [None, Some(revision - 1)] {
            assert!(!db
                .try_begin_review_post(run_id, pr_id, "abc", event, mode, "auto", rejected_revision)
                .unwrap());
        }
        db.save_setting("auto_post_mode", "off").unwrap();
        assert!(!db
            .try_begin_review_post(run_id, pr_id, "abc", event, mode, "auto", Some(revision))
            .unwrap());
        db.save_setting("auto_post_mode", mode).unwrap();
        for (wrong_event, wrong_decision) in [
            ("APPROVE", "approve_post"),
            ("COMMENT", "request_changes"),
            ("REQUEST_CHANGES", "comment"),
        ] {
            assert!(!db
                .try_begin_review_post(
                    run_id,
                    pr_id,
                    "abc",
                    wrong_event,
                    wrong_decision,
                    "auto",
                    Some(revision)
                )
                .unwrap());
        }
        let (other_mode, other_event) = if mode == "comment" {
            ("request_changes", "REQUEST_CHANGES")
        } else {
            ("comment", "COMMENT")
        };
        assert!(!db
            .try_begin_review_post(run_id, pr_id, "abc", other_event, other_mode, "auto", Some(revision))
            .unwrap());
        let saved = db.review_run_by_id(run_id).unwrap().unwrap();
        let comments: serde_json::Value = serde_json::from_str(saved.comments_json.as_deref().unwrap()).unwrap();
        assert_eq!(comments["comments"].as_array().unwrap().len(), 1);
        assert_eq!(comments["comments"][0]["body"], structured_finding());
        assert!(db
            .try_begin_review_post(run_id, pr_id, "abc", event, mode, "auto", Some(revision))
            .unwrap());
        assert_eq!(db.review_run_by_id(run_id).unwrap().unwrap().status, "posting");
        assert!(db.review_requires_finding_selection(run_id).unwrap());
        assert!(!db
            .try_begin_review_post(run_id, pr_id, "abc", event, mode, "auto", Some(view.revision))
            .unwrap());
        assert!(db.save_finding_selection(pr_id, run_id, view.revision, &[]).is_err());
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
#[ignore]
fn retained_auto_post_finding_selection_acquires_lock() {
    let input = std::io::read_to_string(std::io::stdin()).unwrap();
    let fixture: serde_json::Value = serde_json::from_str(&input).unwrap();
    let audit: crate::review::findings::FindingAudit = serde_json::from_value(fixture["audit"].clone()).unwrap();
    let mode = fixture["mode"].as_str().unwrap();
    let event = match mode {
        "comment" => "COMMENT",
        "request_changes" => "REQUEST_CHANGES",
        _ => panic!("Fixture must opt into automatic publication"),
    };
    let (root, db, pr_id, run_id) = finding_selection_fixture();
    db.save_setting("auto_post_mode", mode).unwrap();
    let conn = db.handle().open().unwrap();
    conn.execute("UPDATE pull_requests SET head_sha = $1", [&audit.head_sha])
        .unwrap();
    conn.execute("UPDATE review_runs SET head_sha = $1", [&audit.head_sha])
        .unwrap();
    db.store_finding_audit(run_id, &audit).unwrap();
    db.finish_review_run(
        run_id,
        fixture["body"].as_str().unwrap(),
        fixture["comments"].as_str(),
        "[]",
    )
    .unwrap();
    conn.execute(
        "UPDATE review_finding_audits SET revision = $1, selection_saved = $2",
        crate::pg_params![
            fixture["revision"].as_i64().unwrap(),
            // A BIGINT flag, as production code binds it.
            fixture["selectionSaved"].as_bool().unwrap() as i64
        ],
    )
    .unwrap();
    drop(conn);
    let revision = db
        .auto_confirm_generated_finding_selection(pr_id, run_id)
        .unwrap()
        .unwrap();
    assert!(db
        .try_begin_review_post(run_id, pr_id, &audit.head_sha, event, mode, "auto", Some(revision))
        .unwrap());
    assert!(!db
        .try_begin_review_post(run_id, pr_id, &audit.head_sha, event, mode, "auto", Some(revision))
        .unwrap());
    let saved = db.review_run_by_id(run_id).unwrap().unwrap();
    assert_eq!(saved.status, "posting");
    let (body, comments) = crate::review::findings::selection_payload(&audit);
    assert!(
        saved.review_body.as_deref() == Some(body.as_str()),
        "Retained selection body changed"
    );
    assert!(
        saved.comments_json.as_deref() == Some(comments.as_str()),
        "Retained selection comments changed"
    );
    println!(
        "RETAINED_AUTO_POST {}",
        serde_json::json!({
            "runId": fixture["runId"], "revision": revision,
            "selected": audit.candidates.iter().filter(|candidate| candidate.selected).count(),
            "publicationLockAcquired": true, "duplicateBlocked": true,
            "modelCalls": 0, "githubWrites": 0, "operationalDatabaseWrites": 0
        })
    );
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn auto_post_rejects_unsafe_or_unavailable_finding_audits() {
    for case in ["empty", "discarded", "blocked", "incomplete", "stale", "missing"] {
        let (root, db, pr_id, run_id) = finding_selection_fixture();
        db.save_setting("auto_post_mode", "comment").unwrap();
        let mut audit = candidate_audit_fixture();
        match case {
            "empty" => audit.candidates.clear(),
            "discarded" => audit
                .candidates
                .iter_mut()
                .for_each(|candidate| candidate.default_selected = false),
            "blocked" => audit.candidates[0].blocked_reason = Some("Invalid anchor".into()),
            "incomplete" => audit.complete = false,
            "stale" => audit.head_sha = "old-head".into(),
            _ => {}
        }
        if case != "missing" {
            db.store_finding_audit(run_id, &audit).unwrap();
        }
        db.finish_review_run(run_id, "AI summary", None, "[]").unwrap();
        assert!(
            !db.auto_confirm_generated_finding_selection(pr_id, run_id)
                .is_ok_and(|revision| revision.is_some()),
            "{case}"
        );
        let view = db.finding_audit(pr_id, run_id).unwrap();
        assert!(!view.selection_saved, "{case}");
        assert!(
            !db.try_begin_review_post(run_id, pr_id, "abc", "COMMENT", "comment", "auto", Some(view.revision))
                .unwrap(),
            "{case}"
        );
        assert_eq!(db.review_run_by_id(run_id).unwrap().unwrap().status, "preview");
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn auto_post_preserves_human_edits_and_rejects_outdated_revisions() {
    let (root, db, pr_id, run_id) = finding_selection_fixture();
    db.save_setting("auto_post_mode", "comment").unwrap();
    db.store_finding_audit(run_id, &candidate_audit_fixture()).unwrap();
    db.finish_review_run(run_id, "AI summary", None, "[]").unwrap();
    let generated_revision = db
        .auto_confirm_generated_finding_selection(pr_id, run_id)
        .unwrap()
        .unwrap();
    let saved = db
        .save_finding_selection(pr_id, run_id, generated_revision, &["candidate-1".into()])
        .unwrap();
    assert!(!db
        .try_begin_review_post(
            run_id,
            pr_id,
            "abc",
            "COMMENT",
            "comment",
            "auto",
            Some(generated_revision)
        )
        .unwrap());
    let edited = db
        .save_finding_selection(pr_id, run_id, saved.revision, &["candidate-2".into()])
        .unwrap();
    assert_eq!(
        db.auto_confirm_generated_finding_selection(pr_id, run_id).unwrap(),
        None
    );
    assert_eq!(db.finding_audit(pr_id, run_id).unwrap().revision, edited.revision);
    assert!(!db
        .try_begin_review_post(
            run_id,
            pr_id,
            "abc",
            "COMMENT",
            "comment",
            "auto",
            Some(edited.revision)
        )
        .unwrap());
    assert!(db
        .try_begin_review_post(run_id, pr_id, "abc", "COMMENT", "comment", "you", Some(edited.revision))
        .unwrap());
    db.reset_review_posting(run_id).unwrap();
    let empty = db.save_finding_selection(pr_id, run_id, edited.revision, &[]).unwrap();
    assert_eq!(
        db.auto_confirm_generated_finding_selection(pr_id, run_id).unwrap(),
        None
    );
    assert!(!db
        .try_begin_review_post(run_id, pr_id, "abc", "COMMENT", "comment", "auto", Some(empty.revision))
        .unwrap());
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn auto_post_rejects_changed_review_scope_or_state() {
    for case in ["force_full", "newer_run", "closed", "head", "retarget", "superseded"] {
        let (root, db, pr_id, run_id) = finding_selection_fixture();
        db.save_setting("auto_post_mode", "comment").unwrap();
        db.store_finding_audit(run_id, &candidate_audit_fixture()).unwrap();
        db.finish_review_run(run_id, "AI summary", None, "[]").unwrap();
        let revision = db
            .auto_confirm_generated_finding_selection(pr_id, run_id)
            .unwrap()
            .unwrap();
        match case {
            "force_full" => db.set_force_full_next_review(pr_id, true).unwrap(),
            "newer_run" => {
                db.start_review_skeleton_with(pr_id, None).unwrap();
            }
            "closed" | "head" | "retarget" => {
                let conn = db.handle().open().unwrap();
                let sql = match case {
                    "closed" => "UPDATE pull_requests SET state = 'closed'",
                    "head" => "UPDATE pull_requests SET head_sha = 'new-head'",
                    _ => "UPDATE review_runs SET stacked_base_ref = 'main'; UPDATE pull_requests SET base_ref = 'other-base'",
                };
                conn.execute_batch(sql).unwrap();
            }
            _ => db
                .supersede_review_run_without_changing_pr(run_id, "New PR head")
                .unwrap(),
        }
        assert!(
            !db.try_begin_review_post(run_id, pr_id, "abc", "COMMENT", "comment", "auto", Some(revision))
                .unwrap(),
            "{case}"
        );
        if !matches!(case, "force_full" | "retarget") {
            assert!(
                db.auto_confirm_generated_finding_selection(pr_id, run_id).is_err(),
                "{case}"
            );
        }
        drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn finding_selection_is_versioned_and_locked_during_publication() {
    let (root, db, pr_id, run_id) = finding_selection_fixture();
    let evidence = crate::review::findings::seed_audit(
        &serde_json::json!({ "comments": [] }),
        "abc",
        "private validation evidence",
    );
    db.store_finding_audit(run_id, &evidence).unwrap();
    assert!(db.load_finding_audit(run_id).unwrap().unwrap().diff.is_some());
    assert!(!serde_json::to_string(&db.finding_audit(pr_id, run_id).unwrap())
        .unwrap()
        .contains("private validation evidence"));
    db.clear_finding_evidence(run_id).unwrap();
    assert!(db.load_finding_audit(run_id).unwrap().unwrap().diff.is_none());
    db.store_finding_audit(run_id, &candidate_audit_fixture()).unwrap();
    db.finish_review_run(run_id, "AI summary", None, "[]").unwrap();
    let generated_revision = db.auto_confirm_generated_finding_selection(pr_id, run_id).unwrap();
    let generated = db.finding_audit(pr_id, run_id).unwrap();
    assert_eq!(generated_revision, Some(generated.revision));
    assert!(generated.selection_saved);
    assert!(generated
        .audit
        .as_ref()
        .unwrap()
        .candidates
        .iter()
        .any(|candidate| candidate.selected));
    assert!(db.guard_finding_decision(pr_id, run_id, None, || Ok(())).is_err());
    let view = db.finding_audit(pr_id, run_id).unwrap();
    assert!(view.editable);
    let first = db
        .save_finding_selection(pr_id, run_id, view.revision, &["candidate-1".to_string()])
        .unwrap();
    assert!(db.save_finding_selection(pr_id, run_id, view.revision, &[]).is_err());
    let second = db
        .save_finding_selection(pr_id, run_id, first.revision, &["candidate-2".to_string()])
        .unwrap();
    assert!(db
        .guard_finding_decision(pr_id, run_id, Some(first.revision), || Ok(()))
        .is_err());
    assert!(!db
        .try_begin_review_post(run_id, pr_id, "abc", "COMMENT", "comment", "you", Some(first.revision))
        .unwrap());
    assert!(!db
        .try_begin_review_post(
            run_id,
            pr_id,
            "abc",
            "COMMENT",
            "comment",
            "auto",
            Some(second.revision)
        )
        .unwrap());
    assert!(db
        .try_begin_review_post(run_id, pr_id, "abc", "COMMENT", "comment", "you", Some(second.revision))
        .unwrap());
    assert!(db.save_finding_selection(pr_id, run_id, second.revision, &[]).is_err());
    db.finalize_review_decision(
        run_id,
        pr_id,
        "comment",
        "posted",
        "posted",
        "Fixture posted",
        Some("COMMENT"),
        "abc",
        Some("fixture"),
    )
    .unwrap();
    assert!(!db.finding_audit(pr_id, run_id).unwrap().editable);
    assert!(db.save_finding_selection(pr_id, run_id, second.revision, &[]).is_err());
    assert!(db.finding_audit(pr_id + 100, run_id).is_err());
    drop(db);
    std::fs::remove_dir_all(root).unwrap();
}

fn structured_finding() -> &'static str {
    "**[Achado] Example issue**\n\nDetails.\n\n<details>\n<summary>🔎 Detalhes</summary>\n\nWhy.\n\n</details>\n\n<details>\n<summary>🛠️ Correção sugerida</summary>\n\nFix it.\n\n</details>\n\n<details>\n<summary>🤖 Prompt para agentes de IA</summary>\n\n```text\nVerifique este achado no código atual.\nCorrija somente se ainda for válido; senão, pule o restante com um breve motivo. Mantenha\nas alterações mínimas e valide com os testes/typecheck relevantes.\n\nInspect src/lib.rs and fix only if still valid.\n```\n\n</details>"
}

fn structured_question() -> &'static str {
    "**[Pergunta] Is this behavior intentional?**\n\nClarify intent.\n\n<details>\n<summary>🔎 Por que pergunto</summary>\n\nEvidence.\n\n</details>\n\n<details>\n<summary>💬 O que confirmar</summary>\n\nConfirm expected behavior.\n\n</details>\n\n<details>\n<summary>🤖 Prompt para agentes de IA</summary>\n\n```text\nValide esta pergunta de revisão no código atual.\nNão aplique correção a menos que o autor já tenha respondido. Se respondeu,\nimplemente somente o caminho confirmado e valide com os testes/typecheck\nrelevantes.\n\nInspect src/lib.rs and ask only if still ambiguous.\n```\n\n</details>"
}

#[test]
fn rejects_unstructured_body_only_finding() {
    let result = super::validate_review_templates(
        "Found one correctness issue: automatic recycler creation can fail.",
        Some("{\"comments\":[]}"),
    );
    assert!(result.is_err());
}

#[test]
fn rejects_structured_finding_without_fixed_preamble() {
    let malformed = structured_finding().replace(
        super::FINDING_PROMPT_PREAMBLE,
        "Verify this finding against the current code.",
    );
    let result = super::validate_review_templates(&malformed, Some("{\"comments\":[]}"));
    assert!(result.is_err());
}

#[test]
fn rejects_unstructured_finding_about_no_issue_detection() {
    let result = super::validate_review_templates(
        "Found one validation regression in the relaxed no-issue summary detection: summaries that negate only questions can now hide an untemplated body-only finding.",
        Some("{\"comments\":[]}"),
    );
    assert!(result.is_err());
}

#[test]
fn repairs_unstructured_body_only_finding() {
    let repaired = super::repair_body_only_review_point(
        "Found one validation regression in the relaxed no-issue summary detection: summaries that negate only questions can now hide an untemplated body-only finding.".to_string(),
        Some("{\"comments\":[]}"),
    );
    assert!(!repaired.contains("<sub>🤖"));
    assert!(repaired.contains("**[Achado] validation regression"));
    assert!(repaired.contains("🔎 Detalhes"));
    assert!(repaired.contains("Por quê: Vi isto como um achado"));
    assert!(repaired.contains("🛠️ Correção sugerida"));
    assert!(repaired.contains("🤖 Prompt para agentes de IA"));
    assert!(repaired.contains("Achado de revisão a verificar:"));
    assert!(super::validate_review_templates(&repaired, Some("{\"comments\":[]}")).is_ok());
}

#[test]
fn accepts_neutral_body_only_summary() {
    assert!(super::validate_review_templates(
        "Reviewed the notification timestamp documentation and regression coverage; nothing evidence-backed to flag.",
        Some("{\"comments\":[]}"),
    )
    .is_ok());
}

#[test]
fn accepts_negated_body_only_finding_summary() {
    for body in [
        "No actionable findings or questions.",
        "Reviewed the PR and found no correctness issues to flag.",
    ] {
        assert!(super::validate_review_templates(body, Some("{\"comments\":[]}")).is_ok());
    }
}

#[test]
fn rejects_finding_plus_no_questions_summary() {
    for body in [
        "Found one correctness issue in the review validator. No questions.",
        "This finding identifies a validation regression. There are no questions for the author.",
    ] {
        assert!(super::validate_review_templates(body, Some("{\"comments\":[]}")).is_err());
    }
}

#[test]
fn accepts_structured_body_only_finding() {
    assert!(super::validate_review_templates(structured_finding(), Some("{\"comments\":[]}")).is_ok());
}

#[test]
fn accepts_structured_body_only_question_without_suggested_fix() {
    assert!(super::validate_review_templates(structured_question(), Some("{\"comments\":[]}")).is_ok());
}

#[test]
fn rejects_unstructured_inline_comment() {
    let raw = serde_json::json!({ "comments": [{ "body": "Bug" }] }).to_string();
    assert!(super::validate_review_templates("Overview", Some(&raw)).is_err());
}

#[test]
fn accepts_structured_inline_comment() {
    let raw = serde_json::json!({ "comments": [{ "body": structured_finding() }] }).to_string();
    assert!(super::validate_review_templates("Overview", Some(&raw)).is_ok());
}

#[test]
fn accepts_structured_inline_question_without_suggested_fix() {
    let raw = serde_json::json!({ "comments": [{ "body": structured_question() }] }).to_string();
    assert!(super::validate_review_templates("Overview", Some(&raw)).is_ok());
}

#[test]
fn final_comment_validation_accepts_empty_output() {
    let validated = super::validate_final_comments("", "abc", "").expect("empty comments");
    let value: serde_json::Value = serde_json::from_str(&validated).expect("json");
    assert_eq!(value.get("commit_id").and_then(|v| v.as_str()), Some("abc"));
    assert_eq!(value.get("comments").and_then(|v| v.as_array()).unwrap().len(), 0);
}

#[test]
fn final_comment_validation_drops_duplicates_and_invalid_anchors() {
    let diff =
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,2 @@\n old\n+new\n";
    let raw = serde_json::json!({ "commit_id": "abc", "comments": [
        { "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": structured_finding() },
        { "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": structured_finding() },
        { "path": "src/lib.rs", "line": 1, "side": "RIGHT", "body": structured_question() },
        { "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": "Fixed in https://github.com/BridgLabs/opencodereview/commit/abc." }
    ]})
    .to_string();
    let validated = super::validate_final_comments(&raw, "abc", diff).expect("valid comments");
    let value: serde_json::Value = serde_json::from_str(&validated).expect("json");
    let comments = value.get("comments").and_then(|v| v.as_array()).unwrap();

    assert_eq!(comments.len(), 1);
    assert_eq!(
        comments[0].get("body").and_then(|v| v.as_str()),
        Some(structured_finding())
    );
}

#[test]
fn chunk_findings_fallback_dedupes_cross_chunk_findings() {
    let finding = serde_json::json!({ "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": structured_finding() });
    let other = serde_json::json!({ "path": "src/lib.rs", "line": 5, "side": "RIGHT", "body": structured_finding() });
    // Same finding surfaced by two chunks (should collapse) plus a distinct
    // one at another line (should survive).
    let merged = serde_json::json!({ "commit_id": "abc", "comments": [
        { "chunkId": 0, "comment": finding.clone() },
        { "chunkId": 1, "comment": finding },
        { "chunkId": 1, "comment": other },
    ]});
    let flattened = super::chunk_findings_as_comments(&merged, "abc");
    let value: serde_json::Value = serde_json::from_str(&flattened).expect("json");
    let comments = value.get("comments").and_then(|v| v.as_array()).unwrap();
    assert_eq!(comments.len(), 2, "cross-chunk duplicate collapsed, distinct kept");
}

#[test]
fn chunk_findings_fallback_drops_unstructured_bodies() {
    // A free-form chunk body must be dropped rather than aborting the whole
    // fallback (validate_final_comments hard-errors on finding-like untemplated
    // bodies), so the valid structured finding still survives.
    let merged = serde_json::json!({ "commit_id": "abc", "comments": [
        { "chunkId": 0, "comment": { "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": structured_finding() } },
        { "chunkId": 1, "comment": { "path": "src/lib.rs", "line": 3, "side": "RIGHT", "body": "Bug: this can fail on stale state." } },
    ]});
    let flattened = super::chunk_findings_as_comments(&merged, "abc");
    // The fallback output must validate without error and keep the good one.
    let diff = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,3 @@\n old\n+new\n+more\n";
    let validated = super::validate_final_comments(&flattened, "abc", diff).expect("no hard error");
    let value: serde_json::Value = serde_json::from_str(&validated).expect("json");
    let comments = value.get("comments").and_then(|v| v.as_array()).unwrap();
    assert_eq!(comments.len(), 1, "unstructured body dropped, structured finding kept");
}

#[test]
fn final_comment_validation_rejects_unstructured_findings() {
    let diff =
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,2 @@\n old\n+new\n";
    let raw = serde_json::json!({ "commit_id": "abc", "comments": [{ "path": "src/lib.rs", "line": 2, "side": "RIGHT", "body": "Bug: this can fail on stale state." }] }).to_string();
    assert!(super::validate_final_comments(&raw, "abc", diff).is_err());
}

#[test]
fn sanitize_review_comments_rejects_unstructured_findings() {
    let raw = serde_json::json!({ "comments": [{ "body": "Bug: this can fail on stale state." }] }).to_string();
    assert!(super::sanitize_review_comments(Some(&raw)).is_err());
}

#[test]
fn sanitize_review_comments_drops_unstructured_replies() {
    let raw = serde_json::json!({ "comments": [
        { "body": "Fixed in https://github.com/BridgLabs/opencodereview/commit/abc." },
        { "body": structured_finding() }
    ]})
    .to_string();
    let sanitized = super::sanitize_review_comments(Some(&raw))
        .expect("ok")
        .expect("sanitized");
    let value: serde_json::Value = serde_json::from_str(&sanitized).expect("json");
    let comments = value.get("comments").and_then(|v| v.as_array()).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(
        comments[0].get("body").and_then(|v| v.as_str()),
        Some(structured_finding())
    );
}
