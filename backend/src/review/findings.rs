use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingCandidate {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    pub origin: String,
    pub chunk_id: Option<i64>,
    pub ai_decision: String,
    pub ai_reason: Option<String>,
    pub default_selected: bool,
    pub selected: bool,
    pub blocked_reason: Option<String>,
    pub comment: Option<Value>,
    pub body: String,
    pub original_body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingAudit {
    pub head_sha: String,
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_warnings: Option<Vec<crate::review::warnings::EvidenceWarning>>,
    pub original_body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    pub candidates: Vec<FindingCandidate>,
}

pub fn point_kind(body: &str) -> &'static str {
    let mut fence = false;
    let (mut finding, mut question) = (false, false);
    for line in body.lines() {
        let line = line.trim_start();
        if line.starts_with("```") || line.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        let title = line.trim_start_matches(['*', '_', '>', '-', '#', ' ']);
        match crate::review::strip_point_marker(title) {
            Some(("finding", _)) => finding = true,
            Some(("question", _)) => question = true,
            _ => {}
        }
    }
    match (finding, question) {
        (true, true) => "mixed",
        (true, false) => "finding",
        (false, true) => "question",
        _ => "unknown",
    }
}

pub fn actionable(comments_json: Option<&str>, body: Option<&str>) -> bool {
    comments(comments_json).iter().any(|comment| {
        matches!(
            point_kind(comment["body"].as_str().unwrap_or_default()),
            "finding" | "mixed"
        )
    }) || body.is_some_and(|body| matches!(point_kind(body), "finding" | "mixed"))
}

/// Whether a run's review has nothing but the summary's suggestions: no inline comment, no finding
/// or question in its body, and a suggestions section.
pub fn suggestions_only(comments_json: Option<&str>, body: Option<&str>) -> bool {
    let (points, section) = crate::review::split_suggestions(body.unwrap_or_default());
    comments(comments_json).is_empty() && point_kind(points) == "unknown" && !section.trim().is_empty()
}

/// Whether a run's review asks questions and reports no finding, inline or in its body.
pub fn questions_only(comments_json: Option<&str>, body: Option<&str>) -> bool {
    let (points, _) = crate::review::split_suggestions(body.unwrap_or_default());
    let (findings, questions) = point_counts(comments_json);
    let body_kind = point_kind(points);
    findings == 0 && body_kind != "finding" && body_kind != "mixed" && (questions > 0 || body_kind == "question")
}

/// (findings, questions) among a run's inline comments. A comment that is both counts as both;
/// one without a marker counts as a finding. Older runs stored the comments as a bare array.
pub fn point_counts(comments_json: Option<&str>) -> (i64, i64) {
    let value = comments_json.and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let items = value
        .as_ref()
        .and_then(|value| value.get("comments").or(Some(value)))
        .and_then(Value::as_array);
    items
        .into_iter()
        .flatten()
        .fold((0, 0), |(findings, questions), comment| {
            match point_kind(comment["body"].as_str().unwrap_or_default()) {
                "question" => (findings, questions + 1),
                "mixed" => (findings + 1, questions + 1),
                _ => (findings + 1, questions),
            }
        })
}

/// (findings, questions) among the candidates a complete finding audit kept, leaving out ones
/// inherited from an earlier run. `None` when the audit is unreadable or incomplete: a seeded audit
/// holds undecided (`pending`) chunk candidates, and `unmapped` ones were neither kept nor discarded.
pub fn candidate_counts(audit_json: &str) -> Option<(i64, i64)> {
    let audit = serde_json::from_str::<Value>(audit_json).ok()?;
    if audit["complete"] != true {
        return None;
    }
    let candidates = audit.get("candidates")?.as_array()?;
    Some(
        candidates
            .iter()
            .filter(|candidate| candidate["origin"] != "inherited" && candidate["aiDecision"] == "kept")
            .fold((0, 0), |(findings, questions), candidate| {
                let kind = match candidate["kind"].as_str() {
                    Some(kind) if !kind.is_empty() => kind,
                    _ => point_kind(candidate["body"].as_str().unwrap_or_default()),
                };
                match kind {
                    "question" => (findings, questions + 1),
                    "mixed" => (findings + 1, questions + 1),
                    _ => (findings + 1, questions),
                }
            }),
    )
}

pub(crate) fn auto_post_event(mode: &str, comments_json: Option<&str>, body: Option<&str>) -> Option<&'static str> {
    match mode {
        "request_changes" if actionable(comments_json, body) => Some("REQUEST_CHANGES"),
        "comment" | "request_changes" => Some("COMMENT"),
        _ => None,
    }
}

