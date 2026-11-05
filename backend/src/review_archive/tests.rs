use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;

fn scope() -> Scope {
    Scope {
        host: "github.com".into(),
        repository: "owner/repo".into(),
        head_sha: "a".repeat(40),
        base_sha: "b".repeat(40),
        merge_base_sha: "c".repeat(40),
        prior_sha: None,
    }
}

fn source(path: &str, revision: &str, content: &[u8]) -> SourceEntry {
    SourceEntry {
        path: path.into(),
        revision: revision.into(),
        mode: "100644".into(),
        git_blob_id: hash(content)[..40].into(),
        bytes: content.len() as u64,
    }
}

fn build(files: &[(&str, &str, &[u8])]) -> Archive {
    let sources = files
        .iter()
        .map(|(path, revision, content)| source(path, revision, content))
        .collect();
    Archive::build(scope(), sources, &AtomicBool::new(false), |entry| {
        Ok(files
            .iter()
            .find(|(path, revision, _)| *path == entry.path && *revision == entry.revision)
            .unwrap()
            .2
            .to_vec())
    })
    .unwrap()
}

#[test]
fn archive_resolves_transitive_evidence_without_an_initial_package() {
    let head = scope().head_sha;
    let archive = build(&[
        ("src/test.rs", &head, b"call();\n"),
        ("src/dependency.rs", &head, b"guard();\n"),
        ("src/transitive.rs", &head, b"deny();\n"),
    ]);
    assert_eq!(archive.resolve(&head, "src/transitive.rs").unwrap(), "deny();\n");
    assert!(matches!(archive.resolve(&head, "src/missing.rs"), Err(Error::Absent)));
    assert!(matches!(
        archive.resolve(&scope().base_sha, "src/test.rs"),
        Err(Error::Revision)
    ));
}

#[test]
fn archive_identity_binds_scope_policy_and_all_evidence() {
    let head = scope().head_sha;
    let files = [
        ("test.rs", head.as_str(), b"test();\n".as_slice()),
        ("impl.rs", head.as_str(), b"allow();\n".as_slice()),
    ];
    let first = build(&files);
    let reordered = build(&[files[1], files[0]]);
    let changed = build(&[files[0], ("impl.rs", &head, b"deny();\n")]);
    assert_eq!(first.descriptor(), reordered.descriptor());
    assert_ne!(first.descriptor().id, changed.descriptor().id);
    let mut other = scope();
    other.repository = "other/repo".into();
    let alternate = Archive::build(
        other,
        vec![source("test.rs", &head, b"test();\n")],
        &AtomicBool::new(false),
        |_| Ok(b"test();\n".to_vec()),
    )
    .unwrap();
    assert_ne!(first.descriptor().id, alternate.descriptor().id);
}

#[test]
fn quoted_unicode_deletions_preserve_stage_scope_and_anchors() {
    let s = scope();
    let path = "src/café.rs";
    let archive = build(&[
        (path, &s.merge_base_sha, b"guard();\n"),
        ("other.rs", &s.head_sha, b"current();\n"),
    ]);
    let diff = "diff --git \"a/src/caf\\303\\251.rs\" \"b/src/caf\\303\\251.rs\"\n--- \"a/src/caf\\303\\251.rs\"\n+++ /dev/null\n@@ -1 +0,0 @@\n-guard();\n";
    archive
        .validate_assigned(&[path.into()], diff, &s.merge_base_sha)
        .unwrap();
    assert!(crate::review::diff_anchors(diff).contains(&(path.into(), 1, "LEFT".into())));
    assert!(archive
        .validate_assigned(&[path.into()], "", &s.merge_base_sha)
        .is_err());
    let mut files = BTreeMap::from([
        ("context/PR.md".into(), "Intent".into()),
        ("context/skill/SKILL.md".into(), "Review".into()),
        ("context/PR_DIFF.patch".into(), diff.into()),
        (
            "context/FROZEN.json".into(),
            serde_json::to_string(&archive.descriptor()).unwrap(),
        ),
    ]);
    let stage = Stage {
        files: vec![path.into()],
        before: s.merge_base_sha,
        snapshot_id: archive.descriptor().id,
        diff_hash: hash(diff.as_bytes()),
        context_hashes: Stage::hashes(&files),
    };
    files.insert("context/STAGE.json".into(), serde_json::to_string(&stage).unwrap());
    archive.validate_stage_files(&files).unwrap();
}

#[test]
fn stage_rejects_modified_missing_and_extra_context_inputs() {
    let directory = Directory::new();
    let context = directory.0.join("context");
    let archive = build(&[("code.rs", &scope().head_sha, b"call();\n")]);
    let inputs = [
        "PR.md",
        "skill/SKILL.md",
        "CONTEXT.md",
        "review_map.json",
        "external/task.txt",
        "conventions/AGENTS.md",
    ];
    for path in inputs {
        let target = context.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, "Prepared context").unwrap();
    }
    archive
        .prepare_stage(&context, &["code.rs".into()], "", &scope().merge_base_sha)
        .unwrap();
    let files: BTreeMap<String, String> = inputs
        .into_iter()
        .chain(["changed/code.rs", "PR_DIFF.patch", "FROZEN.json", "STAGE.json"])
        .map(|path| {
            (
                format!("context/{path}"),
                std::fs::read_to_string(context.join(path)).unwrap(),
            )
        })
        .collect();
    archive.validate_stage_files(&files).unwrap();
    for path in inputs {
        let key = format!("context/{path}");
        let mut changed = files.clone();
        changed.get_mut(&key).unwrap().push_str(" changed");
        assert!(archive.validate_stage_files(&changed).is_err(), "Altered {path}");
        changed.remove(&key);
        assert!(archive.validate_stage_files(&changed).is_err(), "Missing {path}");
    }
    let mut legacy = files.clone();
    let mut stage: serde_json::Value = serde_json::from_str(&legacy["context/STAGE.json"]).unwrap();
    stage.as_object_mut().unwrap().remove("contextHashes");
    legacy.insert("context/STAGE.json".into(), stage.to_string());
    assert!(matches!(archive.validate_stage_files(&legacy), Err(Error::Contract)));
    let mut extra = files;
    extra.insert("context/unprepared.txt".into(), "New instructions".into());
    assert!(archive.validate_stage_files(&extra).is_err());
}

#[test]
fn archive_keeps_revisions_and_renamed_paths_distinct() {
    let s = scope();
    let archive = build(&[
        ("old.rs", &s.merge_base_sha, b"before();\n"),
        ("new.rs", &s.head_sha, b"after();\n"),
        ("same.rs", &s.head_sha, b"current();\n"),
        ("same.rs", &s.merge_base_sha, b"prior();\n"),
    ]);
    assert_eq!(archive.resolve(&s.merge_base_sha, "old.rs").unwrap(), "before();\n");
    assert!(matches!(archive.resolve(&s.head_sha, "old.rs"), Err(Error::Absent)));
    assert!(matches!(
        archive.resolve(&s.merge_base_sha, "new.rs"),
        Err(Error::Absent)
    ));
    assert_eq!(archive.resolve(&s.head_sha, "same.rs").unwrap(), "current();\n");
    assert_eq!(archive.resolve(&s.merge_base_sha, "same.rs").unwrap(), "prior();\n");
}

