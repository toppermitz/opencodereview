use super::*;
use std::fs;
use std::path::PathBuf;

// Fixture repositories model gh contents access without calling external services.
fn capture(
    archive: Archive,
    repo: &Path,
    changed: &[String],
    body: &str,
    prepared: &BTreeSet<String>,
    network: bool,
    cancel: &AtomicBool,
) -> Result<Archive, String> {
    capture_with_recorded(archive, repo, changed, body, prepared, network, None, cancel)
}

fn capture_with_recorded(
    archive: Archive,
    repo: &Path,
    changed: &[String],
    body: &str,
    prepared: &BTreeSet<String>,
    network: bool,
    recorded: Option<&[DependencyProvenance]>,
    cancel: &AtomicBool,
) -> Result<Archive, String> {
    super::capture_with_recorded_and_access(
        archive,
        repo,
        changed,
        body,
        prepared,
        network,
        recorded,
        cancel,
        &mut |repository, _, _| {
            if prepared.contains(repository) {
                Ok(())
            } else {
                Err(format!(
                    "Pinned dependency {repository} is not authorized by the fixture gh session"
                ))
            }
        },
    )
}

fn temporary() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ocr-pinned-dependencies-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path.canonicalize().unwrap()
}

fn git(repo: &Path, args: &[&str]) -> String {
    String::from_utf8(crate::indexer::git(repo, args, None).unwrap()).unwrap()
}

fn add_origin(repo: &Path, remote_path: &Path, remote_name: &str) {
    let url = format!("git@github.com:{remote_name}.git");
    if crate::indexer::git(repo, &["remote", "get-url", "origin"], None).is_ok() {
        git(repo, &["remote", "set-url", "origin", &url]);
    } else {
        git(repo, &["remote", "add", "origin", &url]);
    }
    let cmd = format!("sh -c \"exec git-upload-pack '{}'\" --", remote_path.display());
    git(repo, &["config", "core.sshCommand", &cmd]);
}

fn commit(repo: &Path) -> String {
    git(repo, &["add", "."]);
    git(
        repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-m",
            "fixture",
        ],
    );
    git(repo, &["rev-parse", "HEAD"]).trim().into()
}

fn repository(root: &Path, name: &str, files: &[(&str, &str)]) -> (PathBuf, String) {
    let repo = root.join(name);
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=main"]);
    for (path, text) in files {
        let path = repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let sha = commit(&repo);
    add_origin(&repo, &repo, name);
    (repo, sha)
}

fn create_tag(repo: &Path, name: &str) {
    git(repo, &["tag", name]);
}

fn create_annotated_tag(repo: &Path, name: &str, message: &str) {
    git(
        repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "tag",
            "-a",
            name,
            "-m",
            message,
        ],
    );
}

fn create_branch(repo: &Path, name: &str) {
    git(repo, &["branch", name]);
}

#[test]
fn pinned_dependency_links_require_exact_host_commit_and_safe_paths() {
    let sha = "a".repeat(40);
    let links = extract_raw_links(
        &format!("[schema](https://github.com/acme/api/blob/{sha}/src/schema.ts#L2)"),
        "github.com",
    )
    .unwrap();
    assert_eq!(
        links,
        vec![RawDiscoveredLink::Sha {
            repository: "acme/api".into(),
            sha: sha.clone(),
            path: "src/schema.ts".into(),
            url: format!("https://github.com/acme/api/blob/{sha}/src/schema.ts#L2")
        }]
    );
    for punctuation in [".", ",", ";", ":", "!"] {
        let text = format!("See https://github.com/acme/api/blob/{sha}/src/schema.ts{punctuation} Then continue.");
        assert_eq!(
            extract_raw_links(&text, "github.com").unwrap(),
            vec![RawDiscoveredLink::Sha {
                repository: "acme/api".into(),
                sha: sha.clone(),
                path: "src/schema.ts".into(),
                url: format!("https://github.com/acme/api/blob/{sha}/src/schema.ts")
            }]
        );
        let explicit = format!("<https://github.com/acme/api/blob/{sha}/src/schema.ts{punctuation}>");
        assert_eq!(
            extract_raw_links(&explicit, "github.com").unwrap(),
            vec![RawDiscoveredLink::Sha {
                repository: "acme/api".into(),
                sha: sha.clone(),
                path: format!("src/schema.ts{punctuation}"),
                url: format!("https://github.com/acme/api/blob/{sha}/src/schema.ts{punctuation}")
            }]
        );
    }
    let named = extract_raw_links("https://github.com/acme/api/blob/main/src/schema.ts", "github.com").unwrap();
    assert_eq!(named.len(), 1);
    match &named[0] {
        RawDiscoveredLink::Named {
            repository,
            candidate_splits,
            url,
        } => {
            assert_eq!(repository, "acme/api");
            assert_eq!(url, "https://github.com/acme/api/blob/main/src/schema.ts");
            assert_eq!(
                candidate_splits,
                &[
                    ("main".into(), "src/schema.ts".into()),
                    ("main/src".into(), "schema.ts".into())
                ]
            );
        }
        _ => panic!("expected Named"),
    }
    for url in [
        format!("https://github.com.evil.test/acme/api/blob/{sha}/src/schema.ts"),
        format!("https://github.com@evil.test/acme/api/blob/{sha}/src/schema.ts"),
        format!("https://github.com/acme/api/tree/{sha}/src"),
    ] {
        assert!(extract_raw_links(&url, "github.com").unwrap().is_empty(), "{url}");
    }
    for invalid in [
        format!("https://github.com/acme/api/blob/{sha}/../secret"),
        format!("https://github.com/acme/api/blob/{sha}/%2e%2e/secret"),
        "https://github.com/acme/api/blob/main?query=1/file.ts".to_string(),
    ] {
        assert!(extract_raw_links(&invalid, "github.com").is_err(), "{invalid}");
    }
    assert_eq!(
        repository_url("git@github.com:acme/common.git", "github.com").as_deref(),
        Some("acme/common")
    );
    for url in [
        "../../common",
        "file:///tmp/repo",
        "https://evil.test/acme/api",
        "git@github.com:../repo.git",
    ] {
        assert!(repository_url(url, "github.com").is_none());
    }
}

#[test]
fn pinned_markdown_discovery_preserves_links_and_rejects_invalid_declarations() {
    let sha = "a".repeat(40);
    let url = format!("https://github.com/acme/api/blob/{sha}/schema.sql");
    let cancel = AtomicBool::new(false);
    for doc in [
        format!("[migration]({url})"),
        format!("[`migration`]({url})"),
        format!("<{url}>"),
        format!("[migration][schema]\n\n[schema]: {url}"),
        format!("See {url}. Then continue."),
    ] {
        assert_eq!(
            extract_markdown_links(&doc, "github.com", None, &cancel).unwrap().len(),
            1,
            "{doc}"
        );
    }
    for doc in [
        format!("https://github.com/acme/api/blob/{sha}/../secret"),
        "[invalid](https://github.com/acme/api/blob/{sha}/../secret)".into(),
    ] {
        assert!(
            extract_markdown_links(&doc, "github.com", None, &cancel).is_err(),
            "{doc}"
        );
    }
    assert_eq!(
        extract_markdown_links(&url, "github.com", None, &AtomicBool::new(true)).unwrap_err(),
        "cancelled"
    );
    for path in ["README.md", "docs/dependency.markdown", "docs/README.MD"] {
        assert!(is_markdown_document(path), "{path}");
    }
    for path in ["src/dependency.rs", "tests/fixture.json", "src/example.mdx", "src/md"] {
        assert!(!is_markdown_document(path), "{path}");
    }
}

