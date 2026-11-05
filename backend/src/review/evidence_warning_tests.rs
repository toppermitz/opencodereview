use super::*;
use crate::review_archive::{Archive, Scope, SourceEntry};

const DIFF: &str = "diff --git a/docs/IDENTITIES.md b/docs/IDENTITIES.md\n--- /dev/null\n+++ b/docs/IDENTITIES.md\n@@ -0,0 +1 @@\n+See supporting configuration.\ndiff --git a/config.yaml b/config.yaml\n--- /dev/null\n+++ b/config.yaml\n@@ -0,0 +1 @@\n+enabled: true\n";

fn fixture() -> (std::path::PathBuf, Archive, Vec<String>) {
    let root = std::env::temp_dir().join(format!("ocr-evidence-warning-{}", crate::sandbox::secret().unwrap()));
    let head = "a".repeat(40);
    let base = "b".repeat(40);
    let sources = [
        ("docs/IDENTITIES.md", "See supporting configuration.\n"),
        ("config.yaml", "enabled: true\n"),
        ("support.rs", "guard();\n"),
    ];
    let entries = sources
        .iter()
        .map(|(path, content)| SourceEntry {
            path: (*path).into(),
            revision: head.clone(),
            mode: "100644".into(),
            git_blob_id: "d".repeat(40),
            bytes: content.len() as u64,
        })
        .collect();
    let archive = Archive::build(
        Scope {
            host: "github.com".into(),
            repository: "owner/repo".into(),
            head_sha: head,
            base_sha: base.clone(),
            merge_base_sha: base.clone(),
            prior_sha: None,
        },
        entries,
        &std::sync::atomic::AtomicBool::new(false),
        |entry| {
            Ok(sources
                .iter()
                .find(|(path, _)| *path == entry.path)
                .unwrap()
                .1
                .as_bytes()
                .to_vec())
        },
    )
    .unwrap();
    fs::create_dir_all(root.join("skill")).unwrap();
    fs::write(root.join("PR.md"), "Review configuration").unwrap();
    fs::write(root.join("skill/SKILL.md"), "Check correctness").unwrap();
    let files = vec!["docs/IDENTITIES.md".into(), "config.yaml".into()];
    archive.prepare_stage(&root, &files, DIFF, &base).unwrap();
    (root, archive, files)
}

fn report(archive: &Archive, files: &[String]) -> serde_json::Value {
    json!({"contractVersion":2,"snapshotId":archive.descriptor().id,"reviewedFiles":files,
        "missingEvidence":[{"reason":"insufficient_evidence","path":"docs/IDENTITIES.md"}],"findings":[]})
}

fn validate(
    root: &Path,
    archive: &Archive,
    files: &[String],
    report: &serde_json::Value,
) -> Result<Vec<crate::review::warnings::EvidenceWarning>, String> {
    fs::write(root.join("report.json"), report.to_string()).unwrap();
    EvidenceResolver::frozen(root, archive).materialize_with_warnings(
        &root.join("report.json"),
        files,
        &archive.scope().head_sha,
        DIFF,
        &root.join("summary.md"),
        &root.join("comments.json"),
    )
}

#[test]
fn frozen_review_completes_with_an_unspecified_supporting_reference() {
    let (root, archive, files) = fixture();
    let report = report(&archive, &files);
    let warnings = validate(&root, &archive, &files, &report).unwrap();
    assert_eq!(
        warnings,
        vec![crate::review::warnings::EvidenceWarning {
            reason: "insufficient_evidence".into(),
            path: Some("docs/IDENTITIES.md".into()),
            reference: None,
        }]
    );
    assert_eq!(
        fs::read_to_string(root.join("summary.md")).unwrap(),
        "Nada a sinalizar."
    );
    let comments: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("comments.json")).unwrap()).unwrap();
    assert!(comments["comments"].as_array().unwrap().is_empty());
    assert!(EvidenceResolver::frozen(&root, &archive)
        .materialize(
            &root.join("report.json"),
            &files,
            &archive.scope().head_sha,
            DIFF,
            &root.join("summary.md"),
            &root.join("comments.json")
        )
        .is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn evidence_warning_keeps_supported_findings_and_rejects_invalid_output() {
    let (root, archive, files) = fixture();
    let mut valid = report(&archive, &files);
    let reference = json!({"path":"config.yaml","revision":archive.scope().head_sha,"line":1,"quote":"enabled: true"});
    valid["findings"] = json!([{"kind":"finding","title":"Guard required","detail":"Unconditional enablement bypasses the guard","action":"Restore the guard","path":"config.yaml","line":1,"side":"RIGHT","evidence":[reference],"counterEvidence":[reference]}]);
    assert_eq!(validate(&root, &archive, &files, &valid).unwrap().len(), 1);
    let comments: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("comments.json")).unwrap()).unwrap();
    assert_eq!(comments["comments"].as_array().unwrap().len(), 1);
    for (pointer, value) in [
        ("/reviewedFiles", json!(["config.yaml"])),
        (
            "/reviewedFiles",
            json!(["config.yaml", "docs/IDENTITIES.md", "support.rs"]),
        ),
        (
            "/reviewedFiles",
            json!(["config.yaml", "docs/IDENTITIES.md", "config.yaml"]),
        ),
        ("/requestedFiles", json!(["support.rs"])),
        ("/findings/0/evidence/0/quote", json!("invented()")),
        ("/findings/0/line", json!(999)),
        ("/findings/0/counterEvidence", json!([])),
    ] {
        let mut invalid = valid.clone();
        if pointer == "/requestedFiles" {
            invalid["requestedFiles"] = value;
        } else {
            *invalid.pointer_mut(pointer).unwrap() = value;
        }
        assert!(validate(&root, &archive, &files, &invalid).is_err(), "{pointer}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn evidence_warnings_keep_only_assigned_paths_and_verified_reference_metadata() {
    let (root, archive, files) = fixture();
    let mut report = report(&archive, &files);
    let canary = "private-description-and-unregistered-path";
    report["missingEvidence"] = json!([
        {"reason":"insufficient_evidence","path":canary,"description":canary,"reference":{"path":canary,"revision":archive.scope().head_sha}},
        {"reason":"file_unavailable","path":"config.yaml","description":canary,"reference":{"path":"support.rs","revision":archive.scope().head_sha}},
    ]);
    let warnings = validate(&root, &archive, &files, &report).unwrap();
    assert!(!serde_json::to_string(&warnings).unwrap().contains(canary));
    assert!(warnings.iter().any(|warning| warning.path.is_none()));
    assert!(warnings
        .iter()
        .any(|warning| warning.reference.as_deref().is_some_and(
            |reference| reference.contains("support.rs") && reference.contains("available in frozen snapshot")
        )));
    fs::remove_dir_all(root).unwrap();
}