pub fn tag_candidates(findings: &mut Value) {
    if let Some(items) = findings.get_mut("comments").and_then(Value::as_array_mut) {
        for (index, item) in items.iter_mut().enumerate() {
            item["candidateId"] = json!(format!("candidate-{}", index + 1));
        }
    }
}

fn canonical(comment: &Value) -> Value {
    let mut value =
        json!({ "path": comment["path"], "line": comment["line"], "side": comment["side"], "body": comment["body"] });
    for key in ["start_line", "start_side"] {
        if let Some(field) = comment.get(key) {
            value[key] = field.clone();
        }
    }
    value
}

fn key(comment: &Value) -> String {
    let mut value = canonical(comment);
    value["body"] = json!(comment["body"]
        .as_str()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" "));
    value.to_string()
}

fn comments(raw: Option<&str>) -> Vec<Value> {
    raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .and_then(|value| value.get("comments").and_then(Value::as_array).cloned())
        .unwrap_or_default()
}

fn blocker(comment: &Value, head_sha: &str, diff: &str) -> Option<String> {
    let raw = json!({ "commit_id": head_sha, "comments": [canonical(comment)] }).to_string();
    match crate::review::validate_final_comments(&raw, head_sha, diff) {
        Ok(valid) if comments(Some(&valid)).len() == 1 => None,
        Ok(_) => Some("Outside the PR diff or invalid inline comment structure.".to_string()),
        Err(error) => Some(error),
    }
}

fn candidate(
    id: String,
    origin: &str,
    chunk_id: Option<i64>,
    comment: Value,
    head: &str,
    diff: &str,
) -> FindingCandidate {
    FindingCandidate {
        id,
        kind: point_kind(comment["body"].as_str().unwrap_or_default()).into(),
        origin: origin.to_string(),
        chunk_id,
        ai_decision: "pending".to_string(),
        ai_reason: None,
        default_selected: false,
        selected: false,
        blocked_reason: blocker(&comment, head, diff),
        body: comment["body"].as_str().unwrap_or_default().to_string(),
        original_body: None,
        comment: Some(canonical(&comment)),
    }
}

pub fn seed_audit(sources: &Value, head: &str, diff: &str) -> FindingAudit {
    let candidates = sources
        .get("comments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, source)| {
            candidate(
                source["candidateId"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("candidate-{}", index + 1)),
                "chunk",
                source["chunkId"].as_i64(),
                source["comment"].clone(),
                head,
                diff,
            )
        })
        .collect();
    FindingAudit {
        head_sha: head.to_string(),
        complete: false,
        evidence_warnings: None,
        original_body: String::new(),
        diff: Some(diff.to_string()),
        candidates,
    }
}