#[test]
fn pinned_discovery_ignores_code_fixtures_and_preserves_review_evidence() {
    let root = temporary();
    let fixture = "let invalid = \"https://github.com/acme/api/blob/{sha}/../secret\";\nlet valid = \"https://github.com/acme/api/blob/main/schema.sql\";\n";
    let (repo, head) = repository(
        &root,
        "acme/service",
        &[
            ("tests/links.rs", fixture),
            ("src/links.ts", fixture),
            ("fixtures/links.json", fixture),
        ],
    );
    let cancel = AtomicBool::new(false);
    let base = review_archive::capture(&repo, scope("github.com", "acme/service", &head).unwrap(), &cancel).unwrap();
    let changed = vec![
        "tests/links.rs".into(),
        "src/links.ts".into(),
        "fixtures/links.json".into(),
    ];
    let result = super::capture_with_recorded_and_access(
        base,
        &repo,
        &changed,
        "",
        &BTreeSet::new(),
        true,
        None,
        &cancel,
        &mut |_, _, _| panic!("code examples must not request repository access"),
    );
    fs::remove_dir_all(root).unwrap();
    let archive = result.expect("PR #63: source fixtures must not block dependency preparation");
    assert!(archive.dependencies().is_empty());
    for path in changed {
        assert_eq!(archive.resolve(&head, &path).unwrap(), fixture);
    }
}

#[test]
fn pinned_discovery_ignores_markdown_code_examples() {
    let root = temporary();
    let example = "https://github.com/acme/api/blob/{sha}/../secret";
    let doc = format!("# Examples\n\nPortuguês: `{example}` and `` `{example}` ``.\n\n```rust\n{example}\n```\n\n~~~text\nhttps://github.com/acme/api/blob/main/schema.sql\n~~~\n\n    {example}\n\n> ```\n> {example}\n> ```\n\n- Nested example:\n\n  ```\n  {example}\n  ```\n");
    let (repo, head) = repository(&root, "acme/service", &[("README.md", &doc)]);
    let cancel = AtomicBool::new(false);
    let base = review_archive::capture(&repo, scope("github.com", "acme/service", &head).unwrap(), &cancel).unwrap();
    let result = super::capture_with_recorded_and_access(
        base,
        &repo,
        &["README.md".into()],
        &doc,
        &BTreeSet::new(),
        true,
        None,
        &cancel,
        &mut |_, _, _| panic!("Markdown code examples must not request repository access"),
    );
    fs::remove_dir_all(root).unwrap();
    let archive = result.expect("Markdown examples must not declare dependencies");
    assert!(archive.dependencies().is_empty());
    assert_eq!(archive.resolve(&head, "README.md").unwrap(), doc);
}