#[test]
fn archive_never_loads_policy_exclusions() {
    let s = scope();
    let excluded = [
        ".env",
        ".ENV.example",
        "nested/.env.local",
        "credentials.toml",
        "config/hosts.yml",
        "key.pem",
        "nested/.claude/settings.local.json",
        ".gemini/oauth_creds.json",
        ".claude/.credentials.json",
        ".claude/projects/session.jsonl",
        ".git/config",
        "home/.ssh/config",
        "dist/code.js",
        "nested/build/code.js",
        "pnpm-lock.yaml",
        "file.generated.ts",
        "file.png",
        "module.so",
    ];
    let mut entries = excluded
        .iter()
        .map(|path| source(path, &s.head_sha, b"private"))
        .collect::<Vec<_>>();
    let mut link = source("link.rs", &s.head_sha, b"../outside");
    link.mode = "120000".into();
    entries.push(link);
    let mut large = source("large.rs", &s.head_sha, b"");
    large.bytes = MAX_FILE_BYTES + 1;
    entries.push(large);
    let archive = Archive::build(s.clone(), entries, &AtomicBool::new(false), |_| {
        panic!("Excluded content must not be opened")
    })
    .unwrap();
    assert!(archive.blobs().is_empty());
    for path in excluded.into_iter().chain(["link.rs", "large.rs"]) {
        assert!(
            matches!(archive.resolve(&s.head_sha, path), Err(Error::Excluded(_))),
            "{path}"
        );
    }
}

#[test]
fn committed_agent_instructions_are_reviewable() {
    for path in [
        ".claude/agents/reviewer.md",
        "src/user/.agents/skills/root-cause-tracing/find-polluter.sh",
        ".cursor/rules/style.mdc",
        "src/projects/main.rs",
    ] {
        assert_eq!(path_exclusion(path).unwrap(), None, "{path}");
    }
    for path in [
        ".claude/settings.json",
        ".cursor/mcp.json",
        ".mcp.json",
        "tools/.gemini/settings.json",
    ] {
        assert_eq!(
            path_exclusion(path).unwrap(),
            Some(Exclusion::PrivateConfiguration),
            "{path}"
        );
    }
}

#[test]
fn agent_folder_files_stay_out_of_the_runner_workspace() {
    let directory = Directory::new();
    let context = directory.0.join("context");
    let agent = ".claude/agents/reviewer.md";
    let archive = build(&[
        ("code.rs", &scope().head_sha, b"call();\n"),
        (agent, &scope().head_sha, b"Review carefully.\n"),
    ]);
    for path in ["PR.md", "skill/SKILL.md", "related/.agents/skills/a/SKILL.md"] {
        let target = context.join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, "Prepared context").unwrap();
    }
    archive
        .prepare_stage(&context, &[agent.into(), "code.rs".into()], "", &scope().merge_base_sha)
        .unwrap();
    assert!(context.join("changed/code.rs").is_file());
    assert!(!context.join("changed/.claude").exists());
    assert!(!context.join("related/.agents/skills/a/SKILL.md").exists());
    let files: BTreeMap<String, String> = [
        "PR.md",
        "skill/SKILL.md",
        "changed/code.rs",
        "PR_DIFF.patch",
        "FROZEN.json",
        "STAGE.json",
    ]
    .into_iter()
    .map(|path| {
        (
            format!("context/{path}"),
            std::fs::read_to_string(context.join(path)).unwrap(),
        )
    })
    .collect();
    archive.validate_stage_files(&files).unwrap();
    assert_eq!(
        archive.resolve(&scope().head_sha, agent).unwrap(),
        "Review carefully.\n"
    );
}

#[test]
fn archive_rejects_unsafe_paths_duplicate_entries_and_unpinned_revisions() {
    let s = scope();
    for path in [
        "",
        "../file.rs",
        "/file.rs",
        "a/../b.rs",
        "a//b.rs",
        "a/./b.rs",
        "a\\b.rs",
        "a\nb.rs",
    ] {
        assert!(Archive::build(
            s.clone(),
            vec![source(path, &s.head_sha, b"x")],
            &AtomicBool::new(false),
            |_| panic!("Unsafe source opened")
        )
        .is_err());
    }
    let entry = source("a.rs", &s.head_sha, b"x");
    assert!(
        Archive::build(s.clone(), vec![entry.clone(), entry], &AtomicBool::new(false), |_| Ok(
            b"x".to_vec()
        ))
        .is_err()
    );
    assert!(Archive::build(
        s,
        vec![source("a.rs", "main", b"x")],
        &AtomicBool::new(false),
        |_| panic!("Unpinned source opened")
    )
    .is_err());
}

#[test]
fn archive_detects_text_failures_without_truncating_or_hiding_io_errors() {
    let head = scope().head_sha;
    let archive = build(&[
        ("nul.rs", &head, b"a\0b"),
        ("invalid.rs", &head, &[255]),
        ("ok.rs", &head, b"\xef\xbb\xbfhello\r\n"),
    ]);
    assert!(matches!(
        archive.resolve(&head, "nul.rs"),
        Err(Error::Excluded(Exclusion::Binary))
    ));
    assert!(matches!(
        archive.resolve(&head, "invalid.rs"),
        Err(Error::Excluded(Exclusion::NonUtf8))
    ));
    assert_eq!(
        archive.resolve(&head, "ok.rs").unwrap().as_bytes(),
        b"\xef\xbb\xbfhello\r\n"
    );
    let entries = vec![source("a.rs", &head, b"abc")];
    assert!(matches!(
        Archive::build(scope(), entries.clone(), &AtomicBool::new(false), |_| Err(
            Error::Preparation
        )),
        Err(Error::Preparation)
    ));
    assert!(matches!(
        Archive::build(scope(), entries, &AtomicBool::new(false), |_| Ok(b"ab".to_vec())),
        Err(Error::Integrity)
    ));
}

#[test]
fn archive_roundtrip_rejects_tampering_and_unknown_contracts() {
    let head = scope().head_sha;
    let archive = build(&[("a.rs", &head, b"abc"), ("b.rs", &head, b"def")]);
    let descriptor = archive.descriptor();
    let encoded = archive.manifest_bytes().unwrap();
    let restored = Archive::decode(&descriptor, &encoded, archive.blobs().to_vec()).unwrap();
    assert_eq!(restored.resolve(&head, "b.rs").unwrap(), "def");
    let mut blobs = archive.blobs().to_vec();
    blobs[0] ^= 1;
    assert!(Archive::decode(&descriptor, &encoded, blobs).is_err());
    let mut manifest: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    manifest["version"] = 999.into();
    let invalid = serde_json::to_vec(&manifest).unwrap();
    let mut descriptor = descriptor;
    descriptor.id = hash(&invalid);
    descriptor.manifest_bytes = invalid.len() as u64;
    assert!(Archive::decode(&descriptor, &invalid, archive.blobs().to_vec()).is_err());
}

#[test]
fn archive_deduplicates_blobs_not_logical_entries() {
    let s = scope();
    let archive = build(&[
        ("a.rs", &s.head_sha, b"same"),
        ("b.rs", &s.head_sha, b"same"),
        ("a.rs", &s.merge_base_sha, b"same"),
    ]);
    assert_eq!(archive.descriptor().entry_count, 3);
    assert_eq!(archive.descriptor().blob_bytes, 4);
    assert_eq!(archive.descriptor().expanded_bytes, 12);
    let manifest: Manifest = serde_json::from_slice(&archive.manifest_bytes().unwrap()).unwrap();
    assert_eq!(manifest.blobs.len(), 1);
}