pub fn finish_audit(
    mut audit: FindingAudit,
    raw_final: Option<&str>,
    accepted: Option<&str>,
    body: &str,
    decisions: Option<&str>,
    diff: &str,
) -> FindingAudit {
    let evidence = audit.diff.take().unwrap_or_else(|| diff.to_string());
    let diff = evidence.as_str();
    let raw = comments(raw_final);
    let accepted = comments(accepted);
    let decision_doc = decisions.and_then(|text| serde_json::from_str::<Value>(text).ok());
    let mut decisions: HashMap<String, &Value> = HashMap::new();
    let mut duplicate_ids = HashSet::new();
    for entry in decision_doc
        .as_ref()
        .and_then(|doc| doc["decisions"].as_array())
        .into_iter()
        .flatten()
    {
        if let Some(id) = entry["candidateId"].as_str() {
            if decisions.insert(id.to_string(), entry).is_some() {
                duplicate_ids.insert(id.to_string());
            }
        }
    }
    for id in duplicate_ids {
        decisions.remove(&id);
    }
    let mut represented = HashSet::new();
    for item in &mut audit.candidates {
        let original = item.comment.as_ref().unwrap().clone();
        let decision = decisions.get(&item.id);
        let exact = raw.iter().position(|comment| key(comment) == key(&original));
        let mapped = exact.or_else(|| {
            decision
                .filter(|entry| entry["decision"] == "keep")
                .and_then(|entry| entry["commentIndex"].as_u64())
                .and_then(|index| usize::try_from(index).ok())
                .filter(|index| *index < raw.len())
        });
        item.ai_decision = if mapped.is_some() {
            "kept"
        } else if decision.is_some_and(|entry| entry["decision"] == "discard") {
            "discarded"
        } else {
            "unmapped"
        }
        .to_string();
        item.ai_reason = decision
            .filter(|entry| match item.ai_decision.as_str() {
                "kept" => entry["decision"] == "keep",
                "discarded" => entry["decision"] == "discard",
                _ => false,
            })
            .and_then(|entry| entry["reason"].as_str())
            .filter(|reason| !reason.trim().is_empty())
            .map(|reason| reason.chars().take(4000).collect());
        let proposed = mapped.map(|index| &raw[index]).unwrap_or(&original);
        let retained = accepted
            .iter()
            .find(|comment| key(comment) == key(proposed))
            .or_else(|| accepted.iter().find(|comment| key(comment) == key(&original)));
        let chosen = canonical(retained.unwrap_or(proposed));
        represented.insert(key(&chosen));
        item.original_body =
            (chosen["body"] != original["body"]).then(|| original["body"].as_str().unwrap_or_default().to_string());
        item.body = chosen["body"].as_str().unwrap_or_default().to_string();
        item.blocked_reason = blocker(&chosen, &audit.head_sha, diff);
        if mapped.is_some() && retained.is_none() && item.blocked_reason.is_none() {
            item.blocked_reason = Some("Removed from the validated final output.".to_string());
        }
        item.default_selected = retained.is_some() && item.blocked_reason.is_none();
        item.selected = item.default_selected;
        item.comment = Some(chosen);
    }
    for (index, comment) in raw.iter().enumerate() {
        if !represented.insert(key(comment)) {
            continue;
        }
        let mut item = candidate(
            format!("final-{}", index + 1),
            "final",
            None,
            comment.clone(),
            &audit.head_sha,
            diff,
        );
        item.ai_decision = "kept".to_string();
        item.default_selected =
            accepted.iter().any(|accepted| key(accepted) == key(comment)) && item.blocked_reason.is_none();
        item.selected = item.default_selected;
        audit.candidates.push(item);
    }
    for (index, comment) in accepted.iter().enumerate() {
        if !represented.insert(key(comment)) {
            continue;
        }
        let mut item = candidate(
            format!("validated-{}", index + 1),
            "validated",
            None,
            comment.clone(),
            &audit.head_sha,
            diff,
        );
        item.ai_decision = "unmapped".to_string();
        item.default_selected = item.blocked_reason.is_none();
        item.selected = item.default_selected;
        audit.candidates.push(item);
    }
    let mut seen = HashMap::new();
    for item in &mut audit.candidates {
        if let Some(comment) = &item.comment {
            if let Some(first) = seen.get(&key(comment)) {
                item.blocked_reason = Some(format!("Duplicate of candidate {first}."));
                item.default_selected = false;
                item.selected = false;
            } else {
                seen.insert(key(comment), item.id.clone());
            }
        }
    }
    if crate::review::is_structured_review_point(body) {
        // Several points in one body each start with a marker title (or a legacy
        // attribution line before the title); split there so each becomes its own candidate.
        let starts: Vec<usize> = body
            .split_inclusive('\n')
            .scan(0, |offset, line| {
                let start = *offset;
                *offset += line.len();
                Some((start, line))
            })
            .filter(|(_, line)| {
                crate::review::is_review_point_title_line(line) || crate::review::is_review_attribution_line(line)
            })
            .map(|(start, _)| start)
            .collect();
        let pieces: Vec<String> = if starts.len() > 1 {
            starts
                .iter()
                .enumerate()
                .map(|(index, &start)| {
                    body[start..starts.get(index + 1).copied().unwrap_or(body.len())]
                        .trim_end()
                        .to_string()
                })
                .collect()
        } else {
            vec![body.to_string()]
        };
        for (index, body) in pieces.into_iter().enumerate() {
            let selected = (accepted.is_empty() || point_kind(&body) == "question")
                && crate::review::is_structured_review_point(&body);
            audit.candidates.push(FindingCandidate {
                id: format!("body-{}", index + 1),
                kind: point_kind(&body).into(),
                origin: "body".into(),
                chunk_id: None,
                ai_decision: "kept".into(),
                ai_reason: None,
                default_selected: selected,
                selected,
                blocked_reason: (!crate::review::is_structured_review_point(&body))
                    .then(|| "Invalid general review template".into()),
                comment: None,
                body,
                original_body: None,
            });
        }
    }
    for candidate in &mut audit.candidates {
        candidate.kind = point_kind(&candidate.body).into();
        if !matches!(candidate.kind.as_str(), "finding" | "question") {
            candidate.default_selected = false;
            candidate.selected = false;
        }
    }
    audit.original_body = body.to_string();
    audit.complete = true;
    audit
}

