use super::prepare::BatchOutput;
use super::*;

pub(crate) fn validate_output(
    inputs: &ReviewInputs,
    stage: &Prepared,
    payload: &Payload,
    head: &str,
) -> Result<BatchOutput, String> {
    let summary = stage.directory.join("summary.md");
    let comments = stage.directory.join("comments.json");
    let evidence_warnings = frozen::materialize(
        inputs,
        &stage.directory.join("report.json"),
        &stage.context,
        &stage.files,
        head,
        &stage.diff,
        &summary,
        &comments,
    )?;
    let publication = crate::review::evidence::publication_diff(&stage.context, &stage.diff)?;
    let raw = require_valid_comments(
        &fs::read_to_string(&comments).map_err(|error| error.to_string())?,
        head,
        &publication,
    )?;
    let decisions =
        fs::read_to_string(stage.directory.join("finding_decisions.json")).map_err(|error| error.to_string())?;
    let decisions = crate::review::evidence::remap_decisions(&stage.directory.join("report.json"), &decisions, &raw)?;
    let sources = payload.sources_for(&stage.candidate_ids);
    let (decisions, raw, unconfirmed) = complete_decisions(&sources, &decisions, &raw)?;
    crate::review::findings::validate_decisions(&sources, Some(&decisions), &raw)?;
    frozen::decisions(inputs, &decisions, &stage.context)?;
    let comments: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    let decisions: Value = serde_json::from_str(&decisions).map_err(|error| error.to_string())?;
    let entries = decisions["decisions"]
        .as_array()
        .ok_or("Missing integration decisions")?
        .clone();
    if entries.len() != stage.candidate_ids.len() {
        return Err("Unexpected integration decisions".into());
    }
    let mut evidence_warnings = evidence_warnings;
    evidence_warnings.extend(
        unconfirmed
            .into_iter()
            .map(|path| crate::review::warnings::EvidenceWarning {
                reason: "unconfirmed_candidate".into(),
                path: Some(path),
                reference: None,
            }),
    );
    Ok(BatchOutput {
        evidence_warnings,
        files: stage.files.clone(),
        body: fs::read_to_string(summary).map_err(|error| error.to_string())?,
        comments: comments["comments"]
            .as_array()
            .ok_or("Missing integrated comments")?
            .clone(),
        decisions: entries,
        report: serde_json::from_str(
            &fs::read_to_string(stage.directory.join("report.json")).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?,
    })
}

/// The output of a batch the final pass did not run because the review's time ran short: each
/// candidate keeps the comment validated in its chunk, marked unconfirmed.
pub(crate) fn unconfirmed(stage: &Prepared, payload: &Payload, head: &str) -> Result<BatchOutput, String> {
    let sources = payload.sources_for(&stage.candidate_ids);
    let empty = json!({"commit_id": head, "comments": []}).to_string();
    let (decisions, raw, paths) = complete_decisions(&sources, r#"{"decisions":[]}"#, &empty)?;
    let comments: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    let decisions: Value = serde_json::from_str(&decisions).map_err(|error| error.to_string())?;
    Ok(BatchOutput {
        evidence_warnings: paths
            .into_iter()
            .map(|path| crate::review::warnings::EvidenceWarning {
                reason: "unconfirmed_candidate".into(),
                path: Some(path),
                reference: None,
            })
            .collect(),
        files: stage.files.clone(),
        body: String::new(),
        comments: comments["comments"].as_array().cloned().unwrap_or_default(),
        decisions: decisions["decisions"].as_array().cloned().unwrap_or_default(),
        report: json!({"skipped": "time budget"}),
    })
}

/// Gives every candidate the final pass kept `unchanged` the comment already validated in its
/// chunk, so the final pass writes out only the findings it changes or adds. Every candidate it
/// left without a usable decision (none, a duplicate, an unknown kind or a keep pointing nowhere)
/// gets that comment too, kept and marked unconfirmed, so one gap in the final output never drops
/// validated findings or fails the review. Returns the completed decisions and comments and the
/// paths of unconfirmed candidates.
pub(crate) fn complete_decisions(
    sources: &Value,
    decisions: &str,
    raw: &str,
) -> Result<(String, String, Vec<String>), String> {
    let mut doc: Value = serde_json::from_str(decisions).map_err(|error| error.to_string())?;
    let mut output: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let candidates = sources["comments"].as_array().cloned().unwrap_or_default();
    let mut comments = output["comments"].as_array().cloned().unwrap_or_default();
    let entries = doc["decisions"].as_array().cloned().unwrap_or_default();
    let mut kept = Vec::new();
    let mut unconfirmed = Vec::new();
    let mut reused = false;
    let index_of = |comments: &mut Vec<Value>, comment: &Value| match comments
        .iter()
        .position(|existing| comment_key(existing) == comment_key(comment))
    {
        Some(index) => index,
        None => {
            comments.push(comment.clone());
            comments.len() - 1
        }
    };
    for candidate in &candidates {
        let id = &candidate["candidateId"];
        let comment = &candidate["comment"];
        let unchanged = entries.iter().find(|entry| {
            entry["candidateId"] == *id
                && entry["decision"] == "keep"
                && entry["unchanged"] == true
                // A keep that also points at a report finding means that finding: the rewrite wins.
                && entry["commentIndex"].is_null()
                && entry["reason"].as_str().is_some_and(|reason| !reason.trim().is_empty())
        });
        if let Some(entry) = unchanged.filter(|_| !comment.is_null()) {
            let mut entry = entry.clone();
            entry["commentIndex"] = json!(index_of(&mut comments, comment));
            kept.push(entry);
            reused = true;
            continue;
        }
        let usable = entries.iter().find(|entry| {
            entry["candidateId"] == *id
                && entry["reason"].as_str().is_some_and(|reason| !reason.trim().is_empty())
                && match entry["decision"].as_str() {
                    Some("keep") => entry["commentIndex"]
                        .as_u64()
                        .is_some_and(|index| (index as usize) < comments.len()),
                    Some("discard") => !comments
                        .iter()
                        .any(|comment| comment_key(comment) == comment_key(&candidate["comment"])),
                    _ => false,
                }
        });
        if let Some(entry) = usable {
            kept.push(entry.clone());
            continue;
        }
        if comment.is_null() {
            return Err("Incomplete review: an undecided candidate has no validated comment".into());
        }
        let index = index_of(&mut comments, comment);
        kept.push(json!({
            "candidateId": id,
            "decision": "keep",
            "commentIndex": index,
            "reason": "Not confirmed by the final pass; kept as validated in its chunk.",
        }));
        unconfirmed.push(comment["path"].as_str().unwrap_or_default().to_string());
    }
    if !reused && unconfirmed.is_empty() && kept.len() == entries.len() {
        return Ok((decisions.into(), raw.into(), unconfirmed));
    }
    doc["decisions"] = Value::Array(kept);
    output["comments"] = Value::Array(comments);
    Ok((doc.to_string(), output.to_string(), unconfirmed))
}

pub(crate) fn comment_key(comment: &Value) -> String {
    json!([comment["path"], comment["line"], comment["side"], comment["body"]]).to_string()
}

pub(crate) fn combine(
    inputs: &ReviewInputs,
    payload: &Payload,
    outputs: Vec<BatchOutput>,
    expected_files: &[String],
    head: &str,
    diff: &str,
    merge: &Path,
) -> Result<(String, String, Vec<crate::review::warnings::EvidenceWarning>), String> {
    let covered = outputs
        .iter()
        .flat_map(|output| output.files.iter())
        .collect::<BTreeSet<_>>();
    if outputs.is_empty() || covered != expected_files.iter().collect() {
        return Err("Incomplete final integration coverage".into());
    }
    let mut comments = Vec::new();
    let mut indices = BTreeMap::new();
    let mut decisions = Vec::new();
    let mut bodies = Vec::new();
    let mut reports = Vec::new();
    let mut evidence_warnings = Vec::new();
    let mut suggestions = payload.suggestions.clone();
    for output in outputs {
        evidence_warnings.extend(output.evidence_warnings);
        let mut remap = Vec::new();
        for comment in output.comments {
            let key = comment_key(&comment);
            let index = *indices.entry(key).or_insert_with(|| {
                let index = comments.len();
                comments.push(comment);
                index
            });
            remap.push(index);
        }
        for mut decision in output.decisions {
            if decision["decision"] == "keep" {
                let local = decision["commentIndex"]
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or("Invalid integrated comment index")?;
                decision["commentIndex"] =
                    json!(*remap.get(local).ok_or("Integrated comment index is out of range")?);
            }
            decisions.push(decision);
        }
        let (body, _) = crate::review::split_suggestions(&output.body);
        let body = body.trim_end().to_string();
        if is_structured_review_point(&body) && !bodies.contains(&body) {
            bodies.push(body);
        }
        suggestions.extend(output.report["suggestions"].as_array().cloned().unwrap_or_default());
        reports.push(output.report);
    }
    let raw = require_valid_comments(&json!({"commit_id":head,"comments":comments}).to_string(), head, diff)?;
    let archive = inputs.frozen.as_ref().ok_or("Frozen integration archive missing")?;
    let decisions =
        json!({"contractVersion":2,"snapshotId":archive.archive.descriptor().id,"decisions":decisions}).to_string();
    crate::review::findings::validate_decisions(&payload.sources, Some(&decisions), &raw)?;
    let mut body = if bodies.is_empty() {
        if comments.is_empty() {
            crate::review::NO_FINDINGS_SUMMARY.into()
        } else {
            format!("Produzi {} comentário(s) inline.", comments.len())
        }
    } else {
        bodies.join("\n\n")
    };
    let section = crate::review::evidence::suggestions_section(&suggestions);
    if !section.is_empty() {
        body.push_str("\n\n");
        body.push_str(&section);
    }
    validate_review_templates(&body, Some(&raw))?;
    fs::create_dir_all(merge).map_err(|error| error.to_string())?;
    fs::write(merge.join("finding_decisions.json"), decisions).map_err(|error| error.to_string())?;
    fs::write(merge.join("report.json"), json!({"batches":reports}).to_string()).map_err(|error| error.to_string())?;
    Ok((body, raw, crate::review::warnings::merge(evidence_warnings)))
}