#[test]
fn archive_compacts_repeated_metadata_without_dropping_revision_entries() {
    let mut s = scope();
    s.prior_sha = Some("d".repeat(40));
    let sources = s
        .revisions()
        .into_iter()
        .flat_map(|revision| {
            (0..4_557).map(move |index| {
                source(
                    &format!("src/features/ontology/components/training/consumers/consumer-{index:04}.tsx"),
                    revision,
                    b"source\n",
                )
            })
        })
        .collect();
    let archive = Archive::build(
        s.clone(),
        sources,
        &AtomicBool::new(false),
        |_| Ok(b"source\n".to_vec()),
    )
    .unwrap();
    assert_eq!(archive.descriptor().entry_count, 13_671);
    assert_eq!(archive.descriptor().expanded_bytes, 13_671 * 7);
    assert!(serde_json::to_vec(&archive.manifest).unwrap().len() as u64 > COMPACT_MANIFEST_BYTES);
    assert!(archive.descriptor().manifest_bytes <= COMPACT_MANIFEST_BYTES);
    let raw = archive.manifest_bytes().unwrap();
    let mut incoming = Incoming::new(archive.descriptor(), raw.clone()).unwrap();
    for (digest, bytes) in archive.transfer_blobs() {
        incoming.push(digest, bytes).unwrap();
    }
    let restored = incoming.finish().unwrap();
    assert_eq!(restored.descriptor(), archive.descriptor());
    assert_eq!(restored.manifest_bytes().unwrap(), raw);
    for entry in archive.entries() {
        assert_eq!(
            restored.resolve(&entry.source.revision, &entry.source.path).unwrap(),
            "source\n"
        );
    }
    let directory = Directory::new();
    let mut store = ArchiveStore::open(
        &directory.0.join("store"),
        &crate::db::Db::for_test_at(directory.0.join("store")),
    )
    .unwrap();
    store.deposit(&archive).unwrap();
    assert_eq!(
        store.load(&archive.descriptor()).unwrap().manifest_bytes().unwrap(),
        raw
    );
}

fn compact_archive(archive: &Archive) -> Archive {
    let raw = manifest::encode(&archive.manifest, true).unwrap();
    let mut descriptor = archive.descriptor();
    descriptor.id = hash(&raw);
    descriptor.manifest_bytes = raw.len() as u64;
    Archive::decode(&descriptor, &raw, archive.blobs().to_vec()).unwrap()
}

#[test]
fn compact_archive_preserves_changes_exclusions_and_legacy_identity() {
    let s = scope();
    let archive = build(&[
        ("same.rs", &s.head_sha, b"same"),
        ("same.rs", &s.merge_base_sha, b"same"),
        ("changed.rs", &s.head_sha, b"after"),
        ("changed.rs", &s.merge_base_sha, b"before"),
        ("deleted.rs", &s.merge_base_sha, b"gone"),
        (".env", &s.head_sha, b"fixture"),
        (".env", &s.merge_base_sha, b"fixture"),
        ("binary.rs", &s.head_sha, b"\0"),
    ]);
    let legacy = archive.manifest_bytes().unwrap();
    assert_eq!(legacy, serde_json::to_vec(&archive.manifest).unwrap());
    assert!(!archive.protocol().contains("entryGroups"));
    let compact = compact_archive(&archive);
    assert!(compact.protocol().contains("entryGroups"));
    assert_ne!(compact.descriptor().id, archive.descriptor().id);
    assert_eq!(serde_json::to_vec(&compact.manifest).unwrap(), legacy);
    assert_eq!(compact.blobs(), archive.blobs());
    for entry in archive.entries() {
        assert_eq!(
            compact.resolve(&entry.source.revision, &entry.source.path),
            archive.resolve(&entry.source.revision, &entry.source.path)
        );
    }
    assert!(matches!(compact.resolve(&s.head_sha, "deleted.rs"), Err(Error::Absent)));
    let directory = Directory::new();
    let target = directory.0.join("snapshot");
    compact.materialize_new(&target, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        std::fs::read(target.join("trees").join(&s.merge_base_sha).join("changed.rs")).unwrap(),
        b"before"
    );
    assert!(!target.join("trees").join(&s.head_sha).join(".env").exists());
    assert_eq!(
        std::fs::read(target.join("manifest.json")).unwrap(),
        compact.manifest_bytes().unwrap()
    );
    let mut incoming = Incoming::new(archive.descriptor(), legacy.clone()).unwrap();
    for (digest, bytes) in archive.transfer_blobs() {
        incoming.push(digest, bytes).unwrap();
    }
    let restored = incoming.finish().unwrap();
    assert_eq!(restored.descriptor(), archive.descriptor());
    assert_eq!(restored.manifest_bytes().unwrap(), legacy);
}

#[test]
fn compact_archive_rejects_rehashed_invalid_revision_groups_and_expansion() {
    let s = scope();
    let archive = compact_archive(&build(&[
        ("same.rs", &s.head_sha, b"same"),
        ("same.rs", &s.merge_base_sha, b"same"),
    ]));
    let original = String::from_utf8(archive.manifest_bytes().unwrap()).unwrap();
    let revisions = format!("\"additionalRevisions\":[\"{}\"]", s.merge_base_sha);
    for invalid in [
        vec![s.head_sha.clone()],
        vec![s.base_sha.clone()],
        vec![s.merge_base_sha.clone(), s.merge_base_sha.clone()],
        vec![s.merge_base_sha.clone(), s.head_sha.clone()],
    ] {
        let raw = original
            .replace(
                &revisions,
                &format!("\"additionalRevisions\":{}", serde_json::to_string(&invalid).unwrap()),
            )
            .into_bytes();
        let mut descriptor = archive.descriptor();
        descriptor.id = hash(&raw);
        descriptor.manifest_bytes = raw.len() as u64;
        assert!(Archive::decode(&descriptor, &raw, archive.blobs().to_vec()).is_err());
        assert!(transfer_manifest(&descriptor, &raw).is_err());
    }
    let excessive = vec![s.merge_base_sha; MAX_ENTRIES];
    let raw = original
        .replace(
            &revisions,
            &format!("\"additionalRevisions\":{}", serde_json::to_string(&excessive).unwrap()),
        )
        .into_bytes();
    let mut descriptor = archive.descriptor();
    descriptor.id = hash(&raw);
    descriptor.manifest_bytes = raw.len() as u64;
    assert!(matches!(
        transfer_manifest(&descriptor, &raw),
        Err(Error::Limit {
            resource: "manifest entries",
            ..
        })
    ));
    assert!(matches!(
        Archive::decode(&descriptor, &raw, archive.blobs().to_vec()),
        Err(Error::Limit {
            resource: "manifest entries",
            ..
        })
    ));
    let mut descriptor = archive.descriptor();
    descriptor.expanded_bytes -= 1;
    assert!(transfer_manifest(&descriptor, original.as_bytes()).is_err());
    assert!(Archive::decode(&descriptor, original.as_bytes(), archive.blobs().to_vec()).is_err());
}