#[test]
fn pinned_discovery_preserves_pr330_documentation_migration_link() {
    let root = temporary();
    let path = "gitops/charts/reprogramming/templates/db-migration.yaml";
    let (_, revision) = repository(&root, "acme/infrastructure", &[(path, "hook: before-rollout\n")]);
    let doc = format!(
        "The database migration is run by this [`job`](https://github.com/acme/infrastructure/blob/main/{path}).\n"
    );
    let (repo, head) = repository(&root, "acme/service", &[("README.md", &doc)]);
    let cancel = AtomicBool::new(false);
    let base = review_archive::capture(&repo, scope("github.com", "acme/service", &head).unwrap(), &cancel).unwrap();
    let mut reads = Vec::new();
    let archive = super::capture_with_recorded_and_access(
        base,
        &repo,
        &["README.md".into()],
        "",
        &BTreeSet::new(),
        true,
        None,
        &cancel,
        &mut |repository, host, _| {
            reads.push((repository.to_string(), host.to_string()));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(reads, vec![("acme/infrastructure".into(), "github.com".into())]);
    assert_eq!(archive.dependencies().len(), 1);
    assert_eq!(archive.dependencies()[0].revision, revision);
    assert_eq!(
        archive
            .resolve_dependency("acme/infrastructure", &revision, path)
            .unwrap(),
        "hook: before-rollout\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_dependencies_capture_authorized_links_and_submodule_without_checkout() {
    let root = temporary();
    let (api, api_sha) = repository(
        &root,
        "acme/api",
        &[
            ("schema.sql", "CREATE TABLE example(id int);\n"),
            ("unrelated.rs", "not requested\n"),
        ],
    );
    let (common, common_sha) = repository(
        &root,
        "acme/common",
        &[
            ("src/guard.ts", "export const guard = true;\n"),
            (".env.fixture", "excluded-fixture\n"),
        ],
    );
    let doc = format!("[schema](https://github.com/acme/api/blob/{api_sha}/schema.sql)\n");
    let (repo, _) = repository(
        &root,
        "acme/dispatcher",
        &[
            ("docs/schema.md", &doc),
            (
                ".gitmodules",
                "[submodule \"src/common\"]\n path = src/common\n url = git@github.com:acme/common.git\n",
            ),
        ],
    );
    git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &common_sha,
            "src/common",
        ],
    );
    git(
        &repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-m",
            "submodule",
        ],
    );
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    fs::write(api.join("schema.sql"), "working tree must not be used\n").unwrap();
    fs::write(common.join("src/guard.ts"), "working tree must not be used\n").unwrap();
    let cancel = AtomicBool::new(false);
    let base =
        || review_archive::capture(&repo, scope("github.com", "acme/dispatcher", &head).unwrap(), &cancel).unwrap();
    let changed = vec!["docs/schema.md".into()];
    let authorized = BTreeSet::from(["acme/api".into(), "acme/common".into()]);
    assert!(capture(base(), &repo, &changed, "", &BTreeSet::new(), false, &cancel)
        .unwrap_err()
        .contains("not authorized"));
    let archive = capture(base(), &repo, &changed, "", &authorized, false, &cancel).unwrap();
    assert_eq!(archive.dependencies().len(), 2);
    assert_eq!(
        archive.resolve_dependency("acme/api", &api_sha, "schema.sql").unwrap(),
        "CREATE TABLE example(id int);\n"
    );
    assert!(archive
        .resolve_dependency("acme/api", &api_sha, "unrelated.rs")
        .is_err());
    assert_eq!(
        archive
            .resolve_dependency("acme/common", &common_sha, "src/guard.ts")
            .unwrap(),
        "export const guard = true;\n"
    );
    assert!(archive
        .resolve_dependency("acme/common", &common_sha, ".env.fixture")
        .is_err());
    assert!(archive.resolve_dependency("acme/api", &head, "schema.sql").is_err());
    assert!(archive.resolve(&api_sha, "schema.sql").is_err());
    assert_ne!(archive.descriptor().id, base().descriptor().id);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), head);
    assert_eq!(
        fs::read_to_string(api.join("schema.sql")).unwrap(),
        "working tree must not be used\n"
    );
    let mut incoming = review_archive::Incoming::new(archive.descriptor(), archive.manifest_bytes().unwrap()).unwrap();
    for (digest, bytes) in archive.transfer_blobs() {
        incoming.push(digest, bytes).unwrap();
    }
    let transferred = incoming.finish().unwrap();
    assert_eq!(transferred.descriptor(), archive.descriptor());
    let output = root.join("snapshot");
    transferred.materialize_new(&output, &cancel).unwrap();
    assert_eq!(
        fs::read_to_string(output.join(format!("dependencies/acme/api/{api_sha}/schema.sql"))).unwrap(),
        "CREATE TABLE example(id int);\n"
    );
    assert!(!output
        .join(format!("dependencies/acme/common/{common_sha}/.env.fixture"))
        .exists());
    assert!(transferred.protocol().contains("source:\"repository:<owner>/<repo>\""));
    let missing = format!("https://github.com/acme/api/blob/{}/schema.sql", "d".repeat(40));
    assert!(capture(base(), &repo, &[], &missing, &authorized, false, &cancel)
        .unwrap_err()
        .contains("unavailable"));
    let private = format!("https://github.com/acme/api/blob/{api_sha}/.env");
    assert!(capture(base(), &repo, &[], &private, &authorized, false, &cancel)
        .unwrap_err()
        .contains("excluded by evidence policy"));
    // A link to a lockfile or generated file is left out like the file, without failing preparation.
    for generated in ["go.sum", "vendor/lib/x.go", "api.pb.go", "docs/diagram.png"] {
        let link = format!("https://github.com/acme/api/blob/{api_sha}/{generated}");
        let captured = capture(base(), &repo, &[], &link, &authorized, false, &cancel).unwrap();
        let unlinked = capture(base(), &repo, &[], "", &authorized, false, &cancel).unwrap();
        assert_eq!(captured.descriptor(), unlinked.descriptor(), "{generated}");
    }
    // Such a link never reaches the repository's access check, cache or budget, even when that
    // repository is unknown and would be refused.
    let unknown = format!("https://github.com/other/private/blob/{api_sha}/go.sum https://github.com/other/private/blob/main/package-lock.json");
    let captured = capture(base(), &repo, &[], &unknown, &authorized, true, &cancel).unwrap();
    let unlinked = capture(base(), &repo, &[], "", &authorized, false, &cancel).unwrap();
    assert_eq!(captured.descriptor(), unlinked.descriptor());
    assert!(capture(base(), &repo, &changed, "", &authorized, false, &AtomicBool::new(true)).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_submodule_configuration_does_not_follow_includes_or_ambiguous_urls() {
    let root = temporary();
    let (repo, _) = repository(&root, "acme/repo", &[("file", "fixture")]);
    let cancel = AtomicBool::new(false);
    let valid =
        "[include]\npath = /does/not/exist\n[submodule \"lib\"]\npath = lib\nurl = https://github.com/acme/lib.git\n";
    assert_eq!(
        module_repositories(&repo, valid, "github.com", &cancel)
            .unwrap()
            .get("lib")
            .map(String::as_str),
        Some("acme/lib")
    );
    assert!(module_repositories(
        &repo,
        &format!("{valid}url = https://github.com/acme/other.git\n"),
        "github.com",
        &cancel
    )
    .is_err());
    assert!(module_repositories(
        &repo,
        "[submodule \"lib\"]\npath = ../lib\nurl = file:///tmp/lib\n",
        "github.com",
        &cancel
    )
    .is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_pr175_description_migrations_reproduction() {
    let root = temporary();
    let migrations = [
        (
            "migrations/20260916181501_add_critical_error_occurrences/migration.sql",
            "CREATE TABLE critical_error_occurrence(id int);\n",
        ),
        (
            "migrations/20260918152653_keep_ended_suspensions/migration.sql",
            "ALTER TABLE suspension ADD COLUMN ended_at timestamp;\n",
        ),
        (
            "migrations/20260921160919_remove_suspension_reason/migration.sql",
            "ALTER TABLE suspension DROP COLUMN reason;\n",
        ),
    ];
    let (_, api_sha) = repository(&root, "acme/api", &migrations);
    let (repo, head) = repository(
        &root,
        "acme/data-seeding",
        &[
            ("src/config/critical_error_occurrence.ts", "export default {};\n"),
            ("src/config/resolution.ts", "export default {};\n"),
            ("src/config/suspension.ts", "export default {};\n"),
        ],
    );
    let cancel = AtomicBool::new(false);
    let base =
        || review_archive::capture(&repo, scope("github.com", "acme/data-seeding", &head).unwrap(), &cancel).unwrap();
    let changed = vec![
        "src/config/critical_error_occurrence.ts".into(),
        "src/config/resolution.ts".into(),
        "src/config/suspension.ts".into(),
    ];
    let authorized = BTreeSet::from(["acme/api".into()]);
    let body = migrations
        .iter()
        .map(|(path, _)| format!("[migration](https://github.com/acme/api/blob/main/{path})\n"))
        .collect::<String>();

    // Control: exact-SHA links can be captured with network=false
    let pinned = capture(
        base(),
        &repo,
        &changed,
        &body.replace("/blob/main/", &format!("/blob/{api_sha}/")),
        &authorized,
        false,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        pinned.dependencies().len(),
        1,
        "control: exact-SHA links must capture the dependency"
    );
    for (path, content) in &migrations {
        assert_eq!(pinned.resolve_dependency("acme/api", &api_sha, path).unwrap(), *content);
    }

    // Production: named links resolve with network=true
    let named = capture(base(), &repo, &changed, &body, &authorized, true, &cancel).unwrap();
    assert_eq!(
        named.dependencies().len(),
        1,
        "PR #175: all three blob/main migration links must be captured"
    );
    for (path, content) in &migrations {
        assert_eq!(named.resolve_dependency("acme/api", &api_sha, path).unwrap(), *content);
    }
    let dep = &named.dependencies()[0];
    assert_eq!(dep.repository, "acme/api");
    assert_eq!(dep.revision, api_sha);
    assert_eq!(dep.provenance.len(), 3);
    for (index, (path, _)) in migrations.iter().enumerate() {
        assert_eq!(dep.provenance[index].path, *path);
        assert_eq!(dep.provenance[index].reference, "main");
        assert_eq!(dep.provenance[index].reference_kind, ReferenceKind::Branch);
        assert_eq!(dep.provenance[index].commit, api_sha);
    }

    // Verify stage preparation preserves assigned review scope (supporting files are not reviewable files)
    let context = root.join("context");
    fs::create_dir_all(context.join("skill")).unwrap();
    fs::write(context.join("PR.md"), "PR description").unwrap();
    fs::write(context.join("skill/SKILL.md"), "skill prompt").unwrap();
    named.prepare_stage(&context, &changed, "", &head).unwrap();
    let stage_bytes = fs::read_to_string(context.join("STAGE.json")).unwrap();
    let stage: serde_json::Value = serde_json::from_str(&stage_bytes).unwrap();
    let stage_files = stage["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        stage_files,
        vec![
            "src/config/critical_error_occurrence.ts",
            "src/config/resolution.ts",
            "src/config/suspension.ts",
        ]
    );
    assert!(!stage_files.contains(&"migrations/20260916181501_add_critical_error_occurrences/migration.sql"));
    assert!(named.protocol().contains(
        "These are supporting evidence, not additional review scope: never add dependency files to reviewedFiles"
    ));
    assert!(named.protocol().contains("acme/api @"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_links_succeed_for_branch_lightweight_and_annotated_tags() {
    let root = temporary();
    let (api, api_sha) = repository(&root, "acme/api", &[("schema.sql", "CREATE TABLE example(id int);\n")]);
    create_tag(&api, "v1.0-light");
    create_annotated_tag(&api, "v1.0-annotated", "release v1.0");
    create_branch(&api, "release/stable");
    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "console.log(1);\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);

    for (ref_name, expected_kind) in [
        ("main", ReferenceKind::Branch),
        ("release/stable", ReferenceKind::Branch),
        ("v1.0-light", ReferenceKind::Tag),
        ("v1.0-annotated", ReferenceKind::Tag),
        (&api_sha, ReferenceKind::Commit),
    ] {
        let body = format!("Check https://github.com/acme/api/blob/{ref_name}/schema.sql for the schema.");
        let captured = capture(base(), &repo, &[], &body, &authorized, true, &cancel).unwrap();
        assert_eq!(captured.dependencies().len(), 1);
        assert_eq!(
            captured.resolve_dependency("acme/api", &api_sha, "schema.sql").unwrap(),
            "CREATE TABLE example(id int);\n"
        );
        let dep = &captured.dependencies()[0];
        assert_eq!(dep.revision, api_sha);
        assert_eq!(dep.provenance.len(), 1);
        assert_eq!(dep.provenance[0].reference, ref_name);
        assert_eq!(dep.provenance[0].reference_kind, expected_kind);
        assert_eq!(dep.provenance[0].commit, api_sha);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_branch_movement_before_vs_after_preparation() {
    let root = temporary();
    let (api, sha1) = repository(&root, "acme/api", &[("schema.sql", "version 1\n")]);
    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "client\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);
    let body = "See https://github.com/acme/api/blob/main/schema.sql";

    let snap1 = capture(base(), &repo, &[], body, &authorized, true, &cancel).unwrap();
    assert_eq!(
        snap1.resolve_dependency("acme/api", &sha1, "schema.sql").unwrap(),
        "version 1\n"
    );

    // Move branch main in acme/api
    fs::write(api.join("schema.sql"), "version 2\n").unwrap();
    let sha2 = commit(&api);
    assert_ne!(sha1, sha2);

    // Old snapshot remains unchanged at sha1
    assert_eq!(
        snap1.resolve_dependency("acme/api", &sha1, "schema.sql").unwrap(),
        "version 1\n"
    );
    assert_eq!(snap1.dependencies()[0].revision, sha1);

    // New preparation captures sha2
    let snap2 = capture(base(), &repo, &[], body, &authorized, true, &cancel).unwrap();
    assert_eq!(
        snap2.resolve_dependency("acme/api", &sha2, "schema.sql").unwrap(),
        "version 2\n"
    );
    assert_eq!(snap2.dependencies()[0].revision, sha2);
    assert_ne!(snap1.descriptor().id, snap2.descriptor().id);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_duplicate_links_and_multiple_files_share_consistent_commit_and_provenance() {
    let root = temporary();
    let (_api, api_sha) = repository(
        &root,
        "acme/api",
        &[
            ("schema.sql", "CREATE TABLE example(id int);\n"),
            ("views.sql", "CREATE VIEW v AS SELECT 1;\n"),
        ],
    );
    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "client\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);

    let body = format!(
        "Links: https://github.com/acme/api/blob/main/schema.sql and https://github.com/acme/api/blob/main/views.sql and duplicate https://github.com/acme/api/blob/{api_sha}/schema.sql"
    );
    let archive = capture(base(), &repo, &[], &body, &authorized, true, &cancel).unwrap();
    assert_eq!(archive.dependencies().len(), 1);
    let dep = &archive.dependencies()[0];
    assert_eq!(
        dep.entries.len(),
        2,
        "schema.sql must be deduplicated in captured entries"
    );
    assert_eq!(
        dep.provenance.len(),
        3,
        "all three distinct original URLs must be preserved in provenance"
    );
    assert_eq!(
        archive.resolve_dependency("acme/api", &api_sha, "schema.sql").unwrap(),
        "CREATE TABLE example(id int);\n"
    );
    assert_eq!(
        archive.resolve_dependency("acme/api", &api_sha, "views.sql").unwrap(),
        "CREATE VIEW v AS SELECT 1;\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_slash_containing_refs_and_error_cases() {
    let root = temporary();
    let (api, api_sha) = repository(&root, "acme/api", &[("src/db.sql", "select 1;\n")]);
    create_branch(&api, "feat/new-db");
    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "client\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);

    // Unambiguous slash-containing branch resolves
    let valid_slash = capture(
        base(),
        &repo,
        &[],
        "https://github.com/acme/api/blob/feat/new-db/src/db.sql",
        &authorized,
        true,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        valid_slash
            .resolve_dependency("acme/api", &api_sha, "src/db.sql")
            .unwrap(),
        "select 1;\n"
    );

    // Ambiguous split: create tag feat alongside branch feat/new-db
    create_tag(&api, "feat");
    let err_ambig = capture(
        base(),
        &repo,
        &[],
        "https://github.com/acme/api/blob/feat/new-db/src/db.sql",
        &authorized,
        true,
        &cancel,
    )
    .unwrap_err();
    assert!(
        err_ambig.contains("Ambiguous") || err_ambig.contains("multiple valid reference interpretations"),
        "{err_ambig}"
    );

    // Branch/tag collision: create tag matching branch main
    create_tag(&api, "main");
    let err_collision = capture(
        base(),
        &repo,
        &[],
        "https://github.com/acme/api/blob/main/src/db.sql",
        &authorized,
        true,
        &cancel,
    )
    .unwrap_err();
    assert!(
        err_collision.contains("Ambiguous reference") || err_collision.contains("both a branch and a tag"),
        "{err_collision}"
    );

    // Missing ref fails
    let err_missing = capture(
        base(),
        &repo,
        &[],
        "https://github.com/acme/api/blob/does-not-exist/src/db.sql",
        &authorized,
        true,
        &cancel,
    )
    .unwrap_err();
    assert!(
        err_missing.contains("unavailable") || err_missing.contains("not found"),
        "{err_missing}"
    );

    // Non-commit target: create tag pointing to blob
    let blob_sha = git(&api, &["hash-object", "-w", "src/db.sql"]).trim().to_string();
    git(&api, &["tag", "blob-tag", &blob_sha]);
    let err_non_commit = capture(
        base(),
        &repo,
        &[],
        "https://github.com/acme/api/blob/blob-tag/src/db.sql",
        &authorized,
        true,
        &cancel,
    )
    .unwrap_err();
    assert!(
        err_non_commit.contains("non-commit") || err_non_commit.contains("does not target a commit"),
        "{err_non_commit}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_offline_reconstruction_with_and_without_recorded_resolutions() {
    let root = temporary();
    let (_api, api_sha) = repository(&root, "acme/api", &[("src/schema.sql", "CREATE TABLE t(x int);\n")]);
    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "console.log(1);\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);
    let body = "See https://github.com/acme/api/blob/main/src/schema.sql";

    // Online capture (network = true) performs remote resolution
    let online = capture(base(), &repo, &[], body, &authorized, true, &cancel).unwrap();
    let recorded_provs = online.dependencies()[0].provenance.clone();
    assert_eq!(recorded_provs.len(), 1);

    // Offline capture (network = false) without recorded resolutions fails explicitly requiring hydration
    let offline_err = capture_with_recorded(base(), &repo, &[], body, &authorized, false, None, &cancel).unwrap_err();
    assert!(
        offline_err.contains("requires hydration before offline review preparation"),
        "{offline_err}"
    );

    // Offline capture (network = false) WITH recorded resolutions succeeds and matches
    let offline = capture_with_recorded(
        base(),
        &repo,
        &[],
        body,
        &authorized,
        false,
        Some(&recorded_provs),
        &cancel,
    )
    .unwrap();
    assert_eq!(offline.descriptor(), online.descriptor());
    assert_eq!(
        offline
            .resolve_dependency("acme/api", &api_sha, "src/schema.sql")
            .unwrap(),
        "CREATE TABLE t(x int);\n"
    );

    // Tampered recorded resolution (e.g. non-existent commit) fails cleanly offline
    let mut tampered = recorded_provs;
    tampered[0].commit = "0".repeat(40);
    let tampered_err =
        capture_with_recorded(base(), &repo, &[], body, &authorized, false, Some(&tampered), &cancel).unwrap_err();
    assert!(tampered_err.contains("unavailable"), "{tampered_err}");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_remote_resolution_fetches_when_local_clone_is_stale() {
    let root = temporary();
    // Upstream repository has initial commit v1
    let (upstream, _) = repository(&root, "upstream/api", &[("schema.sql", "v1\n")]);
    // Local clone has initial commit v1 and origin configured to upstream
    let (local_api, _) = repository(&root, "acme/api", &[("schema.sql", "v1\n")]);
    add_origin(&local_api, &upstream, "acme/api");

    // Add a new commit v2 to upstream
    fs::write(upstream.join("schema.sql"), "v2\n").unwrap();
    let v2_sha = commit(&upstream);

    // Verify local clone does NOT have v2_sha yet
    assert!(crate::indexer::git_frozen_with_input(
        &local_api,
        &["cat-file", "-e", &format!("{v2_sha}^{{commit}}")],
        &AtomicBool::new(false),
        &[]
    )
    .is_err());

    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "client\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);

    // PR references main branch which is at v2_sha upstream
    let body = "See https://github.com/acme/api/blob/main/schema.sql";
    let archive = capture(base(), &repo, &[], body, &authorized, true, &cancel).unwrap();

    // Verify v2 was fetched into local clone and captured in the archive
    assert_eq!(
        archive.resolve_dependency("acme/api", &v2_sha, "schema.sql").unwrap(),
        "v2\n"
    );
    assert_eq!(archive.dependencies()[0].revision, v2_sha);
    assert_eq!(archive.dependencies()[0].provenance[0].commit, v2_sha);
    assert!(crate::indexer::git_frozen_with_input(
        &local_api,
        &["cat-file", "-e", &format!("{v2_sha}^{{commit}}")],
        &AtomicBool::new(false),
        &[]
    )
    .is_ok());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_origin_mismatch_and_remote_failure_handling() {
    let root = temporary();
    let (api, _) = repository(&root, "acme/api", &[("schema.sql", "CREATE TABLE t(id int);\n")]);
    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "client\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);
    let body = "See https://github.com/acme/api/blob/main/schema.sql";

    // 1. Origin mismatch: origin points to wrong repository
    git(
        &api,
        &["remote", "set-url", "origin", "git@github.com:wrong/other-repo.git"],
    );
    let err_mismatch = capture(base(), &repo, &[], body, &authorized, true, &cancel).unwrap_err();
    assert!(
        err_mismatch.contains("Dependency repository origin does not match the authorized GitHub repository"),
        "{err_mismatch}"
    );

    // 2. Remote command failure: sshCommand fails
    git(&api, &["remote", "set-url", "origin", "git@github.com:acme/api.git"]);
    git(&api, &["config", "core.sshCommand", "sh -c \"exit 1\" --"]);
    let err_remote_fail = capture(base(), &repo, &[], body, &authorized, true, &cancel).unwrap_err();
    assert!(
        err_remote_fail.contains("Failed to resolve reference 'main' in repository 'acme/api'"),
        "{err_remote_fail}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_budgets_and_error_sanitization() {
    let root = temporary();
    let (_api, _) = repository(&root, "acme/api", &[("schema.sql", "select 1;\n")]);
    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "client\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();

    // Budget: >16 distinct repositories in PR body rejected before remote resolution
    let mut many_repos = Vec::new();
    let mut auth_set = BTreeSet::new();
    for i in 0..17 {
        let repo_name = format!("acme/repo{i}");
        many_repos.push(format!("https://github.com/{repo_name}/blob/main/file.ts"));
        auth_set.insert(repo_name);
    }
    let body_many = many_repos.join("\n");
    let err_budget = capture(base(), &repo, &[], &body_many, &auth_set, true, &cancel).unwrap_err();
    assert!(
        err_budget.contains("exceeds its bounded repository/file limit"),
        "{err_budget}"
    );

    // URL sanitization: query parameters and fragments stripped in error messages
    let authorized = BTreeSet::from(["acme/api".into()]);
    let dirty_url = "https://github.com/acme/api/blob/does-not-exist/file.ts?secret=token123&session=xyz#frag";
    let err_dirty =
        capture_with_recorded(base(), &repo, &[], dirty_url, &authorized, false, None, &cancel).unwrap_err();
    assert!(
        !err_dirty.contains("secret=token123"),
        "Secret query parameter must be stripped: {err_dirty}"
    );
    assert!(!err_dirty.contains("#frag"), "Fragment must be stripped: {err_dirty}");
    assert!(
        err_dirty.contains("https://github.com/acme/api/blob/does-not-exist/file.ts"),
        "Base URL preserved: {err_dirty}"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_sanitization_handles_multibyte_unicode_without_panic() {
    let unicode_path = "é".repeat(100);
    let long_url = format!("https://github.com/acme/api/blob/main/{unicode_path}?token=secret#L1");
    let sanitized = sanitize_url(&long_url);
    assert!(sanitized.len() <= 160);
    assert!(!sanitized.contains("token=secret"));
    assert!(!sanitized.contains("#L1"));
    assert!(sanitized.ends_with("..."));

    let long_err = format!("fatal: error in git operation {}", "🦀".repeat(100));
    let sanitized_err = dependency_git_error(&long_err, "Dependency lookup", &AtomicBool::new(false));
    assert_eq!(sanitized_err, "Dependency lookup: Git command failed");
}

#[test]
fn pinned_candidate_split_depth_budget_rejected_explicitly() {
    let deep_path = (0..20).map(|i| format!("d{i}")).collect::<Vec<_>>().join("/");
    let deep_url = format!("https://github.com/acme/api/blob/{deep_path}/file.ts");
    let err = extract_raw_links(&deep_url, "github.com").unwrap_err();
    assert!(err.contains("exceeds the candidate interpretation budget"), "{err}");
}

#[test]
fn pinned_multiple_fragment_links_offline_reconstruction() {
    let root = temporary();
    let (_api, api_sha) = repository(&root, "acme/api", &[("schema.sql", "CREATE TABLE t(id int);\n")]);
    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "client\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);

    // PR description contains multiple links to the same file with different line fragments
    let body = "Lines: https://github.com/acme/api/blob/main/schema.sql#L1 and https://github.com/acme/api/blob/main/schema.sql#L2";

    // Online capture records both provenance URLs
    let online = capture(base(), &repo, &[], body, &authorized, true, &cancel).unwrap();
    let dep = &online.dependencies()[0];
    assert_eq!(dep.provenance.len(), 2);
    let provenance = dep.provenance.clone();

    // Offline capture with recorded provenance must NOT fail as ambiguous
    let offline =
        capture_with_recorded(base(), &repo, &[], body, &authorized, false, Some(&provenance), &cancel).unwrap();
    assert_eq!(
        offline.resolve_dependency("acme/api", &api_sha, "schema.sql").unwrap(),
        "CREATE TABLE t(id int);\n"
    );
    assert_eq!(offline.descriptor(), online.descriptor());

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_url_length_and_repository_name_bounds_rejected_safely() {
    let huge_url = format!("https://github.com/acme/api/blob/main/{}", "x".repeat(2000));
    let err = extract_raw_links(&huge_url, "github.com").unwrap_err();
    assert!(err.contains("exceeds the supported length limit"));
    assert!(err.len() <= 300);

    let huge_owner = "a".repeat(101);
    let url_huge_owner = format!("https://github.com/{huge_owner}/api/blob/main/schema.sql");
    let err = extract_raw_links(&url_huge_owner, "github.com").unwrap_err();
    assert!(err.contains("invalid repository format"));
    assert!(err.len() <= 300);

    // Long non-file GitHub URL is ignored and does NOT fail discovery
    let long_search_url = format!("https://github.com/search?q={}", "query_term+".repeat(200));
    let links = extract_raw_links(&format!("Link: {long_search_url}"), "github.com").unwrap();
    assert!(links.is_empty());
}

#[test]
fn pinned_partial_recorded_coverage_reuses_historical_commit_for_new_links_on_same_ref() {
    let root = temporary();
    let (remote, commit_a) = repository(
        &root,
        "remote/api",
        &[
            ("schema.sql", "CREATE TABLE a(id int);\n"),
            ("views.sql", "CREATE VIEW v_a AS SELECT * FROM a;\n"),
        ],
    );
    let (api, _) = repository(
        &root,
        "acme/api",
        &[
            ("schema.sql", "CREATE TABLE a(id int);\n"),
            ("views.sql", "CREATE VIEW v_a AS SELECT * FROM a;\n"),
        ],
    );
    add_origin(&api, &remote, "acme/api");
    git(&api, &["fetch", "origin", "main"]);

    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "client\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);

    // Initial capture only referenced schema.sql at commit A
    let initial = capture(
        base(),
        &repo,
        &[],
        "https://github.com/acme/api/blob/main/schema.sql",
        &authorized,
        true,
        &cancel,
    )
    .unwrap();
    assert_eq!(initial.dependencies()[0].revision, commit_a);
    let historical_provenance = initial.dependencies()[0].provenance.clone();

    // Remote advances to commit B with updated files
    fs::write(remote.join("schema.sql"), "CREATE TABLE b(id int);\n").unwrap();
    fs::write(remote.join("views.sql"), "CREATE VIEW v_b AS SELECT * FROM b;\n").unwrap();
    let commit_b = commit(&remote);
    assert_ne!(commit_a, commit_b);

    // PR description now adds a new link on main: views.sql
    let body = "https://github.com/acme/api/blob/main/schema.sql and https://github.com/acme/api/blob/main/views.sql";

    // Hydration with historical provenance must resolve views.sql at the SAME historical commit A, not remote commit B
    let hydrated = capture_with_recorded(
        base(),
        &repo,
        &[],
        body,
        &authorized,
        true,
        Some(&historical_provenance),
        &cancel,
    )
    .unwrap();
    assert_eq!(hydrated.dependencies().len(), 1);
    assert_eq!(hydrated.dependencies()[0].revision, commit_a);
    assert_eq!(
        hydrated
            .resolve_dependency("acme/api", &commit_a, "schema.sql")
            .unwrap(),
        "CREATE TABLE a(id int);\n"
    );
    assert_eq!(
        hydrated.resolve_dependency("acme/api", &commit_a, "views.sql").unwrap(),
        "CREATE VIEW v_a AS SELECT * FROM a;\n"
    );
    // Both provenances are now recorded
    assert_eq!(hydrated.dependencies()[0].provenance.len(), 2);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_hydration_preserves_historical_recorded_provenance_when_remote_advances() {
    let root = temporary();
    let (remote, commit_a) = repository(&root, "remote/api", &[("schema.sql", "CREATE TABLE a(id int);\n")]);
    let (api, _) = repository(&root, "acme/api", &[("schema.sql", "CREATE TABLE a(id int);\n")]);
    add_origin(&api, &remote, "acme/api");
    git(&api, &["fetch", "origin", "main"]);

    let (repo, head) = repository(&root, "acme/client", &[("src/index.ts", "client\n")]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);
    let body = "https://github.com/acme/api/blob/main/schema.sql";

    // Online capture records commit A
    let initial = capture(base(), &repo, &[], body, &authorized, true, &cancel).unwrap();
    assert_eq!(initial.dependencies()[0].revision, commit_a);
    let recorded_provenance = initial.dependencies()[0].provenance.clone();

    // Remote advances to commit B with updated schema
    fs::write(remote.join("schema.sql"), "CREATE TABLE b(id int);\n").unwrap();
    let commit_b = commit(&remote);
    assert_ne!(commit_a, commit_b);

    // Hydration (network = true) with historical recorded provenance must preserve commit A, NOT commit B
    let hydrated = capture_with_recorded(
        base(),
        &repo,
        &[],
        body,
        &authorized,
        true,
        Some(&recorded_provenance),
        &cancel,
    )
    .unwrap();
    assert_eq!(hydrated.dependencies()[0].revision, commit_a);
    assert_eq!(
        hydrated
            .resolve_dependency("acme/api", &commit_a, "schema.sql")
            .unwrap(),
        "CREATE TABLE a(id int);\n"
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_dependencies_ignore_primary_links_before_validation() {
    let root = temporary();
    let internal = "https://github.com/ACME/client/blob/main/README.md?plain=1";
    let (repo, head) = repository(&root, "acme/client", &[("README.md", internal)]);
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let changed = vec!["README.md".into()];
    for body in [
        internal,
        "https://github.com/acme/client/blob/main/%66ile",
        "https://github.com/acme/client/blob",
    ] {
        let archive = capture(base(), &repo, &changed, body, &BTreeSet::new(), false, &cancel).unwrap();
        assert!(archive.dependencies().is_empty());
    }
    let external = "https://github.com/acme/api/blob/main/README.md?plain=1";
    let error = capture(
        base(),
        &repo,
        &changed,
        external,
        &BTreeSet::from(["acme/api".into()]),
        false,
        &cancel,
    )
    .unwrap_err();
    assert!(error.contains("unsupported encoding or query"), "{error}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_dependencies_deduplicate_links_across_body_and_files() {
    let root = temporary();
    let (_, sha) = repository(&root, "acme/api", &[("schema.sql", "select 1;\n")]);
    let link = format!("https://github.com/acme/api/blob/{sha}/schema.sql\n");
    let repeated = link.repeat(129);
    let (repo, head) = repository(
        &root,
        "acme/client",
        &[("README.md", &repeated), ("docs/schema.md", &link)],
    );
    let cancel = AtomicBool::new(false);
    let base = || review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);
    let changed = vec!["README.md".into(), "docs/schema.md".into()];
    let archive = capture(base(), &repo, &changed, &repeated, &authorized, false, &cancel).unwrap();
    assert_eq!(archive.dependencies().len(), 1);
    assert_eq!(archive.dependencies()[0].entries.len(), 1);
    assert_eq!(
        archive.resolve_dependency("acme/api", &sha, "schema.sql").unwrap(),
        "select 1;\n"
    );

    let unique = (0..129)
        .map(|i| format!("https://github.com/acme/api/blob/{sha}/file{i}.sql\n"))
        .collect::<Vec<_>>();
    // Both a single input and the union of body/file links remain bounded.
    for body in [unique.concat(), unique[..128].concat()] {
        let error = capture(base(), &repo, &changed, &body, &authorized, false, &cancel).unwrap_err();
        assert!(error.contains("discovery budget"), "{error}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_named_ref_cancellation_preserves_job_runner_signal() {
    let root = temporary();
    let (api, _) = repository(&root, "acme/api", &[("schema.sql", "select 1;\n")]);
    let (repo, head) = repository(&root, "acme/client", &[("README.md", "client\n")]);
    fs::write(
        api.join("remote-fixture.sh"),
        "printf started > .resolution-started\nexec sleep 10\n",
    )
    .unwrap();
    git(&api, &["config", "core.sshCommand", "sh ./remote-fixture.sh"]);
    git(&api, &["config", "ssh.variant", "ssh"]);
    let cancel = AtomicBool::new(false);
    let archive = review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
    let authorized = BTreeSet::from(["acme/api".into()]);
    let marker = api.join(".resolution-started");
    std::thread::scope(|threads| {
        let worker = threads.spawn(|| {
            capture(
                archive,
                &repo,
                &[],
                "https://github.com/acme/api/blob/main/schema.sql",
                &authorized,
                true,
                &cancel,
            )
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() && !worker.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cancel.store(true, Ordering::Relaxed);
        let error = worker.join().unwrap().unwrap_err();
        assert!(
            marker.exists(),
            "The remote lookup must start before cancellation: {error}"
        );
        assert_eq!(error, "cancelled");
    });
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_git_errors_do_not_include_authentication_helper_output() {
    let root = temporary();
    let (api, _) = repository(&root, "acme/api", &[("schema.sql", "select 1;\n")]);
    let (repo, head) = repository(&root, "acme/client", &[("README.md", "client\n")]);
    // Exercise real Git stderr through a failing local transport, without a network call.
    fs::write(api.join("remote-fixture.sh"), "printf '%s\n' 'https://user:fixture-private-value@github.com' 'Authorization: Bearer fixture-private-value' >&2\nexit 1\n").unwrap();
    git(&api, &["config", "core.sshCommand", "sh ./remote-fixture.sh"]);
    git(&api, &["config", "ssh.variant", "ssh"]);
    let cancel = AtomicBool::new(false);
    let authorized = BTreeSet::from(["acme/api".into()]);
    let raw_error = crate::indexer::git(&api, &["ls-remote", "origin"], Some(&cancel)).unwrap_err();
    assert!(
        raw_error.contains("fixture-private-value"),
        "Fixture must emit sensitive stderr"
    );
    // Named references use ls-remote; an absent commit exercises fetch.
    for reference in ["main".to_string(), "a".repeat(40)] {
        let archive =
            review_archive::capture(&repo, scope("github.com", "acme/client", &head).unwrap(), &cancel).unwrap();
        let body = format!("https://github.com/acme/api/blob/{reference}/schema.sql");
        let error = capture(archive, &repo, &[], &body, &authorized, true, &cancel).unwrap_err();
        assert!(error.contains("acme/api"), "{error}");
        assert!(error.contains(&reference), "{error}");
        assert!(error.ends_with("Git command failed"), "{error}");
        assert!(!error.contains("fixture-private-value"));
        assert!(!error.contains("Authorization"));
        assert!(!error.contains("https://user:"));
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_gh_authorized_unmonitored_dependency_is_frozen_without_subscription() {
    let root = temporary();
    let (_, revision) = repository(
        &root,
        "acme/infrastructure",
        &[("deploy.yaml", "hook: before-rollout\n")],
    );
    let link = format!("https://github.com/acme/infrastructure/blob/{revision}/deploy.yaml");
    let (repo, head) = repository(&root, "acme/service", &[("README.md", &link)]);
    let cancel = AtomicBool::new(false);
    let mut requests = Vec::new();
    let archive = review_archive::capture(&repo, scope("github.com", "acme/service", &head).unwrap(), &cancel).unwrap();
    let archive = super::capture_with_recorded_and_access(
        archive,
        &repo,
        &["README.md".into()],
        &link,
        &BTreeSet::new(),
        true,
        None,
        &cancel,
        &mut |repository, host, _| {
            requests.push((repository.to_string(), host.to_string()));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(requests, [("acme/infrastructure".into(), "github.com".into())]);
    assert_eq!(
        archive
            .resolve_dependency("acme/infrastructure", &revision, "deploy.yaml")
            .unwrap(),
        "hook: before-rollout\n"
    );
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).trim(), head);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_gh_access_denial_is_checked_even_for_a_cached_or_enabled_dependency() {
    let root = temporary();
    let (_, revision) = repository(&root, "acme/api", &[("schema.sql", "schema\n")]);
    let (repo, head) = repository(&root, "acme/service", &[("file", "source\n")]);
    let cancel = AtomicBool::new(false);
    let archive = review_archive::capture(&repo, scope("github.com", "acme/service", &head).unwrap(), &cancel).unwrap();
    let error = super::capture_with_recorded_and_access(
        archive,
        &repo,
        &[],
        &format!("https://github.com/acme/api/blob/{revision}/schema.sql"),
        &BTreeSet::from(["acme/api".into()]),
        true,
        None,
        &cancel,
        &mut |_, _, _| Err("gh contents access denied".into()),
    )
    .unwrap_err();
    assert_eq!(error, "gh contents access denied");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_gh_access_initializes_only_an_object_cache_and_denial_creates_nothing() {
    let root = temporary();
    let cancel = AtomicBool::new(false);
    ensure_dependency_repository(
        &root,
        "acme/api",
        "github.com",
        true,
        &BTreeSet::new(),
        &cancel,
        &mut |_, _, _| Ok(()),
    )
    .unwrap();
    let cache = root.join("acme/api");
    assert!(cache.join(".git").is_dir());
    assert_eq!(
        git(&cache, &["remote", "get-url", "origin"]).trim(),
        "https://github.com/acme/api.git"
    );
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 1);
    assert!(ensure_dependency_repository(
        &root,
        "acme/denied",
        "github.com",
        true,
        &BTreeSet::new(),
        &cancel,
        &mut |_, _, _| Err("denied".into())
    )
    .is_err());
    assert!(!root.join("acme/denied").exists());
    assert_eq!(fs::read_dir(root.join("acme")).unwrap().count(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pinned_glab_contents_probe_is_sanitized_and_cancellable() {
    let cancel = AtomicBool::new(false);
    verify_repository_read_access_using("acme/api", "gitlab.example.com", &cancel, |args| {
        assert_eq!(args, ["api", "projects/acme%2Fapi/repository/commits?per_page=1"]);
        Ok(format!(r#"[{{"id":"{}"}}]"#, "a".repeat(40)).into_bytes())
    })
    .unwrap();
    let error = verify_repository_read_access_using("acme/api", "gitlab.example.com", &cancel, |_| {
        Err("private-auth-fixture-do-not-persist".into())
    })
    .unwrap_err();
    assert!(error.contains("authenticated glab read access"));
    assert!(!error.contains("private-auth-fixture"));
    assert!(
        verify_repository_read_access_using("acme/api", "gitlab.example.com", &cancel, |_| Ok(b"null\n".to_vec()))
            .is_err()
    );
    let cancel = AtomicBool::new(true);
    assert_eq!(
        verify_repository_read_access_using("acme/api", "gitlab.example.com", &cancel, |_| panic!(
            "Cancelled access must not call glab"
        ))
        .unwrap_err(),
        "cancelled"
    );
}

fn disclosure(primary: &str, repository: &str, visibility: &[(&str, &str)]) -> Result<(), String> {
    let cancel = AtomicBool::new(false);
    let mut primary_public = None;
    authorize_dependency_disclosure_using(
        primary,
        &mut primary_public,
        repository,
        "gitlab.example.com",
        &cancel,
        |args| {
            assert_eq!(args[0], "api");
            let route = args[1].strip_prefix("projects/").unwrap();
            let decoded = route.replace("%2F", "/");
            let (_, private) = visibility.iter().find(|(name, _)| *name == decoded).unwrap();
            let visibility = if *private == "true" { "private" } else { "public" };
            Ok(format!(r#"{{"visibility":"{visibility}"}}"#).into_bytes())
        },
    )
}

#[test]
fn linked_private_repository_never_reaches_a_wider_audience() {
    // Public dependencies are always fine and never need the primary's visibility.
    disclosure("acme/web", "other/lib", &[("other/lib", "false")]).unwrap();
    // A private dependency of the same owner may back a private review.
    disclosure(
        "acme/web",
        "Acme/secrets",
        &[("Acme/secrets", "true"), ("acme/web", "true")],
    )
    .unwrap();
    // A public pull request must never quote private code, even from its own owner.
    let error = disclosure(
        "acme/web",
        "acme/secrets",
        &[("acme/secrets", "true"), ("acme/web", "false")],
    )
    .unwrap_err();
    assert!(
        error.contains("acme/secrets") && error.contains("public repository"),
        "{error}"
    );
    // Another owner's private code stays out of a private review too.
    let error = disclosure(
        "acme/web",
        "other/secrets",
        &[("other/secrets", "true"), ("acme/web", "true")],
    )
    .unwrap_err();
    assert!(error.contains("another owner"), "{error}");
    // An unreadable visibility answer fails closed without echoing glab output.
    let cancel = AtomicBool::new(false);
    let error =
        authorize_dependency_disclosure_using("acme/web", &mut None, "acme/lib", "gitlab.example.com", &cancel, |_| {
            Err("private-auth-fixture-do-not-persist".into())
        })
        .unwrap_err();
    assert!(
        error.contains("visibility") && !error.contains("private-auth-fixture"),
        "{error}"
    );
    assert!(authorize_dependency_disclosure_using(
        "acme/web",
        &mut None,
        "acme/lib",
        "gitlab.example.com",
        &cancel,
        |_| Ok(b"null\n".to_vec())
    )
    .is_err());
}

#[test]
fn disclosure_check_asks_for_the_primary_visibility_once() {
    let cancel = AtomicBool::new(false);
    let mut primary_public = None;
    let mut primary_calls = 0;
    for repository in ["acme/one", "acme/two"] {
        authorize_dependency_disclosure_using(
            "acme/web",
            &mut primary_public,
            repository,
            "gitlab.example.com",
            &cancel,
            |args| {
                primary_calls += usize::from(args[1] == "projects/acme%2Fweb");
                Ok(br#"{"visibility":"private"}"#.to_vec())
            },
        )
        .unwrap();
    }
    assert_eq!((primary_calls, primary_public), (1, Some(false)));
}

#[test]
fn pinned_gh_authorized_object_cache_fetches_named_reference_without_checkout() {
    let root = temporary();
    let (remote, revision) = repository(
        &root,
        "fixtures/infrastructure",
        &[("deploy.yaml", "hook: before-rollout\n")],
    );
    let cancel = AtomicBool::new(false);
    ensure_dependency_repository(
        &root,
        "acme/infrastructure",
        "github.com",
        true,
        &BTreeSet::new(),
        &cancel,
        &mut |_, _, _| Ok(()),
    )
    .unwrap();
    let cache = root.join("acme/infrastructure");
    add_origin(&cache, &remote, "acme/infrastructure");
    let (repo, head) = repository(&root, "acme/service", &[("file", "source\n")]);
    let archive = review_archive::capture(&repo, scope("github.com", "acme/service", &head).unwrap(), &cancel).unwrap();
    let archive = super::capture_with_recorded_and_access(
        archive,
        &repo,
        &[],
        "https://github.com/acme/infrastructure/blob/main/deploy.yaml",
        &BTreeSet::new(),
        true,
        None,
        &cancel,
        &mut |_, _, _| Ok(()),
    )
    .unwrap();
    assert_eq!(
        archive
            .resolve_dependency("acme/infrastructure", &revision, "deploy.yaml")
            .unwrap(),
        "hook: before-rollout\n"
    );
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 1);
    assert_eq!(archive.dependencies()[0].provenance[0].reference, "main");
    assert_eq!(archive.dependencies()[0].provenance[0].commit, revision);
    fs::remove_dir_all(root).unwrap();
}
