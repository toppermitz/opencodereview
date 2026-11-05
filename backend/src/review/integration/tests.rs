use super::*;
use crate::review::usage_tests::review_inputs;
use std::sync::atomic::AtomicBool;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const MERGE_BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";
const LINE: &str = "let value = 1;\n";

fn archive(files: &[(String, String)]) -> crate::review_archive::Archive {
    let scope = crate::review_archive::Scope {
        host: "github.com".into(),
        repository: "acme/fixture".into(),
        head_sha: HEAD.into(),
        base_sha: BASE.into(),
        merge_base_sha: MERGE_BASE.into(),
        prior_sha: None,
    };
    let contents: std::collections::HashMap<String, Vec<u8>> = files
        .iter()
        .map(|(path, content)| (path.clone(), content.clone().into_bytes()))
        .collect();
    let sources = files
        .iter()
        .map(|(path, content)| crate::review_archive::SourceEntry {
            path: path.clone(),
            revision: HEAD.into(),
            mode: "100644".into(),
            git_blob_id: "d".repeat(40),
            bytes: content.len() as u64,
        })
        .collect();
    crate::review_archive::Archive::build(scope, sources, &AtomicBool::new(false), move |entry| {
        Ok(contents[entry.path.as_str()].clone())
    })
    .unwrap()
}

fn frozen_inputs(files: &[(String, String)]) -> ReviewInputs {
    let mut inputs = review_inputs("deep", 20, 5);
    inputs.frozen = Some(std::sync::Arc::new(frozen::FrozenContext::new(archive(files))));
    inputs
}

fn added_diff(path: &str) -> String {
    format!("diff --git a/{path} b/{path}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+{LINE}")
}

fn fixture(id: i64, inputs: &ReviewInputs, files: &[String]) -> (PathBuf, PathBuf, ReviewMap, String) {
    let root = make_run_dir(id).unwrap();
    let context = root.join("context");
    fs::create_dir_all(context.join("skill")).unwrap();
    fs::create_dir_all(context.join("conventions")).unwrap();
    fs::write(context.join("PR.md"), "SENTINEL-PR\n").unwrap();
    fs::write(context.join("skill/SKILL.md"), "SENTINEL-SKILL\n").unwrap();
    fs::write(context.join("conventions/AGENTS.md"), "SENTINEL-AGENTS\n").unwrap();
    fs::write(context.join("conventions/README.md"), "SENTINEL-README\n").unwrap();
    let diff = files.iter().map(|path| added_diff(path)).collect::<String>();
    fs::write(context.join("PR_DIFF.patch"), &diff).unwrap();
    let plan = crate::db::ReviewContextPlan {
        repo_id: 1,
        base_ref: None,
        base_sha: None,
        changed_files: files.to_vec(),
        reviewed_base_sha: None,
        merge_base_sha: None,
        scope_reason: None,
        scope_selection: None,
        dependency_edges: vec![],
        packages: vec![],
        dependent_packages: vec![],
        related_files: vec![],
        test_files: vec![],
        convention_files: vec!["AGENTS.md".into(), "README.md".into()],
        omitted_count: 0,
        incremental: false,
        range: String::new(),
        head_sha: None,
        prior_sha: None,
    };
    let map = build_review_map(inputs, &plan, HEAD, files, &diff);
    (root, context, map, diff)
}

fn point(path: &str, title: &str, detail: &str) -> Value {
    let reference = json!({"path": path, "revision": HEAD, "line": 1, "quote": "let value = 1;"});
    json!({"kind":"finding","title":title,"detail":detail,"action":"Keep the guard","path":path,"line":1,"side":"RIGHT","evidence":[reference.clone()],"counterEvidence":[reference]})
}

fn snapshot_id(inputs: &ReviewInputs) -> String {
    inputs.frozen.as_ref().unwrap().archive.descriptor().id
}