#[test]
fn pinned_dependency_manifest_roundtrip_rejects_tampering_and_preserves_budgets() {
    let s = scope();
    let revision = "d".repeat(40);
    let dependency_scope = Scope {
        host: s.host.clone(),
        repository: "owner/dependency".into(),
        head_sha: revision.clone(),
        base_sha: revision.clone(),
        merge_base_sha: revision.clone(),
        prior_sha: None,
    };
    let dependency = || {
        Archive::build(
            dependency_scope.clone(),
            vec![source("guard.rs", &revision, b"guard();\n")],
            &AtomicBool::new(false),
            |_| Ok(b"guard();\n".to_vec()),
        )
        .unwrap()
    };
    let primary = || build(&[("code.rs", &s.head_sha, b"guard();\n")]);
    let archive = primary().with_dependencies(vec![dependency()]).unwrap();
    assert_eq!(archive.descriptor().blob_bytes, primary().descriptor().blob_bytes);
    assert_eq!(
        archive.descriptor().expanded_bytes,
        primary().descriptor().expanded_bytes * 2
    );
    assert_eq!(archive.descriptor().entry_count, 2);
    let compact = compact_archive(&archive);
    assert_eq!(
        compact
            .resolve_dependency("owner/dependency", &revision, "guard.rs")
            .unwrap(),
        "guard();\n"
    );
    assert_eq!(
        transfer_manifest(&compact.descriptor(), &compact.manifest_bytes().unwrap())
            .unwrap()
            .entry_count(),
        2
    );
    assert!(primary().with_dependencies(vec![dependency(), dependency()]).is_err());
    for case in ["repository", "revision", "path", "hash", "bytes", "count"] {
        let mut manifest = archive.manifest.clone();
        match case {
            "repository" => manifest.dependencies[0].repository = "../outside".into(),
            "revision" => manifest.dependencies[0].entries[0].source.revision = s.head_sha.clone(),
            "path" => manifest.dependencies[0].entries[0].source.path = "../../outside".into(),
            "hash" => manifest.dependencies[0].entries[0].content_hash = Some("e".repeat(64)),
            "bytes" => manifest.dependencies[0].entries[0].source.bytes += 1,
            "count" => {
                let duplicate = manifest.dependencies[0].entries[0].clone();
                manifest.dependencies[0].entries.push(duplicate);
            }
            _ => unreachable!(),
        }
        let raw = serde_json::to_vec(&manifest).unwrap();
        let mut descriptor = archive.descriptor();
        descriptor.id = hash(&raw);
        descriptor.manifest_bytes = raw.len() as u64;
        assert!(transfer_manifest(&descriptor, &raw).is_err(), "{case}");
        assert!(
            Archive::decode(&descriptor, &raw, archive.blobs().to_vec()).is_err(),
            "{case}"
        );
    }
    let mut descriptor = archive.descriptor();
    descriptor.expanded_bytes -= 1;
    assert!(transfer_manifest(&descriptor, &archive.manifest_bytes().unwrap()).is_err());
}

#[test]
fn archive_preparation_observes_cancellation_before_opening_content() {
    let head = scope().head_sha;
    let entries = vec![source("a.rs", &head, b"abc")];
    assert!(matches!(
        Archive::build(scope(), entries, &AtomicBool::new(true), |_| panic!(
            "Cancelled source opened"
        )),
        Err(Error::Cancelled)
    ));
}

#[test]
fn archive_limits_are_inclusive_and_independent() {
    assert!(validate_limits(MAX_BLOB_BYTES, MAX_MANIFEST_BYTES, MAX_ENTRIES).is_ok());
    for (blob, manifest, entries) in [
        (MAX_BLOB_BYTES + 1, 0, 0),
        (0, MAX_MANIFEST_BYTES + 1, 0),
        (0, 0, MAX_ENTRIES + 1),
    ] {
        let error = validate_limits(blob, manifest, entries).unwrap_err();
        assert!(matches!(error, Error::Limit { actual, limit, .. } if actual == limit + 1));
        assert!(error.to_string().contains("exceeds its approved budget:"));
    }
}

#[test]
fn archive_descriptor_and_offsets_are_not_trusted_even_when_rehashed() {
    let head = scope().head_sha;
    let archive = build(&[("a.rs", &head, b"abc"), ("b.rs", &head, b"def")]);
    let value: serde_json::Value = serde_json::from_slice(&archive.manifest_bytes().unwrap()).unwrap();
    for (field, replacement) in [("offset", serde_json::json!(0)), ("bytes", serde_json::json!(u64::MAX))] {
        let mut invalid = value.clone();
        invalid["blobs"][1][field] = replacement;
        let raw = serde_json::to_vec(&invalid).unwrap();
        let mut descriptor = archive.descriptor();
        descriptor.id = hash(&raw);
        descriptor.manifest_bytes = raw.len() as u64;
        assert!(Archive::decode(&descriptor, &raw, archive.blobs().to_vec()).is_err());
    }
    let mut descriptor = archive.descriptor();
    descriptor.expanded_bytes += 1;
    assert!(Archive::decode(
        &descriptor,
        &archive.manifest_bytes().unwrap(),
        archive.blobs().to_vec()
    )
    .is_err());
}

#[test]
fn archive_maps_metadata_to_content_without_opening_excluded_paths() {
    let s = scope();
    let sources = BTreeMap::from([("src/test.rs", "call();\n"), ("src/impl.rs", "guard();\n")]);
    let entries = sources
        .iter()
        .map(|(path, content)| source(path, &s.head_sha, content.as_bytes()))
        .collect();
    let archive = Archive::build(s.clone(), entries, &AtomicBool::new(false), |entry| {
        Ok(sources[entry.path.as_str()].as_bytes().to_vec())
    })
    .unwrap();
    assert_eq!(archive.resolve(&s.head_sha, "src/impl.rs").unwrap(), "guard();\n");
}

#[test]
fn archive_transfer_checks_order_integrity_duplicates_and_completeness() {
    let head = scope().head_sha;
    let archive = build(&[("a.rs", &head, b"first"), ("b.rs", &head, b"second")]);
    let blobs = archive.transfer_blobs().collect::<Vec<_>>();
    let mut incoming = Incoming::new(archive.descriptor(), archive.manifest_bytes().unwrap()).unwrap();
    assert!(incoming.push(blobs[1].0, blobs[1].1).is_err());
    assert!(incoming.push(blobs[0].0, b"wrong").is_err());
    incoming.push(blobs[0].0, blobs[0].1).unwrap();
    incoming.push(blobs[0].0, blobs[0].1).unwrap();
    incoming.push(blobs[1].0, blobs[1].1).unwrap();
    assert_eq!(incoming.finish().unwrap().descriptor(), archive.descriptor());
    assert!(Incoming::new(archive.descriptor(), archive.manifest_bytes().unwrap())
        .unwrap()
        .finish()
        .is_err());
}

#[test]
fn archive_store_publishes_atomically_and_preserves_sealed_data_on_abort() {
    let directory = Directory::new();
    let head = scope().head_sha;
    let archive = build(&[("a.rs", &head, b"source")]);
    let descriptor = archive.descriptor();
    let mut store = ArchiveStore::open(
        &directory.0.join("store"),
        &crate::db::Db::for_test_at(directory.0.join("store")),
    )
    .unwrap();
    let owner = hash(b"owner");
    assert!(!store
        .begin(
            &owner,
            &descriptor,
            &archive.manifest_bytes().unwrap(),
            crate::sandbox::now() + 90_000
        )
        .unwrap());
    assert!(store.load(&descriptor).is_err());
    assert!(store.seal(&owner).is_err());
    for (hash, bytes) in archive.transfer_blobs() {
        store.put(&owner, hash, bytes).unwrap();
        store.put(&owner, hash, bytes).unwrap();
    }
    assert_eq!(store.seal(&owner).unwrap(), descriptor);
    assert_eq!(store.seal(&owner).unwrap(), descriptor);
    store.abort(&owner).unwrap();
    assert_eq!(
        store.load(&descriptor).unwrap().resolve(&head, "a.rs").unwrap(),
        "source"
    );
    assert!(store
        .begin(
            &hash(b"other"),
            &descriptor,
            &archive.manifest_bytes().unwrap(),
            crate::sandbox::now() + 90_000
        )
        .unwrap());
    drop(store);
    let store = ArchiveStore::open(
        &directory.0.join("store"),
        &crate::db::Db::for_test_at(directory.0.join("store")),
    )
    .unwrap();
    assert_eq!(store.load(&descriptor).unwrap().descriptor(), descriptor);
}

