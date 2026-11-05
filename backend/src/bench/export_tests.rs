use super::*;

const DIFF: &str = "diff --git a/src/pool.rs b/src/pool.rs
index 1111111..2222222 100644
--- a/src/pool.rs
+++ b/src/pool.rs
@@ -3,6 +3,5 @@ fn setup() {
 three
 four
-five
-six
+FIVE
 seven
 eight
@@ -20,3 +19,5 @@ fn other() {
 twenty
+added one
+added two
 twenty-one
 twenty-two
diff --git a/src/gone.rs b/src/gone.rs
deleted file mode 100644
--- a/src/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-first
-second
";

fn body(kind: &str, title: &str, detail: &str, action: &str) -> String {
    let (marker, details, fix) = if kind == "Question" {
        (
            crate::review::QUESTION_MARKER,
            crate::review::QUESTION_DETAILS_SUMMARY,
            crate::review::QUESTION_ACTION_SUMMARY,
        )
    } else {
        (
            crate::review::FINDING_MARKER,
            crate::review::FINDING_DETAILS_SUMMARY,
            crate::review::FINDING_ACTION_SUMMARY,
        )
    };
    format!(
        "**{marker} {title}**\n\n{detail}\n\n<details>\n<summary>{details}</summary>\n\n{} src/pool.rs:4 (abc)\n\n</details>\n\n<details>\n<summary>{fix}</summary>\n\n{action}\n\n</details>\n\n<details>\n<summary>{}</summary>\n\n```text\nFix it\n```\n\n</details>",
        crate::review::EVIDENCE_WHY_PREFIX,
        crate::review::AGENTS_PROMPT_SUMMARY,
    )
}

#[test]
fn points_read_comments_posted_before_the_detail_was_shown_once() {
    // Comments posted earlier repeat the explanation inside Details, before `Why:`.
    let old = body(
        "Finding",
        "Race on `conns`",
        "conns is read without the mutex.",
        "Hold the lock.",
    )
    .replacen(
        &format!("<summary>{}</summary>\n\n", crate::review::FINDING_DETAILS_SUMMARY),
        &format!(
            "<summary>{}</summary>\n\nconns is read without the mutex.\n\n",
            crate::review::FINDING_DETAILS_SUMMARY
        ),
        1,
    );
    assert_eq!(old.matches("conns is read without the mutex.").count(), 2);
    let comments = serde_json::json!({"comments": [{"path": "src/pool.rs", "line": 6, "side": "RIGHT", "body": old}]});
    let points = points(&comments.to_string(), DIFF).unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].detail, "conns is read without the mutex.");
    assert_eq!(points[0].action, "Hold the lock.");
}

#[test]
fn head_line_keeps_context_lines_and_moves_deleted_ones_to_their_place_in_head() {
    assert_eq!(head_line(DIFF, "src/pool.rs", 1), Some(1));
    assert_eq!(head_line(DIFF, "src/pool.rs", 4), Some(4));
    // Both deleted lines land on the head line that replaced them.
    assert_eq!(head_line(DIFF, "src/pool.rs", 5), Some(5));
    assert_eq!(head_line(DIFF, "src/pool.rs", 6), Some(5));
    assert_eq!(head_line(DIFF, "src/pool.rs", 7), Some(6));
    // Between hunks the first hunk's offset applies; after the last, both offsets.
    assert_eq!(head_line(DIFF, "src/pool.rs", 15), Some(14));
    assert_eq!(head_line(DIFF, "src/pool.rs", 21), Some(22));
    assert_eq!(head_line(DIFF, "src/pool.rs", 40), Some(41));
}

#[test]
fn head_line_handles_deleted_files_and_untouched_paths() {
    assert_eq!(head_line(DIFF, "src/gone.rs", 2), Some(1));
    assert_eq!(head_line(DIFF, "src/other.rs", 2), None);
}

#[test]
fn points_parse_the_finding_template_and_keep_questions_apart() {
    let comments = serde_json::json!({"comments": [
        {"path": "./src/pool.rs", "line": 6, "start_line": 4, "side": "RIGHT",
         "body": body("Finding", "Race on `conns`", "conns is read &lt;without&gt; the mutex.", "Hold the lock.")},
        {"path": "src/pool.rs", "line": 6, "side": "LEFT",
         "body": body("Question", "Is six still needed?", "It was removed.", "Confirm callers.")},
    ]});
    let points = points(&comments.to_string(), DIFF).unwrap();
    assert_eq!(points.len(), 2);
    let finding = &points[0];
    assert_eq!(finding.kind, "finding");
    assert_eq!(finding.path, "src/pool.rs");
    assert_eq!((finding.start_line, finding.end_line), (4, 6));
    assert!(!finding.mapped_from_left);
    assert_eq!(finding.title, "Race on `conns`");
    assert_eq!(finding.detail, "conns is read <without> the mutex.");
    assert_eq!(finding.action, "Hold the lock.");
    assert_eq!(
        finding.message(),
        "Race on `conns`. conns is read <without> the mutex. Correção sugerida: Hold the lock."
    );
    let question = &points[1];
    assert_eq!(question.kind, "question");
    assert_eq!((question.start_line, question.end_line), (5, 5));
    assert!(question.mapped_from_left);
    assert_eq!(question.action, "Confirm callers.");
}

#[test]
fn points_accept_the_legacy_bare_list_and_skip_comments_without_an_anchor() {
    let comments = serde_json::json!([
        {"path": "a.rs", "body": "no line"},
        {"path": "a.rs", "line": 3, "body": "plain text without a template"},
    ]);
    let points = points(&comments.to_string(), "").unwrap();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].title, "");
    assert_eq!(points[0].message(), "plain text without a template");
}

#[test]
fn a_cancelled_review_reports_what_its_finished_chunks_validated_once() {
    let budget = crate::review::budget::Budget::new(std::time::Duration::from_secs(60));
    assert_eq!(crate::bench::finished_chunk_comments(&budget), None);
    let comment = serde_json::json!({"path":"a.rs","line":3,"side":"RIGHT","body":"**Bug**"});
    let other = serde_json::json!({"path":"b.rs","line":1,"side":"RIGHT","body":"**Other**"});
    budget.validated(&serde_json::json!({"comments":[comment]}).to_string());
    budget.validated(&serde_json::json!({"commit_id":"head","comments":[comment, other]}).to_string());
    let merged: serde_json::Value =
        serde_json::from_str(&crate::bench::finished_chunk_comments(&budget).unwrap()).unwrap();
    assert_eq!(merged, serde_json::json!({"comments":[comment, other]}));
}

#[test]
fn summary_suggestions_export_as_findings_on_their_line() {
    let body = format!(
        "{}\n\n{}",
        crate::review::NO_FINDINGS_SUMMARY,
        crate::review::evidence::suggestions_section(&[
            serde_json::json!({"path":"src/a.rs","line":7,"title":"No test covers <empty> input","detail":"Nothing checks `parse`.","action":"Add a test for empty input."}),
        ])
    );
    let found = suggestions(&body);
    assert_eq!(found.len(), 1);
    let finding = &found[0];
    assert_eq!(
        (finding.kind.as_str(), finding.path.as_str()),
        ("suggestion", "src/a.rs")
    );
    assert_eq!((finding.start_line, finding.end_line), (7, 7));
    assert_eq!(finding.title, "No test covers <empty> input");
    assert_eq!(
        finding.message(),
        "No test covers <empty> input. Nothing checks `parse`. Correção sugerida: Add a test for empty input."
    );
    assert!(suggestions(crate::review::NO_FINDINGS_SUMMARY).is_empty());
}