#[test]
fn mixed_frozen_review_omits_policy_excluded_paths_from_assigned_scope() {
    let files = vec!["src/media.ts".to_string(), "test/files/sample-video.mp4".to_string()];
    let inputs = frozen_inputs(
        &files
            .iter()
            .map(|path| (path.clone(), LINE.to_string()))
            .collect::<Vec<_>>(),
    );
    let (root, context, map, diff) = fixture(-22050, &inputs, &files);
    assert_eq!(map.changed_files, files);
    assert_eq!(map.reviewable_files, vec!["src/media.ts"]);
    assert_eq!(map.omitted_noise_files, vec!["test/files/sample-video.mp4"]);
    frozen::prepare_stage(&inputs, &context, &map.reviewable_files, &diff, MERGE_BASE).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn mixed_frozen_review_omits_oversized_assets_from_assigned_scope() {
    let files = vec!["src/media.ts".to_string(), "assets/scene.glb".to_string()];
    let inputs = frozen_inputs(&[
        (files[0].clone(), LINE.to_string()),
        (files[1].clone(), "x".repeat(1_000_001)),
    ]);
    let (root, context, map, diff) = fixture(-22070, &inputs, &files);
    assert_eq!(map.reviewable_files, vec!["src/media.ts"]);
    assert_eq!(map.omitted_noise_files, vec!["assets/scene.glb"]);
    frozen::prepare_stage(&inputs, &context, &map.reviewable_files, &diff, MERGE_BASE).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn all_excluded_frozen_review_paths_are_never_assigned() {
    for files in [
        vec!["test/files/sample-video.mp4".to_string()],
        vec!["package-lock.json".to_string(), "go.sum".to_string()],
        vec!["vendor/lib/x.go".to_string(), "api/service.pb.go".to_string()],
    ] {
        let inputs = frozen_inputs(
            &files
                .iter()
                .map(|path| (path.clone(), LINE.to_string()))
                .collect::<Vec<_>>(),
        );
        let (root, _, map, _) = fixture(-22060, &inputs, &files);
        assert_eq!(map.omitted_noise_files, files);
        assert!(map.reviewable_files.is_empty(), "{files:?}");
        assert!(map.risk_triggers.is_empty(), "{:?}", map.risk_triggers);
        // Execution skips such a pull request before inference instead of failing preflight.
        let excluded = frozen_unreviewable_paths(&inputs, &files);
        assert!(files.iter().all(|path| excluded.contains(path)), "{files:?}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn offline_submodules_and_symlinks_are_never_skipped_as_unreviewable() {
    let files = vec!["libs/vendored-sdk".to_string()];
    let mut inputs = frozen_inputs(&[("src/a.rs".to_string(), LINE.to_string())]);
    inputs.offline = true;
    let scope = inputs.frozen.as_ref().unwrap().archive.scope().clone();
    let submodule = crate::review_archive::SourceEntry {
        path: files[0].clone(),
        revision: HEAD.into(),
        mode: "160000".into(),
        git_blob_id: "e".repeat(40),
        bytes: 0,
    };
    let archive =
        crate::review_archive::Archive::build(scope, vec![submodule.clone()], &AtomicBool::new(false), |_| {
            panic!("A submodule has no content to open")
        })
        .unwrap();
    inputs.frozen = Some(std::sync::Arc::new(frozen::FrozenContext::new(archive)));
    assert!(frozen_omittable_paths(&inputs, &files).contains(&files[0]));
    assert!(frozen_unreviewable_paths(&inputs, &files).is_empty());
}

#[test]
fn lockfiles_are_left_out_of_a_mixed_review_and_its_risk_triggers() {
    let files = vec!["src/media.ts".to_string(), "package-lock.json".to_string()];
    let inputs = frozen_inputs(
        &files
            .iter()
            .map(|path| (path.clone(), LINE.to_string()))
            .collect::<Vec<_>>(),
    );
    let (root, context, map, diff) = fixture(-22061, &inputs, &files);
    assert_eq!(map.reviewable_files, vec!["src/media.ts"]);
    assert!(!map
        .risk_triggers
        .iter()
        .any(|trigger| trigger.contains("package-lock.json")));
    frozen::prepare_stage(&inputs, &context, &map.reviewable_files, &diff, MERGE_BASE).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn private_artifacts_are_omitted_from_bound_diffs_and_report_coverage() {
    let files = vec!["src/media.ts".to_string(), ".windsurf/settings.local.json".to_string()];
    let inputs = frozen_inputs(
        &files
            .iter()
            .map(|path| (path.clone(), LINE.to_string()))
            .collect::<Vec<_>>(),
    );
    let (root, context, map, diff) = fixture(-22102, &inputs, &files);
    fs::write(context.join("PUBLICATION_DIFF.patch"), &diff).unwrap();
    frozen::prepare_stage(&inputs, &context, &map.reviewable_files, &diff, MERGE_BASE).unwrap();
    assert_eq!(map.changed_files, files);
    assert_eq!(map.reviewable_files, vec!["src/media.ts"]);
    assert_eq!(map.omitted_noise_files, vec![files[1].clone()]);
    for name in ["PR_DIFF.patch", "PUBLICATION_DIFF.patch"] {
        assert_eq!(fs::read_to_string(context.join(name)).unwrap(), added_diff(&files[0]));
    }
    let report = root.join("report.json");
    fs::write(&report, json!({"contractVersion":2,"snapshotId":snapshot_id(&inputs),"reviewedFiles":map.reviewable_files,"missingEvidence":[],"findings":[point(&files[0], "Fixture", "Inspect the value")]}).to_string()).unwrap();
    frozen::materialize(
        &inputs,
        &report,
        &context,
        &map.reviewable_files,
        HEAD,
        &diff,
        &root.join("summary.md"),
        &root.join("comments.json"),
    )
    .unwrap();
    assert!(frozen::materialize(
        &inputs,
        &report,
        &context,
        &map.reviewable_files,
        HEAD,
        &diff.replace("+let value = 1;", "+let value = 2;"),
        &root.join("summary.md"),
        &root.join("comments.json")
    )
    .unwrap_err()
    .contains("assigned scope differs"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn private_artifacts_only_keep_explicit_preflight_failure() {
    for (index, path, reason) in [
        (0, ".windsurf/settings.local.json", "PrivateConfiguration"),
        (1, ".env.test", "PrivatePath"),
    ] {
        let files = vec![path.to_string()];
        let inputs = frozen_inputs(&[(path.to_string(), LINE.to_string())]);
        let (root, context, map, diff) = fixture(-22100 - index, &inputs, &files);
        assert_eq!(map.changed_files, files);
        assert_eq!(map.omitted_noise_files, files);
        assert_eq!(map.reviewable_files, files);
        assert!(
            frozen_unreviewable_paths(&inputs, &files).is_empty(),
            "{path} is never skipped"
        );
        assert_eq!(
            frozen::prepare_stage(&inputs, &context, &map.reviewable_files, &diff, MERGE_BASE).unwrap_err(),
            format!("Evidence path is excluded by policy: {reason}")
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn frozen_evidence_accepts_prepared_convention_paths_with_context_prefix() {
    let files = vec!["src/media.ts".to_string()];
    let inputs = frozen_inputs(
        &files
            .iter()
            .map(|path| (path.clone(), LINE.to_string()))
            .collect::<Vec<_>>(),
    );
    let (root, context, map, diff) = fixture(-22070, &inputs, &files);
    frozen::prepare_stage(&inputs, &context, &map.reviewable_files, &diff, MERGE_BASE).unwrap();
    let code = json!({"path":"src/media.ts","revision":HEAD,"line":1,"quote":"let value = 1;"});
    let convention = json!({"path":"conventions/AGENTS.md","revision":BASE,"source":"convention","line":1,"quote":"SENTINEL-AGENTS"});
    let report = json!({"contractVersion":2,"snapshotId":snapshot_id(&inputs),"reviewedFiles":files,"missingEvidence":[],"findings":[{"kind":"finding","title":"Convention boundary","detail":"The changed code needs review","action":"Keep the guard","path":"src/media.ts","line":1,"side":"RIGHT","evidence":[code.clone(),code.clone(),code,convention],"counterEvidence":[{"path":"src/media.ts","revision":HEAD,"line":1,"quote":"let value = 1;"}]}]});
    let report_path = root.join("report.json");
    let summary = root.join("summary.md");
    let comments = root.join("comments.json");
    fs::write(&report_path, report.to_string()).unwrap();
    frozen::materialize(
        &inputs,
        &report_path,
        &context,
        &map.reviewable_files,
        HEAD,
        &diff,
        &summary,
        &comments,
    )
    .unwrap();
    assert!(comments.is_file());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn frozen_prompt_marks_unavailable_optional_context() {
    let files = vec![("src/media.ts".to_string(), LINE.to_string())];
    let inputs = frozen_inputs(&files);
    let root = make_run_dir(-22080).unwrap();
    let context = root.join("context");
    fs::create_dir_all(&context).unwrap();
    let prompt = frozen::prompt(
        &inputs,
        &context,
        &["src/media.ts".to_string()],
        &root.join("report.json"),
    );
    assert!(prompt.contains(
        "Unavailable optional context files in this stage: PENDING_REVIEW.md. Never cite an unavailable file."
    ));
    assert!(prompt.contains("Convention evidence may cite only repository-relative paths present under"));
    assert!(prompt.contains("do not prefix them with `context/` or `conventions/`"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn frozen_evidence_rejects_unavailable_optional_context() {
    let files = vec!["src/media.ts".to_string()];
    let inputs = frozen_inputs(&[(files[0].clone(), LINE.to_string())]);
    let (root, context, map, diff) = fixture(-22090, &inputs, &files);
    frozen::prepare_stage(&inputs, &context, &map.reviewable_files, &diff, MERGE_BASE).unwrap();
    let code = json!({"path":"src/media.ts","revision":HEAD,"line":1,"quote":"let value = 1;"});
    let missing = json!({"path":"PENDING_REVIEW.md","line":1,"quote":"pending"});
    let report = json!({"contractVersion":2,"snapshotId":snapshot_id(&inputs),"reviewedFiles":files,"missingEvidence":[],"findings":[{"kind":"finding","title":"Missing context","detail":"The changed code needs review","action":"Keep the guard","path":"src/media.ts","line":1,"side":"RIGHT","evidence":[code.clone(),code.clone(),code,missing],"counterEvidence":[{"path":"src/media.ts","revision":HEAD,"line":1,"quote":"let value = 1;"}]}]});
    let report_path = root.join("report.json");
    fs::write(&report_path, report.to_string()).unwrap();
    let error = frozen::materialize(
        &inputs,
        &report_path,
        &context,
        &map.reviewable_files,
        HEAD,
        &diff,
        &root.join("summary.md"),
        &root.join("comments.json"),
    )
    .unwrap_err();
    assert_eq!(
        error,
        "Incomplete review: code evidence requires an explicit frozen revision [findings[0].evidence[3]]"
    );
    fs::remove_dir_all(root).unwrap();
}

fn chunk_result(
    inputs: &ReviewInputs,
    root: &Path,
    id: i64,
    path: &str,
    findings: Vec<Value>,
    questions: Vec<Value>,
    context_questions: Vec<String>,
) -> ChunkResult {
    let directory = root.join(format!("chunks/{id:03}"));
    let chunk_context = directory.join("context");
    fs::create_dir_all(chunk_context.join("skill")).unwrap();
    fs::write(chunk_context.join("PR.md"), "SENTINEL-PR\n").unwrap();
    fs::write(chunk_context.join("skill/SKILL.md"), "SENTINEL-SKILL\n").unwrap();
    let diff = added_diff(path);
    fs::write(chunk_context.join("CHUNK_SCOPE"), CHUNK_SCOPE_NOTE).unwrap();
    inputs
        .frozen
        .as_ref()
        .unwrap()
        .archive
        .prepare_stage(&chunk_context, &[path.to_string()], &diff, MERGE_BASE)
        .unwrap();
    let report = json!({"contractVersion":2,"snapshotId":snapshot_id(inputs),"reviewedFiles":[path],"missingEvidence":[],"findings":findings,"questions":questions,"contextQuestions":context_questions});
    fs::write(directory.join("report.json"), report.to_string()).unwrap();
    let summary = directory.join("summary.md");
    let comments = directory.join("comments.json");
    frozen::materialize(
        inputs,
        &directory.join("report.json"),
        &chunk_context,
        &[path.to_string()],
        HEAD,
        &diff,
        &summary,
        &comments,
    )
    .unwrap();
    ChunkResult {
        evidence_warnings: Vec::new(),
        cached: false,
        chunk: ReviewChunk {
            id: id as usize,
            files: vec![path.to_string()],
            related_files: vec![],
            global_context_files: vec![],
            omitted_convention_files: vec![],
            risk_triggers: vec![],
            cross_chunk_group_ids: vec![],
        },
        summary: report.to_string(),
        comments_json: Some(fs::read_to_string(comments).unwrap()),
        usage: RunUsage::default(),
    }
}

#[test]
fn diagnostic_restore_preserves_chunk_warnings_when_final_has_none() {
    let files = vec!["src/value.rs".to_string()];
    let mut inputs = frozen_inputs(&[(files[0].clone(), LINE.into())]);
    let (root, context, map, diff) = fixture(-1242, &inputs, &files);
    let db_path = root.join("diagnostic.sqlite");
    inputs.db_handle = Some(crate::db::Db::for_test_at(db_path.clone()));
    inputs.archive_dir = root.join("review-archives");
    let db = inputs.open_db().unwrap();
    db.ensure_temp_repo_for_index(1, "acme/fixture").unwrap();
    let ledger = crate::db::Db::for_test_at(&db_path).open().unwrap();
    ledger.execute("INSERT INTO pull_requests(id,repo_id,number,title,author,head_sha,last_synced_at,created_at,updated_at) VALUES(1,1,1,'Fixture','fixture',$1,0,0,0)", [HEAD]).unwrap();
    ledger
        .execute(
            "INSERT INTO review_runs(id,pr_id,head_sha,status,created_at,updated_at) VALUES(1,1,$1,'running',0,0)",
            [HEAD],
        )
        .unwrap();

    let chunk = chunk_result(&inputs, &root, 1, &files[0], vec![], vec![], vec![]).chunk;
    let directory = root.join("chunks/001");
    let report_path = directory.join("report.json");
    let mut report: Value = serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    report["missingEvidence"] = json!([{"reason":"insufficient_evidence","path":files[0]}]);
    fs::write(&report_path, report.to_string()).unwrap();
    let validation = frozen::materialize(
        &inputs,
        &report_path,
        &directory.join("context"),
        &chunk.files,
        HEAD,
        &diff,
        &directory.join("summary.md"),
        &directory.join("comments.json"),
    );
    let restored =
        super::diagnostics::restore_chunk(&chunk, &directory, validation, true, RunUsage::default()).unwrap();
    let expected = vec![crate::review::warnings::EvidenceWarning {
        reason: "insufficient_evidence".into(),
        path: Some(files[0].clone()),
        reference: None,
    }];
    assert_eq!(restored.evidence_warnings, expected);
    assert!(restored.cached);

    let results = vec![restored];
    let payload = Payload::from_results(&results, merge_chunk_findings(&results, HEAD)).unwrap();
    db.store_finding_audit(
        inputs.run_id,
        &crate::review::findings::seed_audit(&payload.sources, HEAD, &diff),
    )
    .unwrap();
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    let outputs = stages
        .iter()
        .map(|stage| {
            fake_final(&inputs, stage, &payload);
            validate_output(&inputs, stage, &payload, HEAD).unwrap()
        })
        .collect();
    let (body, comments, evidence_warnings) =
        combine(&inputs, &payload, outputs, &files, HEAD, &diff, &root.join("merge")).unwrap();
    assert!(evidence_warnings.is_empty());
    let mut output = ReviewOutput {
        body,
        comments_json: Some(comments),
        evidence_warnings,
        usage: RunUsage::default(),
        risk: empty_review_risk(),
        no_new_changes: false,
    };
    super::diagnostics::retain_chunk_warnings(&mut output, &results);
    super::diagnostics::retain_chunk_warnings(&mut output, &results);
    assert_eq!(output.evidence_warnings, expected);
    assert_eq!(output.body, crate::review::NO_FINDINGS_SUMMARY);

    let comments = output.comments_json.as_deref().unwrap();
    let audit =
        super::diagnostics::finish_audit(&inputs, &root, &output.body, comments, &diff, &output.evidence_warnings)
            .unwrap();
    assert!(audit.complete);
    assert_eq!(audit.evidence_warnings, Some(expected.clone()));
    assert_eq!(
        db.load_finding_audit(inputs.run_id).unwrap().unwrap().evidence_warnings,
        Some(expected.clone())
    );
    let verified = super::diagnostics::finish_audit(&inputs, &root, &output.body, comments, &diff, &[]).unwrap();
    assert_eq!(verified.evidence_warnings, Some(expected));
    assert_eq!(
        serde_json::to_value(verified).unwrap()["evidenceWarnings"][0]["path"],
        files[0]
    );

    report["reviewedFiles"] = json!([]);
    fs::write(&report_path, report.to_string()).unwrap();
    let invalid = frozen::materialize(
        &inputs,
        &report_path,
        &directory.join("context"),
        &chunk.files,
        HEAD,
        &diff,
        &directory.join("summary.md"),
        &directory.join("comments.json"),
    );
    assert!(
        super::diagnostics::restore_chunk(&chunk, &directory, invalid, true, RunUsage::default())
            .err()
            .unwrap()
            .contains("unreviewed assigned files: 1")
    );
    drop(ledger);
    drop(db);
    fs::remove_dir_all(root).unwrap();
}

fn fake_final(inputs: &ReviewInputs, stage: &Prepared, payload: &Payload) {
    let snapshot = snapshot_id(inputs);
    let findings: Vec<Value> = stage
        .candidate_ids
        .iter()
        .map(|id| payload.candidates.get(id).unwrap()["point"].clone())
        .collect();
    fs::write(
        stage.directory.join("report.json"),
        json!({
            "contractVersion":2,"snapshotId":snapshot,"reviewedFiles":stage.files,
            "missingEvidence":[],"findings":findings,"questions":[],"contextQuestions":[]
        })
        .to_string(),
    )
    .unwrap();
    let decisions: Vec<Value> = stage
        .candidate_ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            json!({
                "candidateId":id,"decision":"keep","commentIndex":index,"reason":"Verified fixture evidence"
            })
        })
        .collect();
    fs::write(
        stage.directory.join("finding_decisions.json"),
        json!({
            "contractVersion":2,"snapshotId":snapshot,"decisions":decisions
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn adaptive_integration_partitions_large_context_and_preserves_coverage() {
    let files: Vec<String> = (0..20).map(|i| format!("unit-{i:03}.rs")).collect();
    let inputs = frozen_inputs(
        &files
            .iter()
            .map(|f| (f.clone(), "x".repeat(450_000)))
            .collect::<Vec<_>>(),
    );
    let (root, context, map, diff) = fixture(-1201, &inputs, &files);
    assert!(files.iter().map(|_| 450_000usize).sum::<usize>() > crate::review::policy::MAX_EVIDENCE_BYTES);
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("integration-static"),
        &Payload::empty(HEAD),
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        stages.len() > 1,
        "oversized shared evidence must produce multiple final batches"
    );
    assert_eq!(
        stages
            .iter()
            .flat_map(|stage| stage.files.clone())
            .collect::<BTreeSet<_>>(),
        files.iter().cloned().collect()
    );
    for stage in stages {
        let request = crate::sandbox::runner::stage_request(
            &inputs.model,
            "offline",
            1,
            "claude",
            &root,
            &stage.directory,
            &stage.context,
            &stage.prompt,
        )
        .unwrap();
        if let crate::sandbox::runner::Request::Run { files: sent, .. } = request {
            inputs
                .frozen
                .as_ref()
                .unwrap()
                .archive
                .validate_stage_files(&sent)
                .unwrap();
        } else {
            panic!("integration stage must be a run request");
        }
        for path in &stage.files {
            let expected = inputs.frozen.as_ref().unwrap().archive.resolve(HEAD, path).unwrap();
            assert_eq!(
                fs::read_to_string(stage.context.join("changed").join(path)).unwrap(),
                expected
            );
        }
        assert_eq!(
            fs::read_to_string(stage.context.join("PR.md")).unwrap(),
            "SENTINEL-PR\n"
        );
        assert_eq!(
            fs::read_to_string(stage.context.join("skill/SKILL.md")).unwrap(),
            "SENTINEL-SKILL\n"
        );
        assert_eq!(
            fs::read_to_string(stage.context.join("conventions/AGENTS.md")).unwrap(),
            "SENTINEL-AGENTS\n"
        );
        assert_eq!(
            fs::read_to_string(stage.context.join("conventions/README.md")).unwrap(),
            "SENTINEL-README\n"
        );
        assert!(!stage.context.join("CHUNK_SCOPE").exists());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn adaptive_integration_splits_dynamic_candidates_without_losing_ids() {
    let files: Vec<String> = (0..3).map(|i| format!("src/file-{i}.rs")).collect();
    let inputs = frozen_inputs(&files.iter().map(|f| (f.clone(), LINE.to_string())).collect::<Vec<_>>());
    let (root, context, map, diff) = fixture(-1202, &inputs, &files);
    let detail = "Inspect the value".to_string() + &"d".repeat(25_000);
    let results: Vec<ChunkResult> = files.iter().enumerate().map(|(index, path)| chunk_result(
        &inputs, &root, (index + 1) as i64, path,
        vec![point(path, &format!("Issue {}", index), &detail)],
        vec![json!({"title":"Confirm rollout","detail":"Delegated elsewhere","action":"Confirm the order","evidence":[{"path":path,"revision":HEAD,"line":1,"quote":"let value = 1;"}],"counterEvidence":[{"path":path,"revision":HEAD,"line":1,"quote":"let value = 1;"}]})],
        vec![format!("context-check-{index}")],
    )).collect();
    let sources = merge_chunk_findings(&results, HEAD);
    let payload = Payload::from_results(&results, sources).unwrap();
    let expected: Vec<String> = payload
        .originals()
        .iter()
        .map(|c| c["candidateId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(expected.len(), 3);
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        40_000,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        stages.len() > 1,
        "combined candidate payload above the limit must split"
    );
    assert_eq!(
        stages
            .iter()
            .flat_map(|stage| stage.candidate_ids.clone())
            .collect::<Vec<_>>(),
        expected
    );
    for stage in &stages {
        let findings: Value =
            serde_json::from_str(&fs::read_to_string(stage.directory.join("chunk_findings.json")).unwrap()).unwrap();
        for candidate in findings["comments"].as_array().unwrap() {
            let point = &candidate["point"];
            assert_eq!(point["detail"].as_str().unwrap(), detail);
            assert_eq!(point["evidence"][0]["quote"], "let value = 1;");
            assert_eq!(point["counterEvidence"][0]["revision"], HEAD);
            assert!(point["title"].as_str().unwrap().starts_with("Issue"));
        }
        let notes = fs::read_to_string(stage.directory.join("chunk_summaries.md")).unwrap();
        assert!(
            notes.contains("context-check-"),
            "chunk contextQuestions must survive in notes"
        );
        assert!(
            notes.contains("Confirm rollout"),
            "general questions must survive in notes"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn adaptive_integration_validates_batches_and_remaps_decisions() {
    let files: Vec<String> = (0..3).map(|i| format!("src/file-{i}.rs")).collect();
    let inputs = frozen_inputs(&files.iter().map(|f| (f.clone(), LINE.to_string())).collect::<Vec<_>>());
    let (root, context, map, diff) = fixture(-1203, &inputs, &files);
    let results: Vec<ChunkResult> = files
        .iter()
        .enumerate()
        .map(|(index, path)| {
            chunk_result(
                &inputs,
                &root,
                (index + 1) as i64,
                path,
                vec![point(
                    path,
                    &format!("Issue {}", index),
                    &format!("Inspect the value{}", "d".repeat(30_000)),
                )],
                vec![],
                vec![],
            )
        })
        .collect();
    let sources = merge_chunk_findings(&results, HEAD);
    let payload = Payload::from_results(&results, sources).unwrap();
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        40_000,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(stages.len() > 1);
    let mut outputs = Vec::new();
    for (index, stage) in stages.iter().enumerate() {
        fake_final(&inputs, stage, &payload);
        if index == 0 {
            let path = stage.directory.join("report.json");
            let mut report: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            report["missingEvidence"] = json!([{"reason":"insufficient_evidence","path":stage.files[0]}]);
            fs::write(path, report.to_string()).unwrap();
        }
        outputs.push(validate_output(&inputs, stage, &payload, HEAD).unwrap());
    }
    let merge = root.join("merge");
    let (body, comments, evidence_warnings) =
        combine(&inputs, &payload, outputs, &map.reviewable_files, HEAD, &diff, &merge).unwrap();
    assert_eq!(evidence_warnings.len(), 1);
    assert_eq!(evidence_warnings[0].path.as_deref(), Some(stages[0].files[0].as_str()));
    let merged: Value = serde_json::from_str(&comments).unwrap();
    let merged_comments = merged["comments"].as_array().unwrap();
    assert_eq!(merged_comments.len(), 3, "all batch comments must be preserved");
    assert_eq!(
        merged_comments
            .iter()
            .map(|c| c["path"].as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>(),
        files.iter().cloned().collect()
    );
    let decisions = fs::read_to_string(merge.join("finding_decisions.json")).unwrap();
    crate::review::findings::validate_decisions(&payload.sources, Some(&decisions), &comments).unwrap();
    let entries = serde_json::from_str::<Value>(&decisions).unwrap();
    let entries = entries["decisions"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    let kept: Vec<usize> = entries
        .iter()
        .map(|e| e["commentIndex"].as_u64().unwrap() as usize)
        .collect();
    assert_eq!(
        kept.iter().copied().collect::<BTreeSet<_>>().len(),
        3,
        "local index 0 in every batch must remap to a distinct global index"
    );
    for (entry, index) in entries.iter().zip(&kept) {
        assert_eq!(entry["decision"], "keep");
        let candidate = payload
            .originals()
            .iter()
            .find(|c| c["candidateId"] == entry["candidateId"])
            .unwrap();
        assert_eq!(
            merged_comments[*index]["path"], candidate["comment"]["path"],
            "remapped index must point at the candidate's own comment"
        );
    }
    assert!(
        body.contains("finding") || body.contains("comment"),
        "combined body reports results"
    );
    fs::remove_dir_all(&merge).unwrap();

    let mut outputs = Vec::new();
    for stage in &stages[..stages.len() - 1] {
        outputs.push(validate_output(&inputs, stage, &payload, HEAD).unwrap());
    }
    assert!(
        combine(&inputs, &payload, outputs, &map.reviewable_files, HEAD, &diff, &merge).is_err(),
        "a missing batch must not produce a clean result"
    );

    let stage = stages.last().unwrap();
    fs::write(
        stage.directory.join("finding_decisions.json"),
        json!({
            "contractVersion":2,"snapshotId":snapshot_id(&inputs),
            "decisions":[{"candidateId":"unknown-candidate","decision":"keep","commentIndex":0,"reason":"fabricated"}]
        })
        .to_string(),
    )
    .unwrap();
    // A fabricated decision adds nothing: every real candidate keeps its chunk-validated comment,
    // marked unconfirmed.
    let output = validate_output(&inputs, stage, &payload, HEAD).unwrap();
    assert_eq!(output.decisions.len(), stage.candidate_ids.len());
    assert!(output
        .decisions
        .iter()
        .all(|decision| decision["candidateId"] != "unknown-candidate"));
    assert!(output
        .evidence_warnings
        .iter()
        .any(|warning| warning.reason == "unconfirmed_candidate"));
    let duplicate: Vec<Value> = stage
        .candidate_ids
        .iter()
        .flat_map(|id| {
            vec![
                json!({"candidateId":id,"decision":"keep","commentIndex":0,"reason":"Verified fixture evidence"}),
                json!({"candidateId":id,"decision":"keep","commentIndex":0,"reason":"Verified fixture evidence"}),
            ]
        })
        .collect();
    fs::write(
        stage.directory.join("finding_decisions.json"),
        json!({
            "contractVersion":2,"snapshotId":snapshot_id(&inputs),"decisions":duplicate
        })
        .to_string(),
    )
    .unwrap();
    let output = validate_output(&inputs, stage, &payload, HEAD).unwrap();
    assert_eq!(
        output.decisions.len(),
        stage.candidate_ids.len(),
        "duplicate decisions collapse to one per candidate"
    );
    let mut tampered: Vec<Value> = stage
        .candidate_ids
        .iter()
        .map(|id| payload.candidates.get(id).unwrap()["point"].clone())
        .collect();
    if let Some(point) = tampered.first_mut() {
        point["counterEvidence"] =
            json!([{"path":point["path"],"revision":HEAD,"line":1,"quote":"invented guard does not exist"}]);
    }
    fs::write(
        stage.directory.join("report.json"),
        json!({
            "contractVersion":2,"snapshotId":snapshot_id(&inputs),"reviewedFiles":stage.files,
            "missingEvidence":[],"findings":tampered,"questions":[],"contextQuestions":[]
        })
        .to_string(),
    )
    .unwrap();
    assert!(
        validate_output(&inputs, stage, &payload, HEAD).is_err(),
        "counter-evidence tampering must fail"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn adaptive_integration_retains_empty_chunk_business_notes() {
    let files: Vec<String> = (0..12).map(|i| format!("src/note-{i}.rs")).collect();
    let inputs = frozen_inputs(&files.iter().map(|f| (f.clone(), LINE.to_string())).collect::<Vec<_>>());
    let (root, context, map, diff) = fixture(-1204, &inputs, &files);
    let results: Vec<ChunkResult> = files.iter().enumerate().map(|(index, path)| {
        let report = json!({"contractVersion":2,"snapshotId":snapshot_id(&inputs),"reviewedFiles":[path],"missingEvidence":[],"findings":[],"questions":[],"contextQuestions":[format!("sentinel-{index}-{}", "n".repeat(900_000))]});
        ChunkResult {
            evidence_warnings: Vec::new(), cached: false,
            chunk: ReviewChunk { id: index + 1, files: vec![path.clone()], related_files: vec![], global_context_files: vec![], omitted_convention_files: vec![], risk_triggers: vec![], cross_chunk_group_ids: vec![] },
            summary: report.to_string(),
            comments_json: Some(format!(r#"{{"commit_id":"{HEAD}","comments":[]}}"#)),
            usage: RunUsage::default(),
        }
    }).collect();
    let sources = merge_chunk_findings(&results, HEAD);
    let payload = Payload::from_results(&results, sources).unwrap();
    assert!(payload.candidates.is_empty());
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(
        stages.len() > 1,
        "business notes above the evidence limit still require integration batches"
    );
    assert_eq!(
        stages
            .iter()
            .flat_map(|stage| stage.files.clone())
            .collect::<BTreeSet<_>>(),
        files.iter().cloned().collect()
    );
    let notes: String = stages
        .iter()
        .map(|stage| fs::read_to_string(stage.directory.join("chunk_summaries.md")).unwrap())
        .collect();
    for index in 0..12 {
        assert!(
            notes.contains(&format!("sentinel-{index}-")),
            "notes for chunk {index} dropped"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn integration_candidates_and_notes_are_readable_in_bounded_line_ranges() {
    let mut files = vec!["src/shared.rs".to_string()];
    files.extend((1..90).map(|index| format!("src/domain/integration/service-{index:03}.rs")));
    let inputs = frozen_inputs(&files.iter().map(|path| (path.clone(), LINE.into())).collect::<Vec<_>>());
    let (root, context, map, diff) = fixture(-9304, &inputs, &files);
    let points = (0..53)
        .map(|index| {
            point(
                &files[0],
                &format!("Candidate {index}"),
                &"Detailed evidence. ".repeat(20),
            )
        })
        .collect();
    let result = chunk_result(
        &inputs,
        &root,
        1,
        &files[0],
        points,
        vec![],
        vec!["Verify the documented boundary".into()],
    );
    let sources = merge_chunk_findings(std::slice::from_ref(&result), HEAD);
    let payload = Payload::from_results(&[result], sources).unwrap();
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(stages.len(), 1);
    for name in ["chunk_findings.json", "chunk_summaries.md", "context/STAGE.json"] {
        let raw = fs::read_to_string(stages[0].directory.join(name)).unwrap();
        assert!(
            raw.lines().count() > 1,
            "{name} must not be one truncated tool-output line"
        );
        assert!(
            raw.lines().all(|line| line.len() < 2000),
            "{name} has unreadable structured records"
        );
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        if name == "chunk_findings.json" {
            assert_eq!(parsed["comments"].as_array().unwrap().len(), 53);
            assert_eq!(parsed["comments"][52]["candidateId"], "candidate-53");
            assert_eq!(parsed["comments"][52]["point"]["title"], "Candidate 52");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "Requires a retained coordinator, frozen archive and read-only exported chunk cache; no inference"]
fn retained_final_inputs_preserve_readable_candidates_without_inference() {
    let source = PathBuf::from(std::env::var("OCR_DIAGNOSTIC_INTEGRATION").unwrap());
    let packet: Value = serde_json::from_slice(&fs::read(source.join("coordinator.json")).unwrap()).unwrap();
    let cached: Vec<Value> = serde_json::from_slice(&fs::read(source.join("cached-chunks.json")).unwrap()).unwrap();
    let chunks: Vec<ReviewChunk> = serde_json::from_value(packet["chunks"].clone()).unwrap();
    let map: ReviewMap = serde_json::from_value(packet["map"].clone()).unwrap();
    let archive = crate::review_archive::Archive::read(
        &source.join("snapshot.archive"),
        &serde_json::from_value(packet["descriptor"].clone()).unwrap(),
    )
    .unwrap();
    let mut inputs = review_inputs("deep", 20, 5);
    inputs.model = packet["fixture"]["model"].as_str().unwrap().into();
    inputs.fixed_head_sha = Some(map.head_sha.clone());
    inputs.frozen = Some(std::sync::Arc::new(frozen::FrozenContext::new(archive)));
    let root = make_run_dir(-1359).unwrap();
    assert_eq!(cached.len(), chunks.len());
    let results = cached
        .iter()
        .zip(chunks)
        .map(|(cached, chunk)| {
            assert_eq!(cached["run_id"], packet["runId"]);
            assert_eq!(cached["model"], inputs.model);
            assert_eq!(cached["head_sha"], map.head_sha);
            assert_eq!(
                serde_json::from_str::<Value>(cached["chunk_json"].as_str().unwrap()).unwrap(),
                serde_json::to_value(&chunk).unwrap()
            );
            let context = source.join(format!("prepared/chunks/{:03}/context", chunk.id));
            let directory = root.join(format!("chunk-{}", chunk.id));
            fs::create_dir(&directory).unwrap();
            let summary = cached["summary"].as_str().unwrap().to_string();
            fs::write(directory.join("report.json"), &summary).unwrap();
            frozen::materialize(
                &inputs,
                &directory.join("report.json"),
                &context,
                &chunk.files,
                &map.head_sha,
                &fs::read_to_string(context.join("PR_DIFF.patch")).unwrap(),
                &directory.join("summary.md"),
                &directory.join("comments.json"),
            )
            .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&fs::read_to_string(directory.join("comments.json")).unwrap()).unwrap(),
                serde_json::from_str::<Value>(cached["comments_json"].as_str().unwrap()).unwrap()
            );
            ChunkResult {
                evidence_warnings: Vec::new(),
                cached: true,
                chunk,
                summary,
                comments_json: Some(cached["comments_json"].as_str().unwrap().into()),
                usage: RunUsage::default(),
            }
        })
        .collect::<Vec<_>>();
    let payload = Payload::from_results(&results, merge_chunk_findings(&results, &map.head_sha)).unwrap();
    let stages = prepare(
        &inputs,
        &source.join("prepared/context"),
        &map,
        packet["fullDiff"].as_str().unwrap(),
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    for stage in &stages {
        let request = crate::sandbox::runner::stage_request(
            &inputs.model,
            "offline",
            1,
            "claude",
            &root,
            &stage.directory,
            &stage.context,
            &stage.prompt,
        )
        .unwrap();
        let crate::sandbox::runner::Request::Run { files, .. } = request else {
            panic!("Missing final request")
        };
        inputs
            .frozen
            .as_ref()
            .unwrap()
            .archive
            .validate_stage_files(&files)
            .unwrap();
        for name in [
            "integration/chunk_findings.json",
            "integration/chunk_summaries.md",
            "context/STAGE.json",
        ] {
            assert!(
                files[name].lines().all(|line| line.len() < 2000),
                "Unreadable final input: {name}"
            );
        }
        let candidates: Value = serde_json::from_str(&files["integration/chunk_findings.json"]).unwrap();
        for candidate in candidates["comments"].as_array().unwrap() {
            assert_eq!(
                candidate,
                &payload.candidates[candidate["candidateId"].as_str().unwrap()]
            );
        }
    }
    if std::env::var("OCR_DIAGNOSTIC_VALIDATE_FINAL").as_deref() == Ok("true") {
        assert_eq!(stages.len(), 1, "Single-execution diagnostic requires one final batch");
        let stage = &stages[0];
        for name in ["report.json", "finding_decisions.json"] {
            fs::copy(source.join(name), stage.directory.join(name)).unwrap();
        }
        let output = validate_output(&inputs, stage, &payload, &map.head_sha).unwrap();
        let (_, comments, _evidence_warnings) = combine(
            &inputs,
            &payload,
            vec![output],
            &map.reviewable_files,
            &map.head_sha,
            packet["fullDiff"].as_str().unwrap(),
            &root.join("merge"),
        )
        .unwrap();
        let count = serde_json::from_str::<Value>(&comments).unwrap()["comments"]
            .as_array()
            .unwrap()
            .len();
        println!(
            "VALIDATED_RETAINED_FINAL {}",
            json!({"runId":packet["runId"],"comments":count,"candidates":payload.candidates.len(),"modelCalls":0})
        );
    }
    println!(
        "RETAINED_FINAL_INPUTS {}",
        json!({"runId":packet["runId"],"chunks":results.len(),"files":map.reviewable_files.len(),"candidates":payload.candidates.len(),"batches":stages.len(),"modelCalls":0})
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn final_identical_duplicates_remap_original_decisions_without_losing_other_findings() {
    let files = vec!["src/shared.rs".to_string()];
    let inputs = frozen_inputs(&[(files[0].clone(), LINE.into())]);
    let (root, context, map, diff) = fixture(-9303, &inputs, &files);
    let result = chunk_result(
        &inputs,
        &root,
        1,
        &files[0],
        vec![
            point(&files[0], "Same title", "First defect"),
            point(&files[0], "Same title", "Second defect"),
        ],
        vec![],
        vec![],
    );
    let sources = merge_chunk_findings(std::slice::from_ref(&result), HEAD);
    let payload = Payload::from_results(&[result], sources).unwrap();
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    let stage = &stages[0];
    fake_final(&inputs, stage, &payload);
    let path = stage.directory.join("report.json");
    let mut report: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let duplicate = report["findings"][0].clone();
    report["findings"].as_array_mut().unwrap().insert(1, duplicate);
    fs::write(&path, report.to_string()).unwrap();
    let path = stage.directory.join("finding_decisions.json");
    let mut decisions: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    decisions["decisions"][1]["commentIndex"] = json!(2);
    fs::write(&path, decisions.to_string()).unwrap();
    let output = validate_output(&inputs, stage, &payload, HEAD).unwrap();
    assert_eq!(output.comments.len(), 2);
    assert_eq!(output.decisions[0]["commentIndex"], 0);
    assert_eq!(output.decisions[1]["commentIndex"], 1);
    assert_eq!(
        read_report_value(&stage.directory.join("report.json"))["findings"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    // A keep pointing past the report leaves that candidate undecided: it keeps its chunk comment.
    decisions["decisions"][1]["commentIndex"] = json!(3);
    fs::write(&path, decisions.to_string()).unwrap();
    let output = validate_output(&inputs, stage, &payload, HEAD).unwrap();
    assert_eq!(output.decisions.len(), 2);
    assert!(output
        .evidence_warnings
        .iter()
        .any(|warning| warning.reason == "unconfirmed_candidate"));
    fs::remove_dir_all(root).unwrap();
}

fn read_report_value(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn final_output_repair_preserves_distinct_findings_and_decision_indices() {
    for (index, mode) in ["keep", "changed", "missing", "reworded"].into_iter().enumerate() {
        let alter_decision = mode == "changed";
        let files = vec!["src/shared.rs".to_string()];
        let inputs = frozen_inputs(&[(files[0].clone(), LINE.into())]);
        let (root, context, map, diff) = fixture(-9300 - index as i64, &inputs, &files);
        let result = chunk_result(
            &inputs,
            &root,
            1,
            &files[0],
            vec![
                point(&files[0], "Same title", "First defect"),
                point(&files[0], "Same title", "Second defect"),
            ],
            vec![],
            vec![],
        );
        let sources = merge_chunk_findings(std::slice::from_ref(&result), HEAD);
        let payload = Payload::from_results(&[result], sources).unwrap();
        let stages = prepare(
            &inputs,
            &context,
            &map,
            &diff,
            &root.join("batches"),
            &payload,
            crate::review::policy::MAX_EVIDENCE_BYTES,
            &AtomicBool::new(false),
        )
        .unwrap();
        let stage = &stages[0];
        fake_final(&inputs, stage, &payload);
        let mut invalid: Value =
            serde_json::from_str(&fs::read_to_string(stage.directory.join("report.json")).unwrap()).unwrap();
        invalid["findings"][0]["line"] = json!(99);
        fs::write(stage.directory.join("report.json"), invalid.to_string()).unwrap();
        if mode == "missing" {
            fs::remove_file(stage.directory.join("finding_decisions.json")).unwrap();
        }
        let mut attempts = 0;
        let output = output_repair::run(
            &stage.directory,
            &stage.context,
            &stage.prompt,
            &stage.directory.join("transcript.json"),
            &AtomicBool::new(false),
            true,
            true,
            None,
            || validate_output(&inputs, stage, &payload, HEAD),
            |_| {
                attempts += 1;
                fake_final(&inputs, stage, &payload);
                if alter_decision || mode == "reworded" {
                    let path = stage.directory.join("finding_decisions.json");
                    let mut decisions: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
                    if alter_decision {
                        decisions["decisions"][0]["commentIndex"] = json!(1);
                    } else {
                        decisions["decisions"][0]["reason"] = json!("Reworded by the repair");
                    }
                    fs::write(path, decisions.to_string()).unwrap();
                }
                Ok(())
            },
        );
        if mode == "missing" {
            assert_eq!(attempts, 0);
            assert!(output.is_err());
            assert!(!stage.directory.join("rejected-report.json").exists());
            assert_eq!(read_report_value(&stage.directory.join("report.json")), invalid);
        } else if alter_decision {
            // The repair that changed a decision is set aside; the report as first written then
            // drops its one invalid finding and keeps the other.
            let output = output.unwrap();
            assert_eq!(output.decisions.len(), 2);
            let report = read_report_value(&stage.directory.join("report.json"));
            assert_eq!(report["findings"].as_array().unwrap().len(), 1);
            assert!(output
                .evidence_warnings
                .iter()
                .any(|warning| warning.reason == "dropped_finding"));
        } else {
            let decisions = read_report_value(&stage.directory.join("finding_decisions.json"));
            assert_ne!(decisions["decisions"][0]["reason"], json!("Reworded by the repair"));
            let output = output.unwrap();
            assert_eq!(output.comments.len(), 2);
            let (_, comments, _evidence_warnings) = combine(
                &inputs,
                &payload,
                vec![output],
                &map.reviewable_files,
                HEAD,
                &diff,
                &root,
            )
            .unwrap();
            let comments: Value = serde_json::from_str(&comments).unwrap();
            assert_eq!(comments["comments"].as_array().unwrap().len(), 2);
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn adaptive_integration_cancelled_preparation_does_not_produce_batches() {
    let files: Vec<String> = (0..3).map(|i| format!("src/cancel-{i}.rs")).collect();
    let inputs = frozen_inputs(&files.iter().map(|f| (f.clone(), LINE.to_string())).collect::<Vec<_>>());
    let (root, context, map, diff) = fixture(-1206, &inputs, &files);
    let cancel = AtomicBool::new(true);
    let folder = root.join("batches");
    let error = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &folder,
        &Payload::empty(HEAD),
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &cancel,
    )
    .err()
    .unwrap();
    assert_eq!(error, "cancelled");
    assert!(!folder.exists());
    assert_eq!(fs::read_to_string(context.join("PR.md")).unwrap(), "SENTINEL-PR\n");
    assert_eq!(
        fs::read_to_string(context.join("conventions/AGENTS.md")).unwrap(),
        "SENTINEL-AGENTS\n"
    );
    assert_eq!(
        fs::read_to_string(context.join("conventions/README.md")).unwrap(),
        "SENTINEL-README\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn adaptive_integration_no_findings_is_clean_only_after_full_coverage() {
    let files: Vec<String> = (0..3).map(|i| format!("src/clean-{i}.rs")).collect();
    let inputs = frozen_inputs(&files.iter().map(|f| (f.clone(), LINE.to_string())).collect::<Vec<_>>());
    let (root, context, map, diff) = fixture(-1205, &inputs, &files);
    let results: Vec<ChunkResult> = files.iter().enumerate().map(|(index, path)| ChunkResult {
        evidence_warnings: Vec::new(), cached: false,
        chunk: ReviewChunk { id: index + 1, files: vec![path.clone()], related_files: vec![], global_context_files: vec![], omitted_convention_files: vec![], risk_triggers: vec![], cross_chunk_group_ids: vec![] },
        summary: json!({"contractVersion":2,"snapshotId":snapshot_id(&inputs),"reviewedFiles":[path],"missingEvidence":[],"findings":[],"questions":[],"contextQuestions":[]}).to_string(),
        comments_json: Some(format!(r#"{{"commit_id":"{HEAD}","comments":[]}}"#)),
        usage: RunUsage::default(),
    }).collect();
    let payload = Payload::from_results(&results, merge_chunk_findings(&results, HEAD)).unwrap();
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!stages.is_empty());
    let mut outputs = Vec::new();
    for stage in &stages {
        fs::write(
            stage.directory.join("report.json"),
            json!({
                "contractVersion":2,"snapshotId":snapshot_id(&inputs),"reviewedFiles":stage.files,
                "missingEvidence":[],"findings":[],"questions":[],"contextQuestions":[]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            stage.directory.join("finding_decisions.json"),
            json!({
                "contractVersion":2,"snapshotId":snapshot_id(&inputs),"decisions":[]
            })
            .to_string(),
        )
        .unwrap();
        outputs.push(validate_output(&inputs, stage, &payload, HEAD).unwrap());
    }
    let merge = root.join("merge");
    let (body, comments, _evidence_warnings) =
        combine(&inputs, &payload, outputs, &map.reviewable_files, HEAD, &diff, &merge).unwrap();
    assert_eq!(body, crate::review::NO_FINDINGS_SUMMARY);
    assert_eq!(
        serde_json::from_str::<Value>(&comments).unwrap()["comments"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    fs::remove_dir_all(&merge).unwrap();
    let mut outputs = Vec::new();
    for stage in &stages[..stages.len() - 1] {
        outputs.push(validate_output(&inputs, stage, &payload, HEAD).unwrap());
    }
    assert!(combine(&inputs, &payload, outputs, &map.reviewable_files, HEAD, &diff, &merge).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn detected_renames_prepare_full_and_split_frozen_integration() {
    let paths = vec!["café.rs".to_string(), "new name.rs".to_string()];
    let scope = archive(&[]).scope().clone();
    let sources = paths
        .iter()
        .zip([MERGE_BASE, HEAD])
        .map(|(path, revision)| crate::review_archive::SourceEntry {
            path: path.clone(),
            revision: revision.into(),
            mode: "100644".into(),
            git_blob_id: "d".repeat(40),
            bytes: LINE.len() as u64,
        })
        .collect();
    let frozen = crate::review_archive::Archive::build(scope, sources, &AtomicBool::new(false), |_| {
        Ok(LINE.as_bytes().to_vec())
    })
    .unwrap();
    let mut inputs = review_inputs("deep", 20, 5);
    inputs.frozen = Some(std::sync::Arc::new(frozen::FrozenContext::new(frozen)));
    let (root, context, mut map, _) = fixture(-1234, &inputs, &paths);
    let diff = "diff --git \"a/caf\\303\\251.rs\" \"b/new name.rs\"\nsimilarity index 100%\nrename from \"caf\\303\\251.rs\"\nrename to new name.rs\n";
    for (index, assigned) in [paths.clone(), vec![paths[0].clone()], vec![paths[1].clone()]]
        .into_iter()
        .enumerate()
    {
        map.reviewable_files = assigned;
        let stages = prepare(
            &inputs,
            &context,
            &map,
            diff,
            &root.join(format!("rename-{index}")),
            &Payload::empty(HEAD),
            crate::review::policy::MAX_EVIDENCE_BYTES,
            &AtomicBool::new(false),
        )
        .unwrap();
        for stage in stages {
            assert_eq!(stage.diff, diff);
            let request = crate::sandbox::runner::stage_request(
                &inputs.model,
                "offline",
                1,
                "claude",
                &root,
                &stage.directory,
                &stage.context,
                &stage.prompt,
            )
            .unwrap();
            let crate::sandbox::runner::Request::Run { files, .. } = request else {
                panic!("Expected stage request");
            };
            inputs
                .frozen
                .as_ref()
                .unwrap()
                .archive
                .validate_stage_files(&files)
                .unwrap();
        }
    }
    assert!(inputs
        .frozen
        .as_ref()
        .unwrap()
        .archive
        .validate_assigned(&paths, "", MERGE_BASE)
        .is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn incremental_publication_keeps_delta_coverage_with_separate_frozen_anchors() {
    let prior = "e".repeat(40);
    let scope = crate::review_archive::Scope {
        prior_sha: Some(prior.clone()),
        ..archive(&[]).scope().clone()
    };
    let sources = [
        ("code.rs", HEAD, "old();\nnew();\n"),
        ("code.rs", MERGE_BASE, "base();\n"),
        ("code.rs", prior.as_str(), "old();\nprior();\n"),
        ("untouched.rs", HEAD, LINE),
    ];
    let entries = sources
        .iter()
        .map(|(path, revision, content)| crate::review_archive::SourceEntry {
            path: (*path).into(),
            revision: (*revision).into(),
            mode: "100644".into(),
            git_blob_id: "d".repeat(40),
            bytes: content.len() as u64,
        })
        .collect();
    let archive = crate::review_archive::Archive::build(scope, entries, &AtomicBool::new(false), |entry| {
        Ok(sources
            .iter()
            .find(|(path, revision, _)| *path == entry.path && *revision == entry.revision)
            .unwrap()
            .2
            .as_bytes()
            .to_vec())
    })
    .unwrap();
    let mut inputs = review_inputs("deep", 20, 5);
    inputs.frozen = Some(std::sync::Arc::new(frozen::FrozenContext::new(archive)));
    let files = vec!["code.rs".to_string()];
    let (root, context, map, _) = fixture(-1240, &inputs, &files);
    let delta = "diff --git a/code.rs b/code.rs\n--- a/code.rs\n+++ b/code.rs\n@@ -2 +2 @@\n-prior();\n+new();\n";
    let publication =
        "diff --git a/code.rs b/code.rs\n--- a/code.rs\n+++ b/code.rs\n@@ -1 +1,2 @@\n-base();\n+old();\n+new();\n";
    fs::write(context.join("PUBLICATION_DIFF.patch"), publication).unwrap();
    inputs
        .frozen
        .as_ref()
        .unwrap()
        .archive
        .prepare_stage(&context, &files, delta, &prior)
        .unwrap();
    let payload = Payload::empty(HEAD);
    let stages = prepare(
        &inputs,
        &context,
        &map,
        delta,
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(stages.len(), 1);
    let stage = &stages[0];
    assert_eq!(stage.files, files);
    assert_eq!(stage.diff, delta);
    assert_eq!(
        fs::read_to_string(stage.context.join("PUBLICATION_DIFF.patch")).unwrap(),
        publication
    );
    let request = crate::sandbox::runner::stage_request(
        &inputs.model,
        "offline",
        1,
        "claude",
        &root,
        &stage.directory,
        &stage.context,
        &stage.prompt,
    )
    .unwrap();
    let crate::sandbox::runner::Request::Run {
        files: mut packaged, ..
    } = request
    else {
        panic!("Expected stage request");
    };
    let archive = &inputs.frozen.as_ref().unwrap().archive;
    archive.validate_stage_files(&packaged).unwrap();
    packaged.insert("context/PUBLICATION_DIFF.patch".into(), delta.into());
    assert!(archive.validate_stage_files(&packaged).is_err());
    packaged.remove("context/PUBLICATION_DIFF.patch");
    assert!(archive.validate_stage_files(&packaged).is_err());
    let evidence = json!({"path":"code.rs","revision":HEAD,"line":2,"quote":"new();"});
    let counter = json!({"path":"code.rs","revision":prior,"line":2,"quote":"prior();"});
    let mut report = json!({"contractVersion":2,"snapshotId":snapshot_id(&inputs),"reviewedFiles":files,"missingEvidence":[],"findings":[{"kind":"finding","title":"Changed guard","detail":"The latest change bypasses the guard","action":"Restore the guard","path":"code.rs","line":1,"side":"LEFT","evidence":[evidence],"counterEvidence":[counter]}]});
    fs::write(stage.directory.join("report.json"), report.to_string()).unwrap();
    fs::write(
        stage.directory.join("finding_decisions.json"),
        json!({"contractVersion":2,"snapshotId":snapshot_id(&inputs),"decisions":[]}).to_string(),
    )
    .unwrap();
    let output = validate_output(&inputs, stage, &payload, HEAD).unwrap();
    let (_, comments, _evidence_warnings) = combine(
        &inputs,
        &payload,
        vec![output],
        &files,
        HEAD,
        publication,
        &root.join("merge"),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&comments).unwrap()["comments"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    report["findings"][0]["line"] = json!(2);
    fs::write(stage.directory.join("report.json"), report.to_string()).unwrap();
    assert!(validate_output(&inputs, stage, &payload, HEAD)
        .err()
        .unwrap()
        .contains("invalid finding anchor"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn frozen_final_decision_example_requires_explicit_snapshot_and_revision() {
    let files = vec!["code.rs".to_string()];
    let inputs = frozen_inputs(&[(files[0].clone(), LINE.into())]);
    let (root, context, map, _) = fixture(-1241, &inputs, &files);
    let prompt = render_final_prompt(&inputs, &context, &root, &map);
    let example = prompt
        .split("Candidate decision JSON example:\n")
        .nth(1)
        .unwrap()
        .split("\nEnd candidate decision example.")
        .next()
        .unwrap();
    let example: Value = serde_json::from_str(example).unwrap();
    assert_eq!(example["contractVersion"], 2);
    assert_eq!(example["snapshotId"], snapshot_id(&inputs));
    assert_eq!(example["decisions"][1]["counterEvidence"][0]["revision"], HEAD);
    assert_eq!(example["decisions"][0]["unchanged"], true);
    assert!(example["decisions"][0].get("commentIndex").is_none());
    assert!(prompt.contains("so do not write it again in report.json findings"));
    assert!(prompt.contains("Do not cite chunk reports or candidate summaries as counter-evidence"));
    assert!(prompt.contains("Include findingDecisions alongside report in your final JSON response"));
    assert!(prompt.contains("candidates in the same file with the same root cause and the same fix are duplicates too"));
    assert!(prompt.contains("candidates in different files, or needing different fixes, stay separate comments"));
    assert!(!prompt.contains("Write /"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scoped_diff_partitions_exact_sections_without_truncation() {
    let deleted =
        "diff --git a/old name.rs b/old name.rs\n--- a/old name.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-let value = 1;\n";
    let renamed = "diff --git a/a.rs b/new name.rs\nrename from a.rs\nrename to \"new name.rs\"\n";
    let unicode = "diff --git \"a/caf\\303\\251.rs\" \"b/caf\\303\\251.rs\"\n--- \"a/caf\\303\\251.rs\"\n+++ \"b/caf\\303\\251.rs\"\n@@ -1 +1 @@\n-let v = 1;\n+let v = 2;\n";
    let modify =
        "diff --git a/keep.rs b/keep.rs\n--- a/keep.rs\n+++ b/keep.rs\n@@ -1 +1 @@\n-let value = 1;\n+let value = 2;\n";
    let unknown = "diff --git a/foreign.rs b/foreign.rs\n--- a/foreign.rs\n+++ b/foreign.rs\n@@ -1 +1 @@\n-a\n+b\n";
    let preamble = "arbitrary collector preamble\n";
    let inventory: Vec<String> = ["old name.rs", "new name.rs", "café.rs", "keep.rs"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let diff = format!("{preamble}{deleted}{renamed}{unicode}{modify}{unknown}");

    let scoped = scoped_diff(&diff, &["keep.rs".into()], &inventory);
    assert!(
        scoped.contains(preamble.trim_end()),
        "unrecognized leading bytes retained"
    );
    assert!(scoped.contains(modify), "assigned section kept verbatim");
    assert!(
        scoped.contains(unknown),
        "sections outside the inventory are retained conservatively"
    );
    assert!(
        !scoped.contains("old name.rs") && !scoped.contains("new name.rs") && !scoped.contains("caf"),
        "unassigned known sections excluded"
    );

    let scoped = scoped_diff(
        &diff,
        &["old name.rs".into(), "new name.rs".into(), "café.rs".into()],
        &inventory,
    );
    assert!(scoped.contains(deleted), "deleted-file section kept for its owner");
    assert!(scoped.contains(renamed), "rename section kept for the new path");
    assert!(
        scoped.contains(unicode),
        "quoted unicode section kept for its decoded path"
    );
    assert!(!scoped.contains("keep.rs"));

    let anchors = crate::review::diff_anchors(modify);
    assert!(anchors.contains(&("keep.rs".to_string(), 1, "RIGHT".to_string())));
    assert!(anchors.contains(&("keep.rs".to_string(), 1, "LEFT".to_string())));
}

#[test]
fn prepared_prompt_budget_includes_snapshot_read_instructions() {
    let seed_files = vec!["a.rs".to_string()];
    let seed_inputs = frozen_inputs(&[(seed_files[0].clone(), LINE.into())]);
    let (seed_root, _, seed_map, _) = fixture(-228511, &seed_inputs, &seed_files);
    let seed_stage = seed_root.join("batches/batch-001");
    let seed_context = seed_stage.join("context");
    let seed_prompt = format!(
        "{}{BATCH_INSTRUCTIONS}",
        render_final_prompt(&seed_inputs, &seed_context, &seed_stage, &seed_map)
    );
    // Reach the prompt limit through valid filenames to exercise the real batch planner.
    let target = crate::review::policy::MAX_PROMPT_BYTES - 100;
    let count = 800;
    let fixed_bytes = seed_prompt.len() - seed_files[0].len();
    let path_bytes = target - fixed_bytes - (count - 1) * ", ".len();
    let path_len = path_bytes / count;
    let remainder = path_bytes % count;
    assert!(path_len > 12 && path_len < 255);
    let files = (0..count)
        .map(|index| {
            let length = path_len + usize::from(index < remainder);
            let prefix = format!("file-{index:04}-");
            format!("{prefix}{}.rs", "x".repeat(length - prefix.len() - ".rs".len()))
        })
        .collect::<Vec<_>>();
    let seed_root_text = seed_root.to_string_lossy().into_owned();
    fs::remove_dir_all(seed_root).unwrap();
    let inputs = frozen_inputs(&files.iter().map(|path| (path.clone(), LINE.into())).collect::<Vec<_>>());
    let (root, context, map, diff) = fixture(-228511, &inputs, &files);
    // The run directory's name carries a process-wide counter, so this one can be a digit longer
    // than the seed's, and the prompt repeats it.
    let shift = seed_prompt.matches(seed_root_text.as_str()).count() as isize
        * (root.to_string_lossy().len() as isize - seed_root_text.len() as isize);
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &Payload::empty(HEAD),
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(stages.len(), 1);
    let stage = &stages[0];
    assert_eq!(stage.files, files);
    let prepared = fs::read_to_string(&stage.prompt).unwrap();
    assert_eq!(prepared.len() as isize, target as isize + shift);
    validate_review_stage_inputs(&stage.context, Some(&stage.directory), &stage.prompt).unwrap();
    let request = crate::sandbox::runner::stage_request(
        &inputs.model,
        "offline",
        1,
        "claude",
        &root,
        &stage.directory,
        &stage.context,
        &stage.prompt,
    )
    .expect("A prepared batch near the prompt limit must fit the runner budget");
    let crate::sandbox::runner::Request::Run { prompt, .. } = request else {
        panic!("Expected a stage request");
    };
    let instructions = crate::sandbox::runner::SNAPSHOT_READ_INSTRUCTIONS;
    assert_eq!(prepared.matches(instructions).count(), 1);
    assert_eq!(prompt.matches(instructions).count(), 1);
    assert!(prompt.len() <= crate::review::policy::MAX_PROMPT_BYTES);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn crowded_integration_splits_by_files_without_dropping_candidates() {
    let files: Vec<String> = (0..2).map(|i| format!("src/file-{i}.rs")).collect();
    let inputs = frozen_inputs(&files.iter().map(|f| (f.clone(), LINE.to_string())).collect::<Vec<_>>());
    let (root, context, map, diff) = fixture(-1204, &inputs, &files);
    let results: Vec<ChunkResult> = files
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let points = (0..20)
                .map(|issue| point(path, &format!("Issue {index}-{issue}"), "Inspect the value"))
                .collect();
            chunk_result(&inputs, &root, (index + 1) as i64, path, points, vec![], vec![])
        })
        .collect();
    let sources = merge_chunk_findings(&results, HEAD);
    let payload = Payload::from_results(&results, sources).unwrap();
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(stages.len(), 2);
    assert!(stages
        .iter()
        .all(|stage| stage.candidate_ids.len() <= super::prepare::MAX_BATCH_CANDIDATES));
    let ids = stages
        .iter()
        .flat_map(|stage| stage.candidate_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), payload.originals().len());
}

#[test]
fn a_batch_skipped_for_time_keeps_its_chunk_findings_unconfirmed() {
    let files = vec!["src/shared.rs".to_string()];
    let inputs = frozen_inputs(&[(files[0].clone(), LINE.into())]);
    let (root, context, map, diff) = fixture(-9400, &inputs, &files);
    let result = chunk_result(
        &inputs,
        &root,
        1,
        &files[0],
        vec![
            point(&files[0], "Same title", "First defect"),
            point(&files[0], "Same title", "Second defect"),
        ],
        vec![],
        vec![],
    );
    let sources = merge_chunk_findings(std::slice::from_ref(&result), HEAD);
    let payload = Payload::from_results(&[result], sources).unwrap();
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    let output = unconfirmed(&stages[0], &payload, HEAD).unwrap();
    assert_eq!(output.decisions.len(), 2);
    assert!(output
        .evidence_warnings
        .iter()
        .all(|warning| warning.reason == "unconfirmed_candidate"));
    let (_, comments, warnings) = combine(
        &inputs,
        &payload,
        vec![output],
        &map.reviewable_files,
        HEAD,
        &diff,
        &root,
    )
    .unwrap();
    let comments: Value = serde_json::from_str(&comments).unwrap();
    assert_eq!(comments["comments"].as_array().unwrap().len(), 2);
    assert!(!warnings.is_empty());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn chunk_suggestions_go_to_the_final_summary_not_to_the_final_pass() {
    let suggestion =
        json!({"path":"src/a.rs","line":2,"title":"Stale test name","detail":"Detail","action":"Rename it"});
    let result = ChunkResult {
        cached: false,
        chunk: ReviewChunk {
            id: 1,
            files: vec!["src/a.rs".into()],
            related_files: vec![],
            global_context_files: vec![],
            omitted_convention_files: vec![],
            risk_triggers: vec![],
            cross_chunk_group_ids: vec![],
        },
        summary:
            json!({"reviewedFiles":["src/a.rs"],"missingEvidence":[],"findings":[],"suggestions":[suggestion.clone()]})
                .to_string(),
        comments_json: Some(json!({"commit_id":HEAD,"comments":[]}).to_string()),
        evidence_warnings: vec![],
        usage: RunUsage::default(),
    };
    let results = vec![result.clone()];
    let payload = Payload::from_results(&results, merge_chunk_findings(&results, HEAD)).unwrap();
    assert_eq!(payload.suggestions, std::slice::from_ref(&suggestion));
    assert!(payload
        .notes
        .iter()
        .all(|(_, note)| note["report"].get("suggestions").is_none()));
    // A report whose findings do not line up with its candidates goes as text, without them too.
    let mut unstructured = result;
    unstructured.summary = json!({"reviewedFiles":["src/a.rs"],"missingEvidence":[],
        "findings":[{"path":"src/a.rs","line":9}],"suggestions":[suggestion.clone()]})
    .to_string();
    let results = vec![unstructured];
    let payload = Payload::from_results(&results, merge_chunk_findings(&results, HEAD)).unwrap();
    assert_eq!(payload.suggestions, [suggestion]);
    let (_, note) = &payload.notes[0];
    assert!(note["report"].is_null());
    assert!(!note["summary"].as_str().unwrap().contains("Stale test name"), "{note}");
}

/// Writes a final report holding only the first candidate's point (when `rewrite_first`), keeping
/// it by index and every other candidate `unchanged` with `reason`.
fn final_keeping_unchanged(
    inputs: &ReviewInputs,
    stage: &Prepared,
    payload: &Payload,
    rewrite_first: bool,
    reason: &str,
) {
    let snapshot = snapshot_id(inputs);
    let findings: Vec<Value> = stage
        .candidate_ids
        .iter()
        .take(usize::from(rewrite_first))
        .map(|id| payload.candidates.get(id).unwrap()["point"].clone())
        .collect();
    fs::write(
        stage.directory.join("report.json"),
        json!({
            "contractVersion":2,"snapshotId":snapshot,"reviewedFiles":stage.files,
            "missingEvidence":[],"findings":findings,"questions":[],"contextQuestions":[]
        })
        .to_string(),
    )
    .unwrap();
    let decisions: Vec<Value> = stage
        .candidate_ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            if rewrite_first && index == 0 {
                json!({"candidateId":id,"decision":"keep","commentIndex":0,"reason":"Verified and reworded"})
            } else {
                json!({"candidateId":id,"decision":"keep","unchanged":true,"reason":reason})
            }
        })
        .collect();
    fs::write(
        stage.directory.join("finding_decisions.json"),
        json!({"contractVersion":2,"snapshotId":snapshot,"decisions":decisions}).to_string(),
    )
    .unwrap();
}

fn unchanged_fixture(seed: i64) -> (ReviewInputs, PathBuf, ReviewMap, String, Payload, Vec<Prepared>) {
    let files: Vec<String> = (0..3).map(|i| format!("src/kept-{i}.rs")).collect();
    let inputs = frozen_inputs(&files.iter().map(|f| (f.clone(), LINE.to_string())).collect::<Vec<_>>());
    let (root, context, map, diff) = fixture(seed, &inputs, &files);
    let results: Vec<ChunkResult> = files
        .iter()
        .enumerate()
        .map(|(index, path)| {
            chunk_result(
                &inputs,
                &root,
                (index + 1) as i64,
                path,
                vec![point(path, &format!("Issue {index}"), "Inspect the value")],
                vec![],
                vec![],
            )
        })
        .collect();
    let sources = merge_chunk_findings(&results, HEAD);
    let payload = Payload::from_results(&results, sources).unwrap();
    let stages = prepare(
        &inputs,
        &context,
        &map,
        &diff,
        &root.join("batches"),
        &payload,
        crate::review::policy::MAX_EVIDENCE_BYTES,
        &AtomicBool::new(false),
    )
    .unwrap();
    (inputs, root, map, diff, payload, stages)
}

#[test]
fn candidates_kept_unchanged_post_their_chunk_comments_confirmed() {
    let (inputs, root, map, diff, payload, stages) = unchanged_fixture(-1210);
    let mut outputs = Vec::new();
    for stage in &stages {
        final_keeping_unchanged(&inputs, stage, &payload, false, "Verified; the chunk comment stands");
        outputs.push(validate_output(&inputs, stage, &payload, HEAD).unwrap());
    }
    assert!(outputs.iter().all(|output| output.evidence_warnings.is_empty()));
    let merge = root.join("merge");
    let (_, comments, warnings) =
        combine(&inputs, &payload, outputs, &map.reviewable_files, HEAD, &diff, &merge).unwrap();
    assert!(warnings.is_empty(), "unchanged keeps are confirmed, not unconfirmed");
    let comments: Value = serde_json::from_str(&comments).unwrap();
    let comments = comments["comments"].as_array().unwrap();
    let originals = payload.originals();
    assert_eq!(comments.len(), originals.len());
    for original in originals.iter() {
        assert!(
            comments.contains(&original["comment"]),
            "chunk comment is posted as validated"
        );
    }
    let decisions = fs::read_to_string(merge.join("finding_decisions.json")).unwrap();
    let entries: Value = serde_json::from_str(&decisions).unwrap();
    for entry in entries["decisions"].as_array().unwrap() {
        assert_eq!(entry["reason"], "Verified; the chunk comment stands");
        let candidate = originals
            .iter()
            .find(|c| c["candidateId"] == entry["candidateId"])
            .unwrap();
        assert_eq!(
            comments[entry["commentIndex"].as_u64().unwrap() as usize],
            candidate["comment"]
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_rewritten_finding_and_unchanged_keeps_share_one_batch() {
    let (inputs, root, _, _, payload, stages) = unchanged_fixture(-1211);
    let stage = &stages[0];
    assert!(stage.candidate_ids.len() > 1);
    final_keeping_unchanged(&inputs, stage, &payload, true, "Verified; the chunk comment stands");
    let output = validate_output(&inputs, stage, &payload, HEAD).unwrap();
    assert!(output.evidence_warnings.is_empty());
    assert_eq!(output.comments.len(), stage.candidate_ids.len());
    let indices: BTreeSet<u64> = output
        .decisions
        .iter()
        .map(|entry| entry["commentIndex"].as_u64().unwrap())
        .collect();
    assert_eq!(
        indices.len(),
        stage.candidate_ids.len(),
        "every candidate maps to its own comment"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_unchanged_keep_without_a_reason_stays_unconfirmed() {
    let (inputs, root, _, _, payload, stages) = unchanged_fixture(-1212);
    let stage = &stages[0];
    final_keeping_unchanged(&inputs, stage, &payload, false, " ");
    let output = validate_output(&inputs, stage, &payload, HEAD).unwrap();
    assert_eq!(output.evidence_warnings.len(), stage.candidate_ids.len());
    assert!(output
        .evidence_warnings
        .iter()
        .all(|warning| warning.reason == "unconfirmed_candidate"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_unchanged_keep_with_an_index_keeps_the_rewritten_finding_once() {
    let (inputs, root, _, _, payload, stages) = unchanged_fixture(-1213);
    let stage = &stages[0];
    final_keeping_unchanged(&inputs, stage, &payload, true, "Verified; the chunk comment stands");
    // Reword the rewritten finding and mark its keep unchanged as well, contradicting the index.
    let path = stage.directory.join("report.json");
    let mut report: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    report["findings"][0]["title"] = json!("Issue reworded by the final pass");
    fs::write(&path, report.to_string()).unwrap();
    let path = stage.directory.join("finding_decisions.json");
    let mut decisions: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    decisions["decisions"][0]["unchanged"] = json!(true);
    fs::write(&path, decisions.to_string()).unwrap();
    let output = validate_output(&inputs, stage, &payload, HEAD).unwrap();
    assert_eq!(
        output.comments.len(),
        stage.candidate_ids.len(),
        "no orphan comment is posted"
    );
    let first = &output.comments[output.decisions[0]["commentIndex"].as_u64().unwrap() as usize];
    assert!(first["body"]
        .as_str()
        .unwrap()
        .contains("Issue reworded by the final pass"));
    fs::remove_dir_all(root).unwrap();
}