#[test]
fn archive_store_recovers_interrupted_deposits_without_evicting_history() {
    let directory = Directory::new();
    let root = directory.0.join("store");
    let history = build(&[("a.rs", &scope().head_sha, b"history")]);
    let orphan = build(&[("a.rs", &scope().head_sha, b"interrupted")]);
    let mut store = ArchiveStore::open(&root, &crate::db::Db::for_test_at(&root)).unwrap();
    store.deposit(&history).unwrap();
    store.link("review:published", &history.descriptor()).unwrap();
    let uploading = build(&[("a.rs", &scope().head_sha, b"uploading")]);
    store
        .begin(
            &hash(b"active-upload"),
            &uploading.descriptor(),
            &uploading.manifest_bytes().unwrap(),
            crate::sandbox::now() + 90_000,
        )
        .unwrap();
    let uploading_path = root
        .join("sealed")
        .join(format!("{}.archive", uploading.descriptor().id));
    uploading.write_new(&uploading_path).unwrap();
    drop(store);
    let staged = build(&[("a.rs", &scope().head_sha, b"staged")]);
    let staged_path = root.join("sealed").join(format!(".publish-{}", staged.descriptor().id));
    staged.write_new(&staged_path).unwrap();
    let orphan_path = root.join("sealed").join(format!("{}.archive", orphan.descriptor().id));
    let staging = root.join("sealed").join(format!(".publish-{}", orphan.descriptor().id));
    orphan.write_new(&staging).unwrap();
    std::fs::rename(staging, &orphan_path).unwrap();
    let mut store = ArchiveStore::open(&root, &crate::db::Db::for_test_at(&root)).unwrap();
    assert!(!staged_path.exists());
    assert!(!orphan_path.exists());
    assert!(uploading_path.exists());
    assert_eq!(store.load(&history.descriptor()).unwrap().blobs(), history.blobs());
    assert_eq!(
        store.load_linked("review:published").unwrap().unwrap().descriptor(),
        history.descriptor()
    );
    assert!(store.load_linked("review:missing").unwrap().is_none());
    store.deposit(&orphan).unwrap();
    drop(store);
    assert!(ArchiveStore::open(&root, &crate::db::Db::for_test_at(&root))
        .unwrap()
        .load(&orphan.descriptor())
        .is_ok());
}

#[test]
fn archive_store_rejects_expired_uploads_and_quota_without_evicting_history() {
    let directory = Directory::new();
    let archive = build(&[("a.rs", &scope().head_sha, b"source")]);
    let mut store = ArchiveStore::open(
        &directory.0.join("store"),
        &crate::db::Db::for_test_at(directory.0.join("store")),
    )
    .unwrap();
    let descriptor = archive.descriptor();
    let owner = hash(b"expired");
    assert!(store
        .begin(&owner, &descriptor, &archive.manifest_bytes().unwrap(), 0)
        .is_err());
    store
        .begin(
            &owner,
            &descriptor,
            &archive.manifest_bytes().unwrap(),
            crate::sandbox::now() + 90_000,
        )
        .unwrap();
    let conn = crate::db::Db::for_test_at(directory.0.join("store")).open().unwrap();
    conn.execute("UPDATE archive.uploads SET deadline=0", []).unwrap();
    let (digest, bytes) = archive.transfer_blobs().next().unwrap();
    assert!(store.put(&owner, digest, bytes).is_err());
    store.expire().unwrap();
    assert!(!directory.0.join("store/pending").join(&owner).exists());
    store.deposit(&archive).unwrap();
    conn.execute("UPDATE archive.artifacts SET bytes=$1", [STORE_QUOTA as i64])
        .unwrap();
    let other = build(&[("a.rs", &scope().head_sha, b"changed")]);
    assert!(matches!(
        store.begin(
            &hash(b"full"),
            &other.descriptor(),
            &other.manifest_bytes().unwrap(),
            crate::sandbox::now() + 90_000
        ),
        Err(Error::Budget)
    ));
    assert!(store.load(&descriptor).is_ok());
}

#[test]
fn concurrent_archive_reservations_cannot_overbook_storage() {
    let directory = Directory::new();
    let path = directory.0.join("store");
    let historical = build(&[("a.rs", &scope().head_sha, b"history")]);
    let mut store = ArchiveStore::open(&path, &crate::db::Db::for_test_at(&path)).unwrap();
    store.deposit(&historical).unwrap();
    let conn = crate::db::Db::for_test_at(&path).open().unwrap();
    conn.execute(
        "UPDATE archive.artifacts SET bytes=$1",
        [(STORE_QUOTA - (MAX_BLOB_BYTES + MAX_MANIFEST_BYTES + 128 * 1024 * 1024)) as i64],
    )
    .unwrap();
    let next = build(&[("a.rs", &scope().head_sha, b"next")]);
    let successes = std::thread::scope(|scope| {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for index in 0..2 {
            let (path, next, barrier) = (path.clone(), &next, barrier.clone());
            handles.push(scope.spawn(move || {
                let mut store = ArchiveStore::open(&path, &crate::db::Db::for_test_at(&path)).unwrap();
                barrier.wait();
                store
                    .begin(
                        &hash(format!("owner-{index}").as_bytes()),
                        &next.descriptor(),
                        &next.manifest_bytes().unwrap(),
                        crate::sandbox::now() + 90_000,
                    )
                    .is_ok()
            }));
        }
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|success| *success)
            .count()
    });
    assert_eq!(successes, 1);
    assert!(store.load(&historical.descriptor()).is_ok());
}

#[test]
fn materialized_archive_is_complete_and_stage_rejects_missing_sources() {
    let directory = Directory::new();
    let s = scope();
    let archive = build(&[
        ("a.rs", &s.head_sha, b"source\r\n"),
        ("deleted.rs", &s.merge_base_sha, b"before\n"),
        (".env", &s.head_sha, b"fixture"),
    ]);
    let target = directory.0.join("snapshot");
    archive.materialize_new(&target, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        std::fs::read(target.join("trees").join(&s.head_sha).join("a.rs")).unwrap(),
        b"source\r\n"
    );
    assert!(!target.join("trees").join(&s.head_sha).join(".env").exists());
    assert!(archive.materialize_new(&target, &AtomicBool::new(false)).is_err());
    let mut repeated = scope();
    repeated.prior_sha = Some("d".repeat(40));
    let prior = repeated.prior_sha.clone().unwrap();
    let shared = Archive::build(
        repeated.clone(),
        vec![
            source("big.rs", &repeated.head_sha, b"same body\n"),
            source("big.rs", &repeated.merge_base_sha, b"same body\n"),
            source("big.rs", &prior, b"same body\n"),
            source("other.rs", &repeated.head_sha, b"changed\n"),
        ],
        &AtomicBool::new(false),
        |entry| {
            Ok(if entry.path == "other.rs" {
                b"changed\n".to_vec()
            } else {
                b"same body\n".to_vec()
            })
        },
    )
    .unwrap();
    let linked = directory.0.join("linked");
    shared.materialize_new(&linked, &AtomicBool::new(false)).unwrap();
    use std::os::unix::fs::MetadataExt;
    let inode = |revision: &str, path: &str| std::fs::metadata(linked.join("trees").join(revision).join(path)).unwrap();
    assert_eq!(
        (
            inode(&prior, "big.rs").ino(),
            inode(&repeated.merge_base_sha, "big.rs").ino(),
            inode(&prior, "big.rs").nlink()
        ),
        (
            inode(&repeated.head_sha, "big.rs").ino(),
            inode(&repeated.head_sha, "big.rs").ino(),
            3
        )
    );
    assert_eq!(
        std::fs::read(linked.join("trees").join(&prior).join("big.rs")).unwrap(),
        b"same body\n"
    );
    assert_ne!(
        inode(&repeated.head_sha, "other.rs").ino(),
        inode(&repeated.head_sha, "big.rs").ino()
    );
    assert!(archive
        .validate_assigned(&["missing.rs".into()], "", &s.merge_base_sha)
        .is_err());
    assert!(archive
        .validate_assigned(&[".env".into()], "", &s.merge_base_sha)
        .is_err());
    let deletion = "diff --git a/deleted.rs b/deleted.rs\n--- a/deleted.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-before\n";
    archive
        .validate_assigned(&["deleted.rs".into()], deletion, &s.merge_base_sha)
        .unwrap();
    assert!(archive
        .validate_assigned(&["deleted.rs".into()], "", &s.merge_base_sha)
        .is_err());
}

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ocr-archive-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn archive_storage_is_bounded_and_never_overwrites_an_existing_file() {
    let directory = Directory::new();
    let head = scope().head_sha;
    let archive = build(&[("src/a.rs", &head, b"complete\r\n")]);
    let path = directory.0.join("sealed.archive");
    archive.write_new(&path).unwrap();
    assert!(archive.write_new(&path).is_err());
    let loaded = Archive::read(&path, &archive.descriptor()).unwrap();
    assert_eq!(loaded.resolve(&head, "src/a.rs").unwrap(), "complete\r\n");
    let mut descriptor = archive.descriptor();
    descriptor.blob_bytes += 1;
    assert!(Archive::read(&path, &descriptor).is_err());
    std::fs::write(directory.0.join("partial.archive"), b"OCRARCH1").unwrap();
    assert!(Archive::read(&directory.0.join("partial.archive"), &archive.descriptor()).is_err());
}