pub fn validate_decisions(sources: &Value, decisions: Option<&str>, output: &str) -> Result<(), String> {
    let incomplete = || {
        "Incomplete review: every candidate needs an explicit, consistent decision with evidence. Retry integration or review manually.".to_string()
    };
    let candidates = sources["comments"].as_array().ok_or_else(incomplete)?;
    if candidates.is_empty() {
        return Ok(());
    }
    let doc: Value = serde_json::from_str(decisions.ok_or_else(incomplete)?).map_err(|_| incomplete())?;
    let entries = doc["decisions"].as_array().ok_or_else(incomplete)?;
    let output: Value = serde_json::from_str(output).map_err(|_| incomplete())?;
    let comments = output["comments"].as_array().ok_or_else(incomplete)?;
    if entries.len() != candidates.len() {
        return Err(incomplete());
    }
    let mut seen = HashSet::new();
    for entry in entries {
        let id = entry["candidateId"].as_str().ok_or_else(incomplete)?;
        let candidate = candidates
            .iter()
            .find(|candidate| candidate["candidateId"] == id)
            .ok_or_else(incomplete)?;
        if !seen.insert(id) || entry["reason"].as_str().is_none_or(|reason| reason.trim().is_empty()) {
            return Err(incomplete());
        }
        match entry["decision"].as_str() {
            Some("keep") => {
                let index = entry["commentIndex"]
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or_else(incomplete)?;
                if comments.get(index).is_none() {
                    return Err(incomplete());
                }
            }
            Some("discard") => {
                if comments
                    .iter()
                    .any(|comment| key(comment) == key(&candidate["comment"]))
                {
                    return Err(incomplete());
                }
            }
            _ => return Err(incomplete()),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "findings_tests.rs"]
mod decision_tests;

pub fn all_candidates_discarded(sources: &Value, decisions: Option<&str>) -> bool {
    let ids: HashSet<_> = sources["comments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["candidateId"].as_str())
        .collect();
    if ids.is_empty() {
        return false;
    }
    let Some(doc) = decisions.and_then(|raw| serde_json::from_str::<Value>(raw).ok()) else {
        return false;
    };
    let Some(entries) = doc["decisions"].as_array() else {
        return false;
    };
    let mut seen = HashSet::new();
    entries.len() == ids.len()
        && entries.iter().all(|entry| {
            entry["candidateId"]
                .as_str()
                .is_some_and(|id| ids.contains(id) && seen.insert(id))
                && entry["decision"] == "discard"
                && entry["reason"].as_str().is_some_and(|reason| !reason.trim().is_empty())
        })
}

pub fn select(audit: &mut FindingAudit, ids: &[String]) -> Result<(), String> {
    let selected: HashSet<_> = ids.iter().map(String::as_str).collect();
    if selected.len() != ids.len() {
        return Err("Duplicate candidate ids in selection.".to_string());
    }
    for id in &selected {
        let candidate = audit
            .candidates
            .iter()
            .find(|candidate| candidate.id == *id)
            .ok_or("Unknown finding candidate.")?;
        if let Some(reason) = &candidate.blocked_reason {
            return Err(format!("Candidate {id} cannot be selected: {reason}"));
        }
    }
    for candidate in &mut audit.candidates {
        candidate.selected = selected.contains(candidate.id.as_str());
    }
    Ok(())
}

pub fn selection_payload(audit: &FindingAudit) -> (String, String) {
    let mut comments = Vec::new();
    let mut bodies = Vec::new();
    for candidate in audit
        .candidates
        .iter()
        .filter(|candidate| candidate.selected && candidate.blocked_reason.is_none())
    {
        if let Some(comment) = &candidate.comment {
            comments.push(canonical(comment));
        } else {
            // A body candidate can end with the summary's suggestions, which are appended once below.
            bodies.push(
                crate::review::split_suggestions(&candidate.body)
                    .0
                    .trim_end()
                    .to_string(),
            );
        }
    }
    let mut body = if bodies.is_empty() {
        format!("{} review comments selected for publication.", comments.len())
    } else {
        bodies.join("\n\n")
    };
    // Suggestions are no candidates: they stay listed in the summary whatever is selected.
    let (_, suggestions) = crate::review::split_suggestions(&audit.original_body);
    if !suggestions.trim().is_empty() {
        body = format!("{body}\n\n{}", suggestions.trim());
    }
    (
        body,
        json!({ "commit_id": audit.head_sha, "comments": comments }).to_string(),
    )
}
