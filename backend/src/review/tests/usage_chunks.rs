use super::usage_tests::{fixture_chunk_prompt_bytes, graph_chunk_fixture, review_inputs};

#[test]
fn chunk_metadata_references_the_complete_canonical_pr_description() {
    let (root, mut map) = graph_chunk_fixture(-998, 0, 0);
    map.description = "Author intent and explicit deferrals\n".repeat(500);
    let pr = format!("# PR\n\n{}", map.description);
    std::fs::write(root.join("PR.md"), &pr).unwrap();
    let chunk = super::ReviewChunk {
        id: 1,
        files: vec!["a.rs".into()],
        related_files: vec![],
        global_context_files: vec![],
        omitted_convention_files: vec![],
        risk_triggers: vec![],
        cross_chunk_group_ids: vec![],
    };
    let scoped = super::chunk_review_map(&map, &chunk);
    assert_eq!(
        scoped.description,
        "See PR.md for the MR description and RELATED_ISSUES.md (when present) for linked GitLab issue intent."
    );
    assert_eq!(scoped.changed_files, map.changed_files);
    assert_eq!(scoped.reviewable_files, map.reviewable_files);
    assert_eq!(std::fs::read_to_string(root.join("PR.md")).unwrap(), pr);
    assert!(map.description.contains("explicit deferrals"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_chunks_use_rendered_prompt_instead_of_fixed_reserve() {
    let (root, mut map) = graph_chunk_fixture(-989, 57_041, 0);
    std::fs::write(root.join("changed/a.rs"), "x".repeat(60_105)).unwrap();
    let header = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-previous\n+";
    let diff = format!("{header}{}\n", "x".repeat(14_143 - header.len() - 1));
    std::fs::write(root.join("PR_DIFF.patch"), &diff).unwrap();
    let patches = super::chunk_diff_bytes(&map, &diff);
    let chunk = super::ReviewChunk {
        id: 1,
        files: vec!["a.rs".into()],
        related_files: vec![],
        global_context_files: vec![],
        omitted_convention_files: vec![],
        risk_triggers: vec![],
        cross_chunk_group_ids: vec![],
    };
    map.packages.push(String::new());
    let metadata =
        super::chunk_context_budget(&map, &root, &chunk, &patches, fixture_chunk_prompt_bytes(&root, &chunk))
            .unwrap()
            .metadata;
    map.packages[0].push_str(&"m".repeat(12_403 - metadata));
    let budget =
        super::chunk_context_budget(&map, &root, &chunk, &patches, fixture_chunk_prompt_bytes(&root, &chunk)).unwrap();
    assert_eq!(
        budget.files + budget.diff + budget.metadata + budget.shared + 16_384,
        160_076
    );
    let inputs = review_inputs("deep", 20, 5);
    let directory = root.join("chunks/001");
    let prompt = super::render_chunk_prompt(
        &inputs,
        &directory.join("context"),
        &directory.join("summary.md"),
        "head",
        None,
        &chunk,
    );
    assert!(
        budget.files + budget.diff + budget.metadata + budget.shared + prompt.len()
            <= crate::review::policy::TARGET_CONTEXT_BYTES
    );
    let chunks = super::build_graph_chunks(&map, &root, 5, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap();
    assert_eq!(
        chunks.iter().flat_map(|chunk| chunk.files.clone()).collect::<Vec<_>>(),
        map.reviewable_files
    );
    assert_eq!(std::fs::read(root.join("changed/a.rs")).unwrap().len(), 60_105);
    assert_eq!(std::fs::read_to_string(root.join("PR_DIFF.patch")).unwrap(), diff);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_chunks_skip_an_oversized_candidate_to_pack_a_fitting_file() {
    let (root, map) = graph_chunk_fixture(-1125, 0, 0);
    let chunks = super::build_graph_chunks(&map, &root, 2, &|chunk| {
        if chunk.files.len() > 1 && chunk.files.iter().any(|path| path == "b.rs") {
            crate::review::policy::TARGET_CONTEXT_BYTES
        } else {
            0
        }
    })
    .unwrap();
    assert_eq!(chunks[0].files, ["a.rs", "c.rs"]);
    assert_eq!(chunks[1].files, ["b.rs"]);
    let mut covered = chunks.iter().flat_map(|chunk| chunk.files.clone()).collect::<Vec<_>>();
    covered.sort();
    let mut expected = map.reviewable_files.clone();
    expected.sort();
    assert_eq!(covered, expected);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_chunks_keep_all_conventions_above_the_selection_target() {
    let (root, mut map) = graph_chunk_fixture(-991, 29_346, 0);
    std::fs::create_dir_all(root.join("conventions")).unwrap();
    std::fs::write(root.join("conventions/AGENTS.md"), "a".repeat(9_075)).unwrap();
    std::fs::write(root.join("conventions/README.md"), "r".repeat(15_658)).unwrap();
    std::fs::write(root.join("PR.md"), "p".repeat(2_962)).unwrap();
    std::fs::write(root.join("changed/a.rs"), "x".repeat(78_611)).unwrap();
    map.convention_files = vec!["AGENTS.md".into(), "README.md".into()];
    map.packages.push("p".repeat(14_000));
    let header = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-previous\n+";
    let diff = format!("{header}{}\n", "x".repeat(18_667 - header.len() - 1));
    std::fs::write(root.join("PR_DIFF.patch"), &diff).unwrap();
    let chunks = super::build_graph_chunks(&map, &root, 1, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap();
    let scoped = super::chunk_review_map(&map, &chunks[0]);
    assert_eq!(scoped.convention_files, ["AGENTS.md", "README.md"]);
    let materialized = root.join("chunk-context");
    super::copy_chunk_conventions(&root, &materialized, &chunks[0]).unwrap();
    assert_eq!(
        std::fs::read(materialized.join("conventions/AGENTS.md")).unwrap(),
        std::fs::read(root.join("conventions/AGENTS.md")).unwrap()
    );
    assert_eq!(
        std::fs::read(materialized.join("conventions/README.md")).unwrap(),
        std::fs::read(root.join("conventions/README.md")).unwrap()
    );
    let patches = super::chunk_diff_bytes(&map, &diff);
    let budget = super::chunk_context_budget(
        &map,
        &root,
        &chunks[0],
        &patches,
        fixture_chunk_prompt_bytes(&root, &chunks[0]),
    )
    .unwrap();
    assert_eq!(budget.shared, 29_346 + 9_075 + 2_962 + 15_658);
    assert!(budget.total() > crate::review::policy::TARGET_CONTEXT_BYTES);
    budget.validate().unwrap();
    assert!(chunks[0].omitted_convention_files.is_empty());
    assert_eq!(
        super::chunk_review_map(&map, &chunks[1]).convention_files,
        map.convention_files
    );
    assert_eq!(std::fs::read(root.join("changed/a.rs")).unwrap().len(), 78_611);
    assert_eq!(std::fs::read_to_string(root.join("PR_DIFF.patch")).unwrap(), diff);
    assert_eq!(std::fs::read(root.join("conventions/README.md")).unwrap().len(), 15_658);
    std::fs::write(
        root.join("conventions/AGENTS.md"),
        "a".repeat(crate::review::policy::MAX_EVIDENCE_BYTES + 1),
    )
    .unwrap();
    assert!(super::build_graph_chunks(&map, &root, 1, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn single_and_final_stages_separate_prompt_size_from_disk_evidence() {
    let root = super::make_run_dir(-1002).unwrap();
    let context = root.join("context");
    let stage = root.join("merge");
    std::fs::create_dir(&context).unwrap();
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(context.join("source.rs"), "x".repeat(240_000)).unwrap();
    let prompt = stage.join("prompt.md");
    std::fs::write(&prompt, "p".repeat(crate::review::policy::MAX_PROMPT_BYTES)).unwrap();
    super::validate_review_stage_inputs(&context, None, &prompt).unwrap();
    let available = crate::review::policy::MAX_EVIDENCE_BYTES - 240_000;
    std::fs::write(stage.join("candidates.json"), "x".repeat(available)).unwrap();
    super::validate_review_stage_inputs(&context, Some(&stage), &prompt).unwrap();
    std::fs::write(stage.join("candidates.json"), "x".repeat(available + 1)).unwrap();
    assert!(super::validate_review_stage_inputs(&context, Some(&stage), &prompt)
        .unwrap_err()
        .contains("disk artifact limit"));
    std::fs::write(&prompt, "p".repeat(crate::review::policy::MAX_PROMPT_BYTES + 1)).unwrap();
    assert!(super::validate_review_stage_inputs(&context, None, &prompt)
        .unwrap_err()
        .contains("rendered prompt"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn nested_batch_context_is_counted_once_in_stage_budget() {
    let root = super::make_run_dir(-1123).unwrap();
    let stage = root.join("batch");
    let context = stage.join("context");
    std::fs::create_dir_all(&context).unwrap();
    std::fs::write(
        context.join("big.rs"),
        "x".repeat(crate::review::policy::MAX_EVIDENCE_BYTES / 2 + 1),
    )
    .unwrap();
    let prompt = stage.join("prompt.md");
    std::fs::write(&prompt, "tiny").unwrap();
    std::fs::write(stage.join("candidates.json"), "c").unwrap();
    super::validate_review_stage_inputs(&context, Some(&stage), &prompt).unwrap();
    std::fs::write(
        context.join("big.rs"),
        "x".repeat(crate::review::policy::MAX_EVIDENCE_BYTES),
    )
    .unwrap();
    assert!(super::validate_review_stage_inputs(&context, Some(&stage), &prompt).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn oversized_shared_context_partitions_instead_of_rejecting_the_pr() {
    let (root, mut map) = graph_chunk_fixture(-1120, 0, 0);
    let files: Vec<String> = (0..20).map(|index| format!("unit-{index:03}.rs")).collect();
    for path in &files {
        std::fs::write(root.join("changed").join(path), "x".repeat(450_000)).unwrap();
    }
    map.changed_files = files.clone();
    map.reviewable_files = files.clone();
    super::write_review_map(&root, &map).unwrap();
    assert!(super::dir_bytes(&root) as usize > crate::review::policy::MAX_EVIDENCE_BYTES);
    let chunks = super::build_graph_chunks(&map, &root, 5, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap();
    assert_eq!(
        chunks.iter().flat_map(|chunk| chunk.files.clone()).collect::<Vec<_>>(),
        files
    );
    let patches = super::chunk_diff_bytes(&map, "");
    for chunk in &chunks {
        super::chunk_context_budget(&map, &root, chunk, &patches, fixture_chunk_prompt_bytes(&root, chunk))
            .unwrap()
            .validate()
            .unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn oversized_frozen_single_pass_prefers_batches_over_rejection() {
    let root = super::make_run_dir(-1121).unwrap();
    let context = root.join("context");
    std::fs::create_dir_all(&context).unwrap();
    std::fs::write(
        context.join("PR.md"),
        "p".repeat(crate::review::policy::MAX_EVIDENCE_BYTES + 1),
    )
    .unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let scope = crate::review_archive::Scope {
        host: "github.com".into(),
        repository: "acme/fixture".into(),
        head_sha: "a".repeat(40),
        base_sha: "b".repeat(40),
        merge_base_sha: "c".repeat(40),
        prior_sha: None,
    };
    let source = crate::review_archive::SourceEntry {
        path: "a.rs".into(),
        revision: scope.head_sha.clone(),
        mode: "100644".into(),
        git_blob_id: "d".repeat(40),
        bytes: 2,
    };
    let archive =
        crate::review_archive::Archive::build(scope.clone(), vec![source], &cancel, |_| Ok(b"x\n".to_vec())).unwrap();
    let mut inputs = review_inputs("economic", 20, 5);
    inputs.frozen = Some(std::sync::Arc::new(super::frozen::FrozenContext::new(archive)));
    let (map_root, mut map) = graph_chunk_fixture(-1122, 0, 2);
    map.head_sha = scope.head_sha.clone();
    map.reviewable_files = vec!["a.rs".into()];
    map.changed_files = vec!["a.rs".into()];
    assert!(super::should_chunk_prepared_review(&inputs, &map, &context, &root));
    std::fs::write(context.join("PR.md"), "small").unwrap();
    assert!(!super::should_chunk_prepared_review(&inputs, &map, &context, &root));
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(map_root).unwrap();
}

#[test]
fn graph_chunks_allow_a_complete_file_above_the_selection_target() {
    let (root, mut map) = graph_chunk_fixture(-1001, 20_000, 240_000);
    std::fs::create_dir(root.join("conventions")).unwrap();
    std::fs::write(root.join("conventions/README.md"), "Keep project conventions").unwrap();
    map.convention_files = vec!["README.md".into()];
    let chunks = super::build_graph_chunks(&map, &root, 5, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap();
    assert_eq!(chunks.len(), 3);
    assert!(chunks
        .iter()
        .all(|chunk| chunk.files.len() == 1 && chunk.omitted_convention_files.is_empty()));
    assert_eq!(std::fs::read(root.join("changed/a.rs")).unwrap().len(), 240_000);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_chunks_soft_target_still_packs_hard_valid_files() {
    let (root, map) = graph_chunk_fixture(-983, 128_000, 20_000);
    let chunks = super::build_graph_chunks(&map, &root, 5, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap();
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].files, map.reviewable_files);
    let patches = super::chunk_diff_bytes(&map, "");
    let budget = super::chunk_context_budget(
        &map,
        &root,
        &chunks[0],
        &patches,
        fixture_chunk_prompt_bytes(&root, &chunks[0]),
    )
    .unwrap();
    assert!(budget.total() > crate::review::policy::TARGET_CONTEXT_BYTES);
    assert!(budget.validate().is_ok());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_chunks_reserve_direct_dependencies_before_optional_context() {
    let (root, mut map) = graph_chunk_fixture(-992, 20_000, 20_000);
    std::fs::create_dir_all(root.join("related")).unwrap();
    for path in ["aa.rs", "ab.rs"] {
        std::fs::write(root.join("related").join(path), "x".repeat(50_000)).unwrap();
        map.related_files.push(crate::db::ContextFile {
            path: path.into(),
            package_path: None,
            reason: "same package".into(),
            source: "head".into(),
        });
    }
    map.dependency_edges = vec![("a.rs".into(), "c.rs".into())];
    let chunks = super::build_graph_chunks(&map, &root, 1, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap();
    assert_eq!(chunks[0].global_context_files, ["c.rs"]);
    assert_eq!(chunks[0].related_files.len(), 1);
    assert_eq!(
        chunks.iter().flat_map(|chunk| chunk.files.clone()).collect::<Vec<_>>(),
        map.reviewable_files
    );
    for chunk in &chunks {
        assert!(
            super::chunk_context_bytes(
                &map,
                &root,
                chunk,
                &(std::collections::BTreeMap::new(), 0),
                fixture_chunk_prompt_bytes(&root, chunk)
            )
            .unwrap()
                <= crate::review::policy::TARGET_CONTEXT_BYTES
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore]
fn saved_context_preserves_required_chunk_dependency_without_inference() {
    let root = std::path::PathBuf::from(std::env::var("OCR_DIAGNOSTIC_CONTEXT").unwrap());
    let source = std::env::var("OCR_DIAGNOSTIC_SOURCE").unwrap();
    let dependency = std::env::var("OCR_DIAGNOSTIC_DEPENDENCY").unwrap();
    let map: super::ReviewMap =
        serde_json::from_str(&std::fs::read_to_string(root.join("review_map.json")).unwrap()).unwrap();
    let chunks = super::build_graph_chunks(&map, &root, 5, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap();
    let chunk = chunks.iter().find(|chunk| chunk.files.contains(&source)).unwrap();
    assert!(
        chunk.files.contains(&dependency)
            || chunk.global_context_files.contains(&dependency)
            || chunk.related_files.contains(&dependency),
        "Required direct dependency omitted from chunk {}",
        chunk.id
    );
    let patches = super::chunk_diff_bytes(&map, &std::fs::read_to_string(root.join("PR_DIFF.patch")).unwrap());
    for chunk in &chunks {
        super::chunk_context_budget(&map, &root, chunk, &patches, fixture_chunk_prompt_bytes(&root, chunk))
            .unwrap()
            .validate()
            .unwrap();
    }
    println!(
        "SAVED_CONTEXT_CHECK {}",
        serde_json::json!({"chunks":chunks.len(),"checkedChunk":chunk.id,"directDependencyPresent":true,"modelCalls":0})
    );
}

#[test]
fn graph_chunks_keep_direct_evidence_above_target_and_scope_the_graph() {
    let (root, mut map) = graph_chunk_fixture(-984, 20_000, 20_000);
    std::fs::create_dir_all(root.join("related")).unwrap();
    std::fs::write(root.join("related/large.rs"), "x".repeat(160_000)).unwrap();
    std::fs::write(root.join("related/small.rs"), "small evidence").unwrap();
    std::fs::write(root.join("related/b.rs"), "x".repeat(20_000)).unwrap();
    map.dependency_edges = (0..5_000)
        .map(|i| (format!("unrelated/{i}/caller.rs"), format!("unrelated/{i}/callee.rs")))
        .collect();
    map.dependency_edges.extend([
        ("a.rs".into(), "large.rs".into()),
        ("a.rs".into(), "small.rs".into()),
        ("a.rs".into(), "b.rs".into()),
    ]);
    super::write_review_map(&root, &map).unwrap();
    assert!(super::file_bytes(&root.join("review_map.json")) > crate::review::policy::TARGET_CONTEXT_BYTES as i64);
    let chunks = super::build_graph_chunks(&map, &root, 1, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap();
    assert_eq!(chunks.len(), 3);
    let chunk = &chunks[0];
    assert_eq!(chunk.files, ["a.rs"]);
    assert_eq!(chunk.related_files, ["large.rs", "small.rs"]);
    assert_eq!(chunk.global_context_files, ["b.rs"]);
    let scoped = super::chunk_review_map(&map, chunk);
    assert_eq!(scoped.dependency_edges.len(), 3);
    assert_eq!(scoped.reviewable_files, map.reviewable_files);
    let materialized = root.join("materialized");
    std::fs::create_dir_all(&materialized).unwrap();
    super::write_review_map(&materialized, &scoped).unwrap();
    for (from, to, paths) in [
        ("changed", "changed", &chunk.files),
        ("changed", "global", &chunk.global_context_files),
        ("related", "related", &chunk.related_files),
    ] {
        super::copy_dir_subset(&root.join(from), &materialized.join(to), paths);
    }
    super::copy_tree(&root.join("skill"), &materialized.join("skill"));
    let context = super::render_chunk_context(&map, chunk);
    assert!(context.contains("files not listed were not supplied"));
    std::fs::write(materialized.join("CONTEXT.md"), context).unwrap();
    let inputs = review_inputs("deep", 10, 5);
    let prompt = super::render_chunk_prompt(&inputs, &materialized, &root.join("summary.md"), "head", None, chunk);
    assert!(super::dir_bytes(&materialized) as usize > crate::review::policy::TARGET_CONTEXT_BYTES);
    crate::review::policy::validate_prepared_inputs(super::dir_bytes(&materialized) as usize, prompt.len()).unwrap();
    assert_eq!(
        std::fs::read(materialized.join("related/large.rs")).unwrap().len(),
        160_000
    );
    assert_eq!(std::fs::read(materialized.join("changed/a.rs")).unwrap().len(), 20_000);
    assert_eq!(map.dependency_edges.len(), 5_003);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_chunks_budget_deleted_file_diffs_and_reject_oversized_single_files() {
    let (root, map) = graph_chunk_fixture(-985, 20_000, 0);
    let diff = map
        .reviewable_files
        .iter()
        .map(|path| {
            format!(
                "diff --git a/{path} b/{path}\n--- a/{path}\n+++ /dev/null\n@@ -1 +0,0 @@\n-{}\n",
                "x".repeat(70_000)
            )
        })
        .collect::<String>();
    std::fs::write(root.join("PR_DIFF.patch"), &diff).unwrap();
    let chunks = super::build_graph_chunks(&map, &root, 5, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap();
    assert_eq!(chunks.len(), 2);
    std::fs::write(root.join("changed/a.rs"), "x".repeat(160_000)).unwrap();
    assert!(super::build_graph_chunks(&map, &root, 5, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).is_ok());
    std::fs::write(
        root.join("changed/a.rs"),
        "x".repeat(crate::review::policy::MAX_EVIDENCE_BYTES + 1),
    )
    .unwrap();
    let error =
        super::build_graph_chunks(&map, &root, 5, &|chunk| fixture_chunk_prompt_bytes(&root, chunk)).unwrap_err();
    assert!(error.contains("disk artifact limit"));
    assert!(error.contains("No model execution started"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn graph_chunk_diff_budget_excludes_noise_but_charges_unrecognized_headers() {
    let (root, mut map) = graph_chunk_fixture(-986, 0, 0);
    let diff = "diff --git \"a/\\303\\251.rs\" \"b/\\303\\251.rs\"\n--- /dev/null\n+++ \"b/\\303\\251.rs\"\n+new\n";
    let (sizes, shared) = super::chunk_diff_bytes(&map, diff);
    assert!(sizes.is_empty());
    assert_eq!(shared, diff.len());
    map.changed_files.push("pnpm-lock.yaml".into());
    let noise = format!(
        "diff --git a/pnpm-lock.yaml b/pnpm-lock.yaml\n--- a/pnpm-lock.yaml\n+++ b/pnpm-lock.yaml\n+{}\n",
        "x".repeat(200_000)
    );
    let (sizes, shared) = super::chunk_diff_bytes(&map, &noise);
    assert!(sizes.is_empty());
    assert_eq!(shared, 0);
    std::fs::write(root.join("PR_DIFF.patch"), noise).unwrap();
    assert_eq!(
        super::build_graph_chunks(&map, &root, 5, &|chunk| fixture_chunk_prompt_bytes(&root, chunk))
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sharded_chunks_group_related_tests_and_handlers() {
    let map = super::ReviewMap {
        dependency_edges: Vec::new(),
        pr_number: 7,
        title: "title".to_string(),
        description: String::new(),
        head_sha: "abc".to_string(),
        base_ref: "main".to_string(),
        changed_files: vec![],
        reviewable_files: vec![
            "apps/admin/tests/features/resolved-engagement-page.md".to_string(),
            "apps/admin/tests/resolved-engagement-page/handlers/resolved-engagements.ts".to_string(),
            "apps/admin/tests/resolved-engagement-page/resolved-engagements.test.ts".to_string(),
            "apps/customer/tests/resolved-engagement/handlers/resolved-engagement.ts".to_string(),
            "apps/customer/tests/resolved-engagement/resolved-engagements.test.ts".to_string(),
            "packages/features/engagement-list/src/utils/resolvedEngagementTimezone.ts".to_string(),
        ],
        omitted_noise_files: vec![],
        diff_stats: super::DiffStats {
            added: 1,
            removed: 0,
            files: 6,
        },
        categories: vec![],
        packages: vec![],
        dependent_packages: vec![],
        related_files: vec![],
        convention_files: vec![],
        http_refs: vec![],
        risk_triggers: vec![],
        data_lifecycle_triggers: vec![],
        public_contract_triggers: vec![],
        test_relationship_hints: vec![],
    };
    let chunks = super::build_review_chunks(&map, super::DEFAULT_REVIEW_CHUNK_SIZE);
    let admin_chunk = chunks
        .iter()
        .find(|chunk| {
            chunk
                .files
                .contains(&"apps/admin/tests/resolved-engagement-page/handlers/resolved-engagements.ts".to_string())
        })
        .expect("admin handler chunk");

    assert!(admin_chunk
        .files
        .contains(&"apps/admin/tests/resolved-engagement-page/resolved-engagements.test.ts".to_string()));
}

#[test]
fn side_by_side_batches_overlap_and_return_outputs_in_order() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let running = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let mut messages = Vec::new();
    let (outputs, usages) = super::run_side_by_side(
        "final",
        4,
        3,
        &AtomicBool::new(false),
        &mut |_, tail| messages.push(tail.to_string()),
        |index, _, progress| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            progress("{}", &format!("batch {index}"));
            // Later batches finish first, so the order comes from the index, not completion.
            std::thread::sleep(std::time::Duration::from_millis(40 * (4 - index as u64)));
            running.fetch_sub(1, Ordering::SeqCst);
            (Ok(index), Some(super::RunUsage::default()))
        },
    )
    .unwrap();
    assert_eq!(outputs, vec![0, 1, 2, 3]);
    assert_eq!(usages.len(), 4);
    assert_eq!(peak.load(Ordering::SeqCst), 3);
    assert_eq!(messages.len(), 4);
}

#[test]
fn a_failed_side_by_side_batch_cancels_the_others_and_reports_its_error() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let result = super::run_side_by_side(
        "final",
        5,
        2,
        &AtomicBool::new(false),
        &mut |_, _| {},
        |index, cancel, _| {
            if index == 1 {
                return (Err::<usize, _>("batch output invalid".into()), None);
            }
            while index == 0 && !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            if cancel.load(Ordering::Relaxed) {
                return (Err("cancelled".into()), None);
            }
            (Ok(index), None)
        },
    );
    assert_eq!(result.unwrap_err(), "batch output invalid");
}

#[test]
fn cancelling_the_review_cancels_side_by_side_batches() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let cancel = AtomicBool::new(true);
    let result = super::run_side_by_side("final", 2, 2, &cancel, &mut |_, _| {}, |_, batch_cancel, _| {
        while !batch_cancel.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        (Err::<usize, _>("cancelled".into()), None)
    });
    assert_eq!(result.unwrap_err(), "cancelled");
}

#[test]
fn a_waiting_batch_does_not_show_the_final_phase_as_queued_while_another_runs() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let started = AtomicUsize::new(0);
    let mut shown = Vec::new();
    super::run_side_by_side(
        "final",
        2,
        2,
        &AtomicBool::new(false),
        &mut |phase, tail| shown.push((tail.to_string(), phase.to_string())),
        |index, _, progress| {
            if index == 0 {
                progress(&super::sharded_phase_json("final", "running"), "batch 0");
                started.store(1, Ordering::SeqCst);
                while started.load(Ordering::SeqCst) < 2 {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            } else {
                while started.load(Ordering::SeqCst) < 1 {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                // Let batch 0's running status reach the collector before this one waits.
                std::thread::sleep(std::time::Duration::from_millis(50));
                progress(&super::sharded_phase_json("final", "queued"), "batch 1 waits");
                started.store(2, Ordering::SeqCst);
            }
            (Ok(index), None)
        },
    )
    .unwrap();
    // Batch 1's wait reaches the collector before batch 0 finishes (batch 0 waits for it), so it is
    // hidden behind batch 0's running phase. What follows depends on which Done arrives first.
    let (_, phase) = shown.iter().find(|(tail, _)| tail == "batch 1 waits").unwrap();
    assert!(
        phase.contains(r#""id":"final","label":"Integração final","status":"running""#),
        "{shown:?}"
    );
}

#[test]
fn a_batch_waiting_again_no_longer_counts_as_running() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let step = AtomicUsize::new(0);
    let wait_for = |at: usize| {
        while step.load(Ordering::SeqCst) < at {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Let the previous report reach the collector first.
        std::thread::sleep(std::time::Duration::from_millis(30));
    };
    let mut shown = Vec::new();
    super::run_side_by_side(
        "final",
        3,
        3,
        &AtomicBool::new(false),
        &mut |phase, tail| shown.push((tail.to_string(), phase.to_string())),
        |index, _, progress| {
            match index {
                0 => {
                    progress(&super::sharded_phase_json("final", "running"), "0 runs");
                    progress(&super::sharded_phase_json("final", "queued"), "0 waits for a repair");
                    step.store(1, Ordering::SeqCst);
                    wait_for(3);
                }
                2 => {
                    wait_for(1);
                    progress(&super::sharded_phase_json("final", "running"), "2 runs");
                    step.store(2, Ordering::SeqCst);
                }
                _ => {
                    wait_for(2);
                    // Wait until batch 2 reported done.
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    progress(&super::sharded_phase_json("final", "queued"), "1 waits");
                    step.store(3, Ordering::SeqCst);
                }
            }
            (Ok(index), None)
        },
    )
    .unwrap();
    let (tail, phase) = shown.last().unwrap();
    assert_eq!(tail, "1 waits");
    assert!(
        phase.contains(r#""id":"final","label":"Integração final","status":"queued""#),
        "every batch waits, so the phase shows queued: {shown:?}"
    );
}

#[test]
fn the_final_phase_shows_queued_once_the_last_running_batch_finishes() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let step = AtomicUsize::new(0);
    let wait_for = |at: usize| {
        while step.load(Ordering::SeqCst) < at {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    };
    let mut shown = Vec::new();
    super::run_side_by_side(
        "final",
        2,
        2,
        &AtomicBool::new(false),
        &mut |phase, tail| shown.push((tail.to_string(), phase.to_string())),
        |index, _, progress| {
            if index == 0 {
                progress(&super::sharded_phase_json("final", "running"), "0 runs");
                step.store(1, Ordering::SeqCst);
                wait_for(2);
            } else {
                wait_for(1);
                progress(&super::sharded_phase_json("final", "queued"), "1 waits");
                step.store(2, Ordering::SeqCst);
                // Still waiting well after batch 0 finished.
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            (Ok(index), None)
        },
    )
    .unwrap();
    let queued = r#""id":"final","label":"Integração final","status":"queued""#;
    assert!(!shown[1].1.contains(queued), "hidden while batch 0 runs: {shown:?}");
    let (tail, phase) = shown.last().unwrap();
    assert_eq!(tail, "1 waits");
    assert!(phase.contains(queued), "shown once nothing runs: {shown:?}");
}