#[cfg(unix)]
#[test]
fn archive_storage_rejects_symlinks_in_files_and_parent_directories() {
    let directory = Directory::new();
    let head = scope().head_sha;
    let archive = build(&[("a.rs", &head, b"source")]);
    let path = directory.0.join("sealed.archive");
    archive.write_new(&path).unwrap();
    let redirected = directory.0.join("redirected.archive");
    std::os::unix::fs::symlink(&path, &redirected).unwrap();
    assert!(Archive::read(&redirected, &archive.descriptor()).is_err());
    assert!(archive.write_new(&redirected).is_err());
    let parent = directory.0.join("redirected");
    std::os::unix::fs::symlink(&directory.0, &parent).unwrap();
    assert!(Archive::read(&parent.join("sealed.archive"), &archive.descriptor()).is_err());
    assert!(archive.write_new(&parent.join("other.archive")).is_err());
    assert!(!directory.0.join("other.archive").exists());
}

#[test]
fn archive_git_capture_uses_pinned_objects_not_checkout_or_replace_refs() {
    let directory = Directory::new();
    let repo = &directory.0;
    let run = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.name=Archive Test",
                "-c",
                "user.email=archive@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    run(&["init", "--quiet"]);
    std::fs::write(repo.join("implementation.rs"), "before();\n").unwrap();
    std::fs::write(repo.join("old.rs"), "renamed();\n").unwrap();
    std::fs::write(repo.join(".env.example"), "FIXTURE=placeholder\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "--quiet", "-m", "Before"]);
    let before = run(&["rev-parse", "HEAD"]);
    std::fs::write(repo.join("implementation.rs"), "committed();\n").unwrap();
    std::fs::rename(repo.join("old.rs"), repo.join("new.rs")).unwrap();
    run(&["add", "."]);
    run(&["commit", "--quiet", "-m", "After"]);
    let head = run(&["rev-parse", "HEAD"]);
    let blob = run(&["rev-parse", &format!("{head}:implementation.rs")]);
    run(&["replace", &head, &before]);
    let refs = run(&["for-each-ref", "--format=%(refname) %(objectname)"]);
    std::fs::write(repo.join("implementation.rs"), "uncommitted();\n").unwrap();
    let scope = Scope {
        host: "github.com".into(),
        repository: "owner/repo".into(),
        head_sha: head.clone(),
        base_sha: before.clone(),
        merge_base_sha: before.clone(),
        prior_sha: None,
    };
    let archive = capture(repo, scope.clone(), &AtomicBool::new(false)).unwrap();
    assert_eq!(archive.resolve(&head, "implementation.rs").unwrap(), "committed();\n");
    assert_eq!(archive.resolve(&before, "implementation.rs").unwrap(), "before();\n");
    assert_eq!(archive.resolve(&head, "new.rs").unwrap(), "renamed();\n");
    assert!(matches!(archive.resolve(&head, "old.rs"), Err(Error::Absent)));
    assert!(matches!(
        archive.resolve(&head, ".env.example"),
        Err(Error::Excluded(Exclusion::PrivatePath))
    ));
    assert_eq!(run(&["for-each-ref", "--format=%(refname) %(objectname)"]), refs);
    assert_eq!(
        std::fs::read_to_string(repo.join("implementation.rs")).unwrap(),
        "uncommitted();\n"
    );
    std::fs::remove_file(repo.join(".git/objects").join(&blob[..2]).join(&blob[2..])).unwrap();
    assert_eq!(
        capture(repo, scope.clone(), &AtomicBool::new(false)).err(),
        Some(Error::Preparation)
    );
    assert!(matches!(
        capture(repo, scope, &AtomicBool::new(true)),
        Err(Error::Cancelled)
    ));
}

#[test]
fn pinned_dependency_provenance_roundtrip_legacy_compatibility_and_validation() {
    let s = scope();
    let revision = "d".repeat(40);
    let dependency_scope = Scope {
        host: s.host.clone(),
        repository: "owner/dependency".into(),
        head_sha: revision.clone(),
        base_sha: revision.clone(),
        merge_base_sha: revision.clone(),
        prior_sha: None,
    };
    let dep_archive = Archive::build(
        dependency_scope,
        vec![source("guard.rs", &revision, b"guard();\n")],
        &AtomicBool::new(false),
        |_| Ok(b"guard();\n".to_vec()),
    )
    .unwrap();
    let primary = || build(&[("code.rs", &s.head_sha, b"guard();\n")]);

    let provenance = vec![DependencyProvenance {
        url: "https://github.com/owner/dependency/blob/main/guard.rs".into(),
        reference_kind: ReferenceKind::Branch,
        reference: "main".into(),
        commit: revision.clone(),
        repository: "owner/dependency".into(),
        path: "guard.rs".into(),
    }];

    // 1. with_dependencies_and_provenance binds provenance
    let archive = primary()
        .with_dependencies_and_provenance(vec![(dep_archive, provenance.clone())])
        .unwrap();
    assert_eq!(archive.dependencies().len(), 1);
    assert_eq!(archive.dependencies()[0].provenance, provenance);

    // 2. Round-trip through JSON manifest
    let manifest_bytes = archive.manifest_bytes().unwrap();
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(manifest.dependencies[0].provenance, provenance);

    // 3. Legacy compatibility: legacy JSON without "provenance" field deserializes with empty Vec
    let mut manifest_val: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    manifest_val["dependencies"][0]
        .as_object_mut()
        .unwrap()
        .remove("provenance");
    let legacy_bytes = serde_json::to_vec(&manifest_val).unwrap();
    let legacy_manifest: Manifest = serde_json::from_slice(&legacy_bytes).unwrap();
    assert!(legacy_manifest.dependencies[0].provenance.is_empty());

    // Also, when provenance is empty, it is omitted from JSON serialization
    let without_prov = primary()
        .with_dependencies_and_provenance(vec![(
            Archive::build(
                Scope {
                    host: s.host.clone(),
                    repository: "owner/dependency".into(),
                    head_sha: revision.clone(),
                    base_sha: revision.clone(),
                    merge_base_sha: revision.clone(),
                    prior_sha: None,
                },
                vec![source("guard.rs", &revision, b"guard();\n")],
                &AtomicBool::new(false),
                |_| Ok(b"guard();\n".to_vec()),
            )
            .unwrap(),
            vec![],
        )])
        .unwrap();
    let raw_no_prov = String::from_utf8(without_prov.manifest_bytes().unwrap()).unwrap();
    assert!(!raw_no_prov.contains("\"provenance\""));

    // 4. Provenance validation rejections:
    // a. Repository mismatch
    let mut bad_repo_prov = provenance.clone();
    bad_repo_prov[0].repository = "other/repo".into();
    let mut bad_manifest = manifest.clone();
    bad_manifest.dependencies[0].provenance = bad_repo_prov;
    assert!(bad_manifest.validate_sources().is_err());

    // b. Commit mismatch
    let mut bad_commit_prov = provenance.clone();
    bad_commit_prov[0].commit = "e".repeat(40);
    let mut bad_manifest = manifest.clone();
    bad_manifest.dependencies[0].provenance = bad_commit_prov;
    assert!(bad_manifest.validate_sources().is_err());

    // c. Path not in dependency entries
    let mut bad_path_prov = provenance.clone();
    bad_path_prov[0].path = "unrelated.rs".into();
    let mut bad_manifest = manifest.clone();
    bad_manifest.dependencies[0].provenance = bad_path_prov;
    assert!(bad_manifest.validate_sources().is_err());

    // d. Unsafe path (path traversal)
    let mut bad_safe_path = provenance.clone();
    bad_safe_path[0].path = "../outside.rs".into();
    let mut bad_manifest = manifest.clone();
    bad_manifest.dependencies[0].provenance = bad_safe_path;
    assert!(bad_manifest.validate_sources().is_err());

    // e. Duplicate provenance
    let mut dup_prov = provenance.clone();
    dup_prov.push(provenance[0].clone());
    let mut bad_manifest = manifest.clone();
    bad_manifest.dependencies[0].provenance = dup_prov;
    assert!(bad_manifest.validate_sources().is_err());

    // f. URL mismatch with host/repo/ref/path
    for bad_url in [
        "https://evil.test/owner/dependency/blob/main/guard.rs",
        "https://github.com/other/dependency/blob/main/guard.rs",
        "https://github.com/owner/dependency/blob/other/guard.rs",
        "https://github.com/owner/dependency/blob/main/other.rs",
        "https://github.com/owner/dependency/blob/main/guard.rs.bak",
    ] {
        let mut bad_url_prov = provenance.clone();
        bad_url_prov[0].url = bad_url.into();
        let mut bad_manifest = manifest.clone();
        bad_manifest.dependencies[0].provenance = bad_url_prov;
        assert!(
            bad_manifest.validate_sources().is_err(),
            "Expected rejection of {bad_url}"
        );
    }

    // g. URL with fragment is valid
    let mut frag_prov = provenance.clone();
    frag_prov[0].url = "https://github.com/owner/dependency/blob/main/guard.rs#L10".into();
    let mut valid_manifest = manifest.clone();
    valid_manifest.dependencies[0].provenance = frag_prov;
    assert!(valid_manifest.validate_sources().is_ok());

    // h. Commit kind requires reference == commit
    let mut commit_mismatch_prov = provenance.clone();
    commit_mismatch_prov[0].reference_kind = ReferenceKind::Commit;
    commit_mismatch_prov[0].reference = "e".repeat(40);
    commit_mismatch_prov[0].url = format!("https://github.com/owner/dependency/blob/{}/guard.rs", "e".repeat(40));
    let mut bad_manifest = manifest.clone();
    bad_manifest.dependencies[0].provenance = commit_mismatch_prov;
    assert!(bad_manifest.validate_sources().is_err());

    // i. Commit kind with matching reference succeeds
    let mut commit_match_prov = provenance.clone();
    commit_match_prov[0].reference_kind = ReferenceKind::Commit;
    commit_match_prov[0].reference = revision.clone();
    commit_match_prov[0].url = format!("https://github.com/owner/dependency/blob/{revision}/guard.rs");
    let mut valid_manifest = manifest.clone();
    valid_manifest.dependencies[0].provenance = commit_match_prov;
    assert!(valid_manifest.validate_sources().is_ok());

    // j. Excluded entry in provenance is rejected
    let mut excluded_manifest = manifest.clone();
    excluded_manifest.dependencies[0].entries[0].exclusion = Some(Exclusion::PrivatePath);
    assert!(excluded_manifest.validate_sources().is_err());

    // k. Entry without content_hash in provenance is rejected
    let mut no_hash_manifest = manifest.clone();
    no_hash_manifest.dependencies[0].entries[0].content_hash = None;
    assert!(no_hash_manifest.validate_sources().is_err());

    // 5. Compact archive preserves provenance
    let compact = compact_archive(&archive);
    assert_eq!(compact.dependencies().len(), 1);
    assert_eq!(compact.dependencies()[0].provenance, provenance);
    assert_eq!(
        compact
            .resolve_dependency("owner/dependency", &revision, "guard.rs")
            .unwrap(),
        "guard();\n"
    );
    let transferred = transfer_manifest(&compact.descriptor(), &compact.manifest_bytes().unwrap()).unwrap();
    assert_eq!(transferred.dependencies[0].provenance, provenance);
}

#[test]
fn archive_store_open_read_and_load_linked_integrity() {
    let directory = Directory::new();
    let store_path = directory.0.join("store");

    // 1. open_read on non-existent path fails with Path error and does not create path
    assert!(matches!(
        ArchiveStore::open_read(&store_path, &crate::db::Db::for_test_at(&store_path)),
        Err(Error::Path)
    ));
    assert!(!store_path.exists());

    // 2. Setup a store using open, deposit archive, link receipt
    let mut store = ArchiveStore::open(&store_path, &crate::db::Db::for_test_at(&store_path)).unwrap();
    let archive_a = build(&[("a.rs", &scope().head_sha, b"source_a")]);
    let archive_b = build(&[("b.rs", &scope().head_sha, b"source_b")]);
    store.deposit(&archive_a).unwrap();
    store.deposit(&archive_b).unwrap();
    store.link("review:100", &archive_a.descriptor()).unwrap();
    drop(store);

    // 3. open_read successfully opens existing store read-only
    let ro_store = ArchiveStore::open_read(&store_path, &crate::db::Db::for_test_at(&store_path)).unwrap();
    let loaded = ro_store
        .load_linked("review:100")
        .unwrap()
        .expect("archive should load");
    assert_eq!(loaded.descriptor().id, archive_a.descriptor().id);
    assert!(ro_store.load_linked("review:nonexistent").unwrap().is_none());
    drop(ro_store);

    // 4. Integrity error when receipt points to missing artifact row
    let conn = crate::db::Db::for_test_at(&store_path).open().unwrap();
    conn.execute("INSERT INTO archive.receipts(owner, id) VALUES('review:corrupt', '0000000000000000000000000000000000000000000000000000000000000000')", []).unwrap();
    drop(conn);

    let ro_store = ArchiveStore::open_read(&store_path, &crate::db::Db::for_test_at(&store_path)).unwrap();
    assert!(matches!(ro_store.load_linked("review:corrupt"), Err(Error::Integrity)));
    drop(ro_store);

    // 5. Integrity error when artifact row descriptor.id does not match receipt ID
    let raw_b = serde_json::to_string(&archive_b.descriptor()).unwrap();
    let conn = crate::db::Db::for_test_at(&store_path).open().unwrap();
    conn.execute(
        "UPDATE archive.artifacts SET descriptor=$1 WHERE id=$2",
        crate::pg_params![raw_b, archive_a.descriptor().id],
    )
    .unwrap();
    drop(conn);

    let ro_store = ArchiveStore::open_read(&store_path, &crate::db::Db::for_test_at(&store_path)).unwrap();
    assert!(matches!(ro_store.load_linked("review:100"), Err(Error::Integrity)));
}

#[test]
fn a_store_with_an_unimported_sqlite_index_refuses_to_open_and_keeps_its_archives() {
    let directory = Directory::new();
    let root = directory.0.join("store");
    let sealed = root.join("sealed").join(format!("{}.archive", "a".repeat(64)));
    std::fs::create_dir_all(root.join("sealed")).unwrap();
    std::fs::write(&sealed, b"frozen evidence").unwrap();
    std::fs::write(root.join("index.sqlite"), b"").unwrap();
    let db = crate::db::Db::for_test_at(&root);
    // The Postgres index is empty until the import: recovery would delete every sealed archive.
    assert!(matches!(ArchiveStore::open(&root, &db), Err(Error::UnimportedIndex)));
    assert!(matches!(
        ArchiveStore::open_read(&root, &db),
        Err(Error::UnimportedIndex)
    ));
    assert!(sealed.exists());
    let report = crate::review_archive::purge::run(&db, &root, true, 10 * crate::review_archive::purge::RETENTION_MS);
    assert!(report.unwrap_err().contains("import-sqlite"));
    assert!(sealed.exists());
}

#[test]
fn a_repository_over_budget_keeps_the_reviewed_paths_first() {
    let head = "a".repeat(40);
    let sources = vec![
        source("a/big.rs", &head, &[b'a'; 60]),
        source("a/copy.rs", &head, &[b'a'; 60]),
        source("b/other.rs", &head, &[b'b'; 50]),
        source("z/changed.rs", &head, &[b'z'; 50]),
    ];
    let priority = BTreeSet::from(["z/changed.rs".to_string()]);
    // 120 blob bytes: the changed file first, then a/big.rs (60); its duplicate costs nothing more;
    // b/other.rs no longer fits and stays listed as excluded.
    let (kept, excluded) = plan_budget(&sources, &priority, u64::MAX, 120).unwrap();
    assert_eq!(kept, vec![true; 4]);
    assert_eq!(excluded, BTreeSet::from([(head.clone(), "b/other.rs".to_string())]));
    // A manifest with room for two entries keeps the changed file and the first other one.
    let room = 2 * ENTRY_OVERHEAD + "z/changed.rs".len() as u64 + "a/big.rs".len() as u64;
    let (kept, _) = plan_budget(&sources, &priority, room, u64::MAX).unwrap();
    assert_eq!(kept, vec![true, false, false, true]);

    let archive = Archive::build_planned(scope(), sources.clone(), &excluded, &AtomicBool::new(false), |entry| {
        Ok(vec![entry.path.as_bytes()[0]; entry.bytes as usize])
    })
    .unwrap();
    assert_eq!(archive.resolve(&head, "z/changed.rs").unwrap(), "z".repeat(50));
    assert_eq!(
        archive.resolve(&head, "b/other.rs"),
        Err(Error::Excluded(Exclusion::Budget))
    );
    // The budget exclusion survives the archive's own integrity check.
    let raw = archive.manifest_bytes().unwrap();
    Archive::decode(&archive.descriptor(), &raw, archive.data.clone()).unwrap();
    // And so does the runner's transfer check.
    transfer::transfer_manifest(&archive.descriptor(), &raw).unwrap();
}

/// A copy of `archive` whose manifest records `policy`, with `rename` applied to an entry path,
/// rehashed so only the policy check can reject it. Edits the canonical bytes in place.
fn recorded_under(archive: &Archive, policy: u32, rename: (&str, &str)) -> (Descriptor, Vec<u8>) {
    let encoded = String::from_utf8(archive.manifest_bytes().unwrap()).unwrap();
    let current = format!("\"policyVersion\":{POLICY_VERSION}");
    assert!(encoded.contains(&current));
    let mut raw = encoded.replace(&current, &format!("\"policyVersion\":{policy}"));
    if !rename.0.is_empty() {
        raw = raw.replace(
            &format!("\"path\":\"{}\"", rename.0),
            &format!("\"path\":\"{}\"", rename.1),
        );
    }
    let raw = raw.into_bytes();
    let mut descriptor = archive.descriptor();
    descriptor.id = hash(&raw);
    descriptor.manifest_bytes = raw.len() as u64;
    (descriptor, raw)
}

#[test]
fn policy_two_freezes_its_own_generated_and_lockfile_rules() {
    for path in [
        "vendor/github.com/pkg/errors/errors.go",
        "web/generated/client.ts",
        "static/app.min.css",
        "static/app.js.map",
        "static/app.css.map",
        "api/service.pb.go",
        "api/service.pb.h",
        "api/service.pb.cc",
        "proto/service_pb2.py",
        "proto/service_pb2_grpc.py",
        "internal/mock_gen.go",
        "pkg/zz_generated.deepcopy.go",
        "src/schema.generated.ts",
        "src/graphql/generated.ts",
        "package-lock.json",
        "npm-shrinkwrap.json",
        "go.sum",
        "Package.resolved",
        "src/App/packages.lock.json",
        "bun.lockb",
    ] {
        assert_eq!(path_exclusion(path).unwrap(), Some(Exclusion::Generated), "{path}");
        let policy_one = path_exclusion_for(path, 1).unwrap();
        assert!(
            policy_one.is_none() || path.contains(".generated."),
            "{path}: {policy_one:?}"
        );
    }
    // Policy 2 only adds generated paths after policy 1's rules, so their reasons never change.
    assert_eq!(path_exclusion("vendor/.env").unwrap(), Some(Exclusion::PrivatePath));
    assert_eq!(path_exclusion("vendor/logo.png").unwrap(), Some(Exclusion::BinaryPath));
    assert_eq!(
        path_exclusion("generated/.git/config").unwrap(),
        Some(Exclusion::PrivateConfiguration)
    );
    for kept in [
        "src/build.rs",
        "go.mod",
        "package.json",
        "src/vendored.rs",
        "docs/generated.md",
    ] {
        assert_eq!(path_exclusion(kept).unwrap(), None, "{kept}");
    }
}

#[test]
fn stored_archives_validate_under_the_policy_they_record() {
    let head = scope().head_sha;
    let archive = build(&[("gosum.rs", &head, b"h1:abc"), ("src/a.rs", &head, b"fn a() {}")]);
    // An archive frozen under policy 1 holds go.sum as content and still reads after the upgrade.
    let (descriptor, raw) = recorded_under(&archive, 1, ("gosum.rs", "go.sum"));
    let restored = Archive::decode(&descriptor, &raw, archive.blobs().to_vec()).unwrap();
    assert_eq!(restored.resolve(&head, "go.sum").unwrap(), "h1:abc");
    // The same entries under policy 2 claim content its rules exclude, and an unknown policy is refused.
    for policy in [2, 3] {
        let (descriptor, raw) = recorded_under(&archive, policy, ("gosum.rs", "go.sum"));
        assert!(
            Archive::decode(&descriptor, &raw, archive.blobs().to_vec()).is_err(),
            "{policy}"
        );
    }
    // New archives exclude go.sum without opening it.
    let fresh = Archive::build(
        scope(),
        vec![source("go.sum", &head, b"h1:abc")],
        &AtomicBool::new(false),
        |_| panic!("Excluded content must not be opened"),
    )
    .unwrap();
    assert!(matches!(fresh.resolve(&head, "go.sum"), Err(Error::Excluded(_))));
}

#[test]
fn transfer_accepts_only_the_current_policy() {
    let head = scope().head_sha;
    let archive = build(&[("src/a.rs", &head, b"fn a() {}")]);
    assert!(Incoming::new(archive.descriptor(), archive.manifest_bytes().unwrap()).is_ok());
    let (descriptor, raw) = recorded_under(&archive, 1, ("", ""));
    assert!(Archive::decode(&descriptor, &raw, archive.blobs().to_vec()).is_ok());
    assert!(Incoming::new(descriptor, raw).is_err());
}

#[test]
fn scope_accepts_nested_gitlab_namespaces() {
    let mut nested = scope();
    nested.host = "gitlab.example.com".into();
    nested.repository = "acme/team/api".into();
    assert!(nested.validate().is_ok());
    nested.repository = "solo".into();
    assert!(matches!(nested.validate(), Err(Error::Contract)));
    nested.repository = "a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u".into();
    assert!(matches!(nested.validate(), Err(Error::Contract)));
}
