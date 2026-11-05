use super::*;
use std::sync::atomic::AtomicBool;

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=OpenCodeReview Test",
        ])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git command");
    assert!(status.success(), "git {:?} failed", args);
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn parses_tree_without_loading_blob_sizes() {
    let entries = parse_tree(
        b"100644 blob abc\tsrc/a file.ts\0\
        160000 commit def\tvendor/submodule\0",
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "src/a file.ts");
    assert_eq!(entries[0].size, -1);
}

#[test]
fn partial_clone_indexes_sources_without_hydrating_binary_blobs() {
    let root = std::env::temp_dir().join(format!(
        "ocr-partial-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = root.join("source");
    let clone = root.join("clone");
    fs::create_dir_all(&source).unwrap();
    write_file(&source, "package.json", r#"{"name":"partial"}"#);
    write_file(&source, "src/index.ts", "export function greet() { return 'hello'; }");
    write_file(&source, "assets/large.png", &"binary".repeat(100_000));
    run_git(&source, &["init", "-q"]);
    run_git(&source, &["add", "-A"]);
    run_git(&source, &["commit", "-q", "-m", "fixture"]);
    run_git(
        &root,
        &[
            "clone",
            "--no-checkout",
            "--filter=blob:none",
            "--upload-pack=git -c uploadpack.allowFilter=true upload-pack",
            &format!("file://{}", source.display()),
            clone.to_str().unwrap(),
        ],
    );
    let cancel = AtomicBool::new(false);
    let write = run_index(&clone, None, &cancel, &mut |_| {}).expect("partial index");
    assert!(write.symbols.iter().any(|symbol| symbol.name == "greet"));
    assert!(write
        .files
        .iter()
        .any(|file| file.path == "src/index.ts" && file.size > 0));
    assert!(write
        .files
        .iter()
        .any(|file| file.path == "assets/large.png" && file.size == -1));
    write_file(
        &source,
        "src/index.ts",
        "export function updated() { return 'new content'; }",
    );
    run_git(&source, &["add", "src/index.ts"]);
    run_git(&source, &["commit", "-q", "-m", "update source"]);
    run_git(&clone, &["fetch", "origin", "HEAD"]);
    let refreshed = run_index_for_ref(&clone, "FETCH_HEAD", None, &cancel, &mut |_| {}).expect("refresh partial index");
    assert_ne!(write.base_sha, refreshed.base_sha);
    assert!(refreshed.symbols.iter().any(|symbol| symbol.name == "updated"));
    let missing = git(
        &clone,
        &["rev-list", "--objects", "--missing=print", "HEAD"],
        Some(&cancel),
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&missing)
        .lines()
        .any(|line| line.starts_with('?')));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn oversized_manifests_fail_indexing_instead_of_disappearing() {
    let manifest = r#"{"name":"bounded-package"}"#;
    for path in ["package.json", "dist/package.json"] {
        let dir = std::env::temp_dir().join(format!(
            "ocr-manifest-limit-{}-{}",
            std::process::id(),
            crate::db::now_millis()
        ));
        fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q"]);
        for size in [2_000_000, 2_000_001] {
            write_file(&dir, path, &format!("{manifest}{}", " ".repeat(size - manifest.len())));
            run_git(&dir, &["add", path]);
            run_git(&dir, &["commit", "-qm", "manifest boundary"]);
            let result = run_index(&dir, None, &AtomicBool::new(false), &mut |_| {});
            if size == 2_000_000 {
                assert!(result
                    .unwrap()
                    .packages
                    .iter()
                    .any(|package| package.name == "bounded-package"));
            } else {
                let error = result.expect_err("oversized manifest must fail indexing");
                assert!(error.contains("package.json") && error.contains("2000000"), "{error}");
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn manifests_in_one_directory_make_one_package() {
    let dir = std::env::temp_dir().join(format!(
        "ocr-shared-manifest-dir-{}-{}",
        std::process::id(),
        crate::db::now_millis()
    ));
    fs::create_dir_all(&dir).unwrap();
    run_git(&dir, &["init", "-q"]);
    write_file(&dir, "requirements.txt", "requests==2.32.0\n");
    write_file(&dir, "setup.py", "from setuptools import setup\nsetup(name='tool')\n");
    write_file(&dir, "tool/__init__.py", "");
    run_git(&dir, &["add", "-A"]);
    run_git(&dir, &["commit", "-qm", "python package"]);
    let write = run_index(&dir, None, &AtomicBool::new(false), &mut |_| {}).expect("index");
    assert_eq!(
        write
            .packages
            .iter()
            .filter(|package| package.package_path.is_empty())
            .count(),
        1,
        "{:?}",
        write
            .packages
            .iter()
            .map(|package| &package.package_path)
            .collect::<Vec<_>>()
    );
    // setup.py's declared name wins over the one requirements.txt makes up.
    let root = write
        .packages
        .iter()
        .find(|package| package.package_path.is_empty())
        .unwrap();
    assert_eq!(root.name, "tool");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pyproject_comments_do_not_break_dependency_parsing() {
    let text = "[project]\nname = \"ingest\"\ndependencies = [\n    \"biolink-model\", # pull 'master' for now, until \"x\n    \"bmt>=1.4\", # a lone \" quote\n]\n";
    assert_eq!(
        manifests::toml_array_strings_in_section(text, "project", "dependencies"),
        ["biolink-model", "bmt>=1.4"]
    );
    assert_eq!(manifests::unquote("\""), None);
}

#[test]
fn parses_market_language_manifests() {
    let node = parse_manifest(
        "package.json",
        br#"{"name":"web","dependencies":{"react":"latest"},"devDependencies":{"vite":"latest"}}"#,
    )
    .unwrap();
    assert_eq!(node.name, "web");
    assert!(node.deps.iter().any(|(name, dev)| name == "react" && !dev));
    assert!(node.deps.iter().any(|(name, dev)| name == "vite" && *dev));

    let go = parse_manifest(
        "go.mod",
        b"module github.com/acme/service\n\nrequire github.com/stretchr/testify v1.8.0\n",
    )
    .unwrap();
    assert_eq!(go.name, "github.com/acme/service");
    assert!(go.deps.iter().any(|(name, _)| name == "github.com/stretchr/testify"));

    let rust = parse_manifest(
        "Cargo.toml",
        b"[package]\nname = \"core\"\n\n[dependencies]\nserde = \"1\"\n\n[dev-dependencies]\ninsta = \"1\"\n",
    )
    .unwrap();
    assert_eq!(rust.name, "core");
    assert!(rust.deps.iter().any(|(name, dev)| name == "serde" && !dev));
    assert!(rust.deps.iter().any(|(name, dev)| name == "insta" && *dev));

    let python = parse_manifest(
        "pyproject.toml",
        b"[project]\nname = \"api\"\ndependencies = [\"fastapi>=0.1\", \"pydantic\"]\n",
    )
    .unwrap();
    assert_eq!(python.name, "api");
    assert!(python.deps.iter().any(|(name, _)| name == "fastapi"));
    assert!(python.deps.iter().any(|(name, _)| name == "pydantic"));
}

#[test]
fn indexes_go_internal_import_edges() {
    let dir = std::env::temp_dir().join(format!("ocr-indexer-go-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write_file(&dir, "go.mod", "module github.com/acme/service\n\ngo 1.22\n");
    write_file(
        &dir,
        "cmd/api/main.go",
        "package main\n\nimport \"github.com/acme/service/internal/version\"\n\nfunc main() { version.Get() }\n",
    );
    write_file(
        &dir,
        "internal/version/version.go",
        "package version\n\nfunc Get() string { return \"1\" }\n",
    );
    run_git(&dir, &["init", "-q"]);
    run_git(&dir, &["add", "-A"]);
    run_git(&dir, &["commit", "-q", "-m", "init"]);

    let cancel = AtomicBool::new(false);
    let write = run_index(&dir, None, &cancel, &mut |_| {}).expect("index go repo");
    assert_eq!(write.packages.len(), 1);
    assert!(write.edges.iter().any(|edge| edge.from_path == "cmd/api/main.go"
        && edge.to_ref == "github.com/acme/service/internal/version"
        && edge.resolved
        && edge.to_path.as_deref() == Some("internal/version/version.go")));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn extracts_python_import_specs() {
    let src = "# comment\nimport os\nimport numpy as np\nimport a.b, c.d as e\nfrom .utils import helper\nfrom ..models.base import Base\nfrom pkg.sub import thing\n";
    let specs: Vec<String> = extract_python_imports(src).into_iter().map(|(spec, _)| spec).collect();
    for expected in ["os", "numpy", "a.b", "c.d", ".utils", "..models.base", "pkg.sub"] {
        assert!(specs.contains(&expected.to_string()), "missing {expected}");
    }
}

#[test]
fn indexes_python_import_edges() {
    let dir = std::env::temp_dir().join(format!("ocr-indexer-py-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write_file(&dir, "pkg/__init__.py", "");
    write_file(&dir, "pkg/utils.py", "def helper():\n    return 1\n");
    write_file(&dir, "pkg/config.py", "setting = 1\n");
    write_file(
        &dir,
        "pkg/main.py",
        "import os\nfrom pkg.utils import helper\nfrom .config import setting\n",
    );
    run_git(&dir, &["init", "-q"]);
    run_git(&dir, &["add", "-A"]);
    run_git(&dir, &["commit", "-q", "-m", "init"]);

    let cancel = AtomicBool::new(false);
    let write = run_index(&dir, None, &cancel, &mut |_| {}).expect("index python repo");
    let edge = |to_ref: &str| {
        write
            .edges
            .iter()
            .find(|edge| edge.from_path == "pkg/main.py" && edge.to_ref == to_ref)
    };
    let utils = edge("pkg.utils").expect("pkg.utils edge");
    assert!(utils.resolved && utils.to_path.as_deref() == Some("pkg/utils.py"));
    let config = edge(".config").expect(".config edge");
    assert!(config.resolved && config.to_path.as_deref() == Some("pkg/config.py"));
    let os_edge = edge("os").expect("os edge");
    assert!(!os_edge.resolved && os_edge.to_path.is_none());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn extracts_python_symbol_calls() {
    let src = "def run():\n    x = helper()\n    obj.method(x)\n    return _step(x)\n";
    let lines: Vec<&str> = src.lines().collect();
    let calls = python_symbol_calls(&lines, 1, 4, "run");
    for expected in ["helper", "method", "_step"] {
        assert!(calls.contains(&expected.to_string()), "missing {expected}");
    }
    assert!(!calls.contains(&"run".to_string()), "self excluded");
    // builtins/keywords filtered
    let src2 = "def f():\n    print(len(x))\n    return str(x)\n";
    let lines2: Vec<&str> = src2.lines().collect();
    assert!(python_symbol_calls(&lines2, 1, 3, "f").is_empty());
}

#[test]
fn builds_python_symbol_edges_prefers_same_file() {
    fn sym(name: &str, path: &str) -> RepoIndexSymbolInput {
        RepoIndexSymbolInput {
            name: name.to_string(),
            kind: "function".to_string(),
            language: "python".to_string(),
            path: path.to_string(),
            package_path: None,
            start_line: 1,
            end_line: 2,
            signature: None,
            is_exported: true,
        }
    }
    let symbols = vec![
        sym("run", "a.py"),
        sym("step", "a.py"),
        sym("helper", "b.py"),
        sym("helper", "c.py"),
    ];
    let refs = vec![vec!["step".to_string(), "helper".to_string()], vec![], vec![], vec![]];
    let edges = build_symbol_edges(&symbols, &refs);
    let has = |f: usize, t: usize| edges.iter().any(|e| e.from_symbol_index == f && e.to_symbol_index == t);
    assert!(has(0, 1), "run->step (same file)");
    assert!(has(0, 2) && has(0, 3), "run->helper (ambiguity within cap)");
    assert!(edges.iter().all(|e| e.from_symbol_index != e.to_symbol_index));
}

#[test]
fn build_symbol_edges_skips_cross_language_same_name() {
    fn sym(name: &str, path: &str, language: &str) -> RepoIndexSymbolInput {
        RepoIndexSymbolInput {
            name: name.to_string(),
            kind: "function".to_string(),
            language: language.to_string(),
            path: path.to_string(),
            package_path: None,
            start_line: 1,
            end_line: 2,
            signature: None,
            is_exported: true,
        }
    }
    // Python `run` calls `helper`, but the only `helper` definitions are in
    // Rust/TS — no same-language target, so no edge should be emitted.
    let symbols = vec![
        sym("run", "app.py", "python"),
        sym("helper", "lib.rs", "rust"),
        sym("helper", "util.ts", "typescript"),
    ];
    let refs = vec![vec!["helper".to_string()], vec![], vec![]];
    assert!(
        build_symbol_edges(&symbols, &refs).is_empty(),
        "no cross-language call edges"
    );

    // With a Python `helper` present, only that one links (not the Rust one).
    let symbols2 = vec![
        sym("run", "app.py", "python"),
        sym("helper", "helpers.py", "python"),
        sym("helper", "lib.rs", "rust"),
    ];
    let refs2 = vec![vec!["helper".to_string()], vec![], vec![]];
    let edges2 = build_symbol_edges(&symbols2, &refs2);
    assert_eq!(edges2.len(), 1, "only the python helper links");
    assert_eq!(edges2[0].to_symbol_index, 1);
}

#[test]
fn indexes_python_symbol_edges() {
    let dir = std::env::temp_dir().join(format!("ocr-indexer-pysym-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write_file(&dir, "pkg/util.py", "def helper():\n    return 1\n");
    write_file(
        &dir,
        "pkg/main.py",
        "from pkg.util import helper\n\n\ndef run():\n    value = helper()\n    return _step(value)\n\n\ndef _step(v):\n    return v + 1\n",
    );
    run_git(&dir, &["init", "-q"]);
    run_git(&dir, &["add", "-A"]);
    run_git(&dir, &["commit", "-q", "-m", "init"]);

    let cancel = AtomicBool::new(false);
    let write = run_index(&dir, None, &cancel, &mut |_| {}).expect("index python repo");
    let idx = |name: &str| {
        write
            .symbols
            .iter()
            .position(|s| s.name == name)
            .unwrap_or_else(|| panic!("symbol {name} not found"))
    };
    let (run_i, helper_i, step_i) = (idx("run"), idx("helper"), idx("_step"));
    let has = |f: usize, t: usize| {
        write
            .symbol_edges
            .iter()
            .any(|e| e.from_symbol_index == f && e.to_symbol_index == t)
    };
    assert!(has(run_i, helper_i), "run should call helper (cross-file)");
    assert!(has(run_i, step_i), "run should call _step (same file)");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn indexes_rust_and_python_import_edges() {
    let dir = std::env::temp_dir().join(format!("ocr-indexer-polyglot-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    write_file(&dir, "backend/Cargo.toml", "[package]\nname = \"backend\"\n");
    write_file(
        &dir,
        "backend/src/review.rs",
        "use crate::{gitlab, indexer};\nuse crate::db::{Database, Repo};\npub mod local;\nfn review() {}\n",
    );
    write_file(
        &dir,
        "backend/src/db/mod.rs",
        "pub struct Database;\npub struct Repo;\n",
    );
    write_file(&dir, "backend/src/github.rs", "pub fn pr() {}\n");
    write_file(&dir, "backend/src/indexer.rs", "pub fn run_index() {}\n");
    write_file(&dir, "backend/src/local.rs", "pub fn helper() {}\n");
    write_file(&dir, "pyproject.toml", "[project]\nname = \"api\"\n");
    write_file(
        &dir,
        "app/main.py",
        "from .db import Session\nfrom app.core.config import Settings\nimport app.handlers\n",
    );
    write_file(&dir, "app/db.py", "class Session: pass\n");
    write_file(&dir, "app/core/config.py", "class Settings: pass\n");
    write_file(&dir, "app/handlers/__init__.py", "def handle(): pass\n");
    run_git(&dir, &["init", "-q"]);
    run_git(&dir, &["add", "-A"]);
    run_git(&dir, &["commit", "-q", "-m", "init"]);

    let cancel = AtomicBool::new(false);
    let write = run_index(&dir, None, &cancel, &mut |_| {}).expect("index polyglot repo");
    let review_edges: Vec<&RepoIndexEdgeInput> = write
        .edges
        .iter()
        .filter(|edge| edge.from_path == "backend/src/review.rs")
        .collect();
    for expected in [
        "backend/src/github.rs",
        "backend/src/indexer.rs",
        "backend/src/db/mod.rs",
        "backend/src/local.rs",
    ] {
        assert!(
            review_edges
                .iter()
                .any(|edge| edge.resolved && edge.to_path.as_deref() == Some(expected)),
            "missing Rust edge to {expected}"
        );
    }
    let py_edges: Vec<&RepoIndexEdgeInput> = write
        .edges
        .iter()
        .filter(|edge| edge.from_path == "app/main.py")
        .collect();
    for expected in ["app/db.py", "app/core/config.py", "app/handlers/__init__.py"] {
        assert!(
            py_edges
                .iter()
                .any(|edge| edge.resolved && edge.to_path.as_deref() == Some(expected)),
            "missing Python edge to {expected}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn extracts_symbols_for_supported_languages() {
    let cases = [
        (
            "src/app.ts",
            "typescript",
            "export function buildContextPack() {}\nclass LocalThing {}\n",
            vec![("buildContextPack", "function", true), ("LocalThing", "class", false)],
        ),
        (
            "src/lib.rs",
            "rust",
            "pub struct RepoIndex;\nfn internal() {}\npub(crate) async fn load_repo() {}\n",
            vec![
                ("RepoIndex", "struct", true),
                ("internal", "function", false),
                ("load_repo", "function", true),
            ],
        ),
        (
            "app/main.py",
            "python",
            "class ApiClient:\n    pass\ndef build_context():\n    return True\ndef _private():\n    pass\n",
            vec![
                ("ApiClient", "class", true),
                ("build_context", "function", true),
                ("_private", "function", false),
            ],
        ),
        (
            "cmd/api/main.go",
            "go",
            "type Server struct {}\ntype Handler interface {}\nfunc NewServer() {}\nfunc internal() {}\n",
            vec![
                ("Server", "struct", true),
                ("Handler", "interface", true),
                ("NewServer", "function", true),
                ("internal", "function", false),
            ],
        ),
    ];
    for (path, language, content, expected) in cases {
        let file = RepoIndexFileInput {
            path: path.to_string(),
            language: Some(language.to_string()),
            size: content.len() as i64,
            blob_sha: "sha".to_string(),
            package_path: None,
            is_binary: false,
            is_generated: false,
        };
        let symbols = extract_symbols(&file, content);
        for (name, kind, exported) in expected {
            assert!(
                symbols
                    .iter()
                    .any(|symbol| symbol.name == name && symbol.kind == kind && symbol.is_exported == exported),
                "missing {language} symbol {name}"
            );
        }
    }
}

#[test]
fn indexes_synthetic_monorepo() {
    let dir = std::env::temp_dir().join(format!("ocr-indexer-test-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    write_file(&dir, "package.json", "{\"name\":\"root\",\"private\":true}");
    write_file(&dir, "pnpm-workspace.yaml", "packages:\n  - packages/*\n");
    write_file(&dir, "packages/a/package.json", "{\"name\":\"@scope/a\"}");
    write_file(&dir, "packages/a/src/index.ts", "export const a = 1;\n");
    write_file(
        &dir,
        "packages/b/package.json",
        "{\"name\":\"@scope/b\",\"dependencies\":{\"@scope/a\":\"workspace:*\"}}",
    );
    write_file(&dir, "packages/b/tsconfig.json", "{\n  // jsonc comment\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"@/*\": [\"./src/*\"] },\n  },\n}");
    write_file(&dir, "packages/b/src/index.ts", "import { a } from '@scope/a';\nimport { y } from './util';\nimport { t } from '@/thing';\nimport React from 'react';\nexport const b = a + y + t;\n");
    write_file(&dir, "packages/b/src/util.ts", "export const y = 2;\n");
    write_file(&dir, "packages/b/src/thing.ts", "export const t = 3;\n");
    write_file(&dir, "assets/logo.png", "binarydata");

    run_git(&dir, &["init", "-q"]);
    run_git(&dir, &["add", "-A"]);
    run_git(&dir, &["commit", "-q", "-m", "init"]);

    let cancel = AtomicBool::new(false);
    let mut stages: Vec<String> = Vec::new();
    let write =
        run_index(&dir, None, &cancel, &mut |progress| stages.push(progress.stage)).expect("index synthetic repo");

    // completeness: every tracked file is represented
    assert_eq!(write.files.len() as i64, write.total_count);
    assert!(
        write.total_count >= 8,
        "expected all tracked files, got {}",
        write.total_count
    );

    // package graph: root + a + b
    assert_eq!(write.packages.len(), 3, "expected 3 packages");
    let names: HashSet<&str> = write.packages.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains("@scope/a") && names.contains("@scope/b") && names.contains("root"));

    // internal workspace dep b -> a
    assert!(
        write.deps.iter().any(|d| d.to_package_name == "@scope/a"),
        "expected internal dep b->a"
    );

    // import edges resolved (package name + relative)
    let from_b: Vec<&RepoIndexEdgeInput> = write
        .edges
        .iter()
        .filter(|e| e.from_path == "packages/b/src/index.ts")
        .collect();
    assert!(
        from_b.iter().any(|e| e.to_ref == "@scope/a" && e.resolved),
        "package import should resolve"
    );
    assert!(
        from_b.iter().any(|e| e.to_ref == "./util" && e.resolved),
        "relative import should resolve"
    );
    assert!(
        from_b.iter().any(|e| e.to_ref == "@/thing" && e.resolved),
        "tsconfig alias should resolve"
    );
    assert!(
        from_b.iter().any(|e| e.to_ref == "react" && !e.resolved),
        "external import stays unresolved"
    );

    // binary skipped
    assert!(write.skipped_count >= 1, "binary asset should be skipped");

    // file -> package assignment
    let b_index = write
        .files
        .iter()
        .find(|f| f.path == "packages/b/src/index.ts")
        .unwrap();
    assert_eq!(b_index.package_path.as_deref(), Some("packages/b"));

    assert!(write.symbols.iter().any(|symbol| symbol.name == "b"
        && symbol.kind == "const"
        && symbol.path == "packages/b/src/index.ts"
        && symbol.is_exported));

    // staged pipeline ran through parsing
    assert!(stages.iter().any(|s| s == "parsing_imports"));

    let _ = fs::remove_dir_all(&dir);
}
