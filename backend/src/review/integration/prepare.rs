use super::output::comment_key;
use super::*;

pub(crate) fn scoped_diff(diff: &str, files: &[String], inventory: &[String]) -> String {
    let mut sections = Vec::<String>::new();
    for line in diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") || sections.is_empty() {
            sections.push(String::new());
        }
        sections.last_mut().unwrap().push_str(line);
    }
    sections
        .into_iter()
        .filter(|section| {
            let mut old = None;
            let mut current = None;
            let mut renamed_from = None;
            for line in section.lines().take_while(|line| !line.starts_with("@@")) {
                if let Some(raw) = line.strip_prefix("--- ") {
                    old = crate::review_archive::decoded_git_path(raw)
                        .and_then(|path| path.strip_prefix("a/").map(str::to_string));
                }
                if let Some(raw) = line.strip_prefix("+++ ") {
                    current = crate::review_archive::decoded_git_path(raw)
                        .and_then(|path| path.strip_prefix("b/").map(str::to_string));
                }
                if let Some(raw) = line.strip_prefix("rename from ") {
                    renamed_from = crate::review_archive::decoded_git_path(raw);
                }
                if let Some(raw) = line.strip_prefix("rename to ") {
                    current = crate::review_archive::decoded_git_path(raw);
                }
            }
            let owner = current.or(old).or_else(|| {
                inventory
                    .iter()
                    .find(|path| section.lines().next() == Some(format!("diff --git a/{path} b/{path}").as_str()))
                    .cloned()
            });
            owner
                .as_ref()
                .is_none_or(|path| files.contains(path) || !inventory.contains(path))
                || renamed_from.as_ref().is_some_and(|path| files.contains(path))
        })
        .collect()
}

fn shared_inputs(master: &Path) -> Result<BTreeMap<String, Input>, String> {
    let mut output = BTreeMap::new();
    for name in ["PR.md", "RELATED_ISSUES.md", "PENDING_REVIEW.md"] {
        let path = master.join(name);
        if path.exists() {
            output.insert(format!("context/{name}"), Input::File(path));
        }
    }
    for area in ["skill", "conventions"] {
        let root = master.join(area);
        let mut pending = if root.exists() { vec![root] } else { vec![] };
        while let Some(path) = pending.pop() {
            let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.is_dir() {
                for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
                    pending.push(entry.map_err(|error| error.to_string())?.path());
                }
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(master)
                    .map_err(|error| error.to_string())?
                    .to_str()
                    .ok_or("Invalid shared evidence path")?;
                output.insert(format!("context/{relative}"), Input::File(path));
            } else {
                return Err("Non-regular shared integration evidence".into());
            }
        }
    }
    Ok(output)
}

/// Candidates one final-integration batch verifies before it is split by files.
pub(crate) const MAX_BATCH_CANDIDATES: usize = 30;

pub(crate) fn prepare(
    inputs: &ReviewInputs,
    master: &Path,
    map: &ReviewMap,
    full_diff: &str,
    folder: &Path,
    payload: &Payload,
    evidence_limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<Prepared>, String> {
    let frozen = inputs.frozen.as_ref().ok_or("Frozen integration requires an archive")?;
    let prior = frozen.archive.scope().prior_sha.as_deref();
    let before = prior.unwrap_or(&frozen.archive.scope().merge_base_sha);
    let publication = crate::review::evidence::publication_diff(master, full_diff)?;
    if map.reviewable_files.is_empty() {
        return Err("No integration scope prepared".into());
    }
    let mut ids = Vec::new();
    for candidate in payload.originals() {
        if !candidate["comment"]["path"]
            .as_str()
            .is_some_and(|path| map.reviewable_files.iter().any(|file| file == path))
        {
            return Err("Integration candidate is outside the assigned review scope".into());
        }
        ids.push(
            candidate["candidateId"]
                .as_str()
                .ok_or("Missing candidate identity")?
                .to_string(),
        );
    }
    let mut pending = VecDeque::from([Task {
        files: map.reviewable_files.clone(),
        ids,
    }]);
    let mut prepared = Vec::new();
    while let Some(task) = pending.pop_front() {
        check_cancel(cancel)?;
        let directory = folder.join(format!("batch-{:03}", prepared.len() + 1));
        let context = directory.join("context");
        let prompt_path = directory.join("prompt.md");
        let chunk = ReviewChunk {
            id: prepared.len() + 1,
            files: task.files.clone(),
            related_files: vec![],
            global_context_files: vec![],
            omitted_convention_files: vec![],
            risk_triggers: vec![],
            cross_chunk_group_ids: vec![],
        };
        let mut scoped = chunk_review_map(map, &chunk);
        scoped.reviewable_files = task.files.clone();
        let diff = scoped_diff(full_diff, &task.files, &map.changed_files);
        frozen
            .archive
            .validate_assigned(&task.files, &diff, before)
            .map_err(|error| error.to_string())?;
        let mut files = shared_inputs(master)?;
        for path in task
            .files
            .iter()
            .filter(|path| !crate::review_archive::in_agent_folder(path))
        {
            match frozen.archive.resolve(&frozen.archive.scope().head_sha, path) {
                Ok(_) => {
                    files.insert(format!("context/changed/{path}"), Input::Source(path.clone()));
                }
                Err(crate::review_archive::Error::Absent) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        files.insert(
            "context/review_map.json".into(),
            Input::Text(serde_json::to_string_pretty(&scoped).map_err(|error| error.to_string())?),
        );
        files.insert(
            "context/REVIEW_MAP.md".into(),
            Input::Text(render_review_map_md(&scoped)),
        );
        files.insert("context/CONTEXT.md".into(), Input::Text(format!("# Final integration batch\n\nAssigned files:\n{}\n\nThe frozen archive retains supporting code for cross-file verification. Other batches cover the remaining PR scope. Read PR.md, RELATED_ISSUES.md when present, and the complete supplied conventions and skill.\n", task.files.iter().map(|path| format!("- {path}\n")).collect::<String>())));
        files.insert("context/PR_DIFF.patch".into(), Input::Text(diff.clone()));
        if master.join("PUBLICATION_DIFF.patch").exists() {
            files.insert(
                "context/PUBLICATION_DIFF.patch".into(),
                Input::Text(scoped_diff(&publication, &task.files, &map.changed_files)),
            );
        }
        files.insert(
            "context/FROZEN.json".into(),
            Input::Text(serde_json::to_string(&frozen.archive.descriptor()).map_err(|error| error.to_string())?),
        );
        let candidates = task
            .ids
            .iter()
            .map(|id| payload.candidates.get(id).ok_or("Candidate input missing"))
            .collect::<Result<Vec<_>, _>>()?;
        files.insert("chunk_findings.json".into(), Input::Text(serde_json::to_string_pretty(&json!({"commit_id":inputs.fixed_head_sha.as_deref().unwrap_or(&map.head_sha),"comments":candidates})).map_err(|error| error.to_string())?));
        let notes = payload
            .notes
            .iter()
            .filter(|(paths, _)| paths.iter().any(|path| task.files.contains(path)))
            .map(|(_, note)| note)
            .collect::<Vec<_>>();
        files.insert(
            "chunk_summaries.md".into(),
            Input::Text(serde_json::to_string_pretty(&notes).map_err(|error| error.to_string())?),
        );
        let prompt = format!(
            "{}{BATCH_INSTRUCTIONS}",
            render_final_prompt(inputs, &context, &directory, &scoped)
        );
        let mut bytes = 0usize;
        let mut context_hashes = BTreeMap::new();
        for (name, value) in &files {
            check_cancel(cancel)?;
            let size = match value {
                Input::Text(text) => text.len(),
                Input::File(path) => {
                    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
                    if !metadata.is_file() {
                        return Err("Non-regular integration input".into());
                    }
                    usize::try_from(metadata.len()).map_err(|_| "Integration input size is unavailable")?
                }
                Input::Source(path) => frozen
                    .archive
                    .resolve(&frozen.archive.scope().head_sha, path)
                    .map_err(|error| error.to_string())?
                    .len(),
            };
            if name.starts_with("context/") {
                let hash = match value {
                    Input::Text(text) => crate::sandbox::digest(text.as_bytes()),
                    Input::File(path) => {
                        crate::review_archive::Stage::file_hash(path).map_err(|error| error.to_string())?
                    }
                    Input::Source(path) => crate::sandbox::digest(
                        frozen
                            .archive
                            .resolve(&frozen.archive.scope().head_sha, path)
                            .map_err(|error| error.to_string())?
                            .as_bytes(),
                    ),
                };
                context_hashes.insert(name.clone(), hash);
            }
            let name_bytes = name.len()
                + if name.starts_with("context/") {
                    0
                } else {
                    "integration/".len()
                };
            bytes = bytes
                .checked_add(size)
                .and_then(|value| value.checked_add(name_bytes))
                .ok_or("Integration input size overflow")?;
        }
        let stage = crate::review_archive::Stage {
            files: task.files.clone(),
            before: before.into(),
            snapshot_id: frozen.archive.descriptor().id,
            diff_hash: crate::sandbox::digest(diff.as_bytes()),
            context_hashes,
        };
        let stage = serde_json::to_string_pretty(&stage).map_err(|error| error.to_string())?;
        bytes = bytes
            .checked_add(stage.len() + "context/STAGE.json".len())
            .ok_or("Integration input size overflow")?;
        files.insert("context/STAGE.json".into(), Input::Text(stage));
        // Every candidate gets its own verification, so a crowded batch can outlast the stage
        // time limit; it is split by files, never dropping a candidate.
        let crowded = task.ids.len() > MAX_BATCH_CANDIDATES
            && payload
                .originals()
                .iter()
                .filter(|candidate| task.ids.iter().any(|id| candidate["candidateId"] == *id))
                .filter_map(|candidate| candidate["comment"]["path"].as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                > 1;
        let fits = bytes <= evidence_limit.min(crate::review::policy::MAX_EVIDENCE_BYTES)
            && prompt.len() <= crate::review::policy::MAX_PROMPT_BYTES
            && files.len() <= 5000
            && task.files.len() <= 1000
            && !crowded;
        if !fits {
            if task.files.len() > 1 {
                let middle = task.files.len() / 2;
                for paths in [&task.files[middle..], &task.files[..middle]] {
                    let ids = payload
                        .originals()
                        .iter()
                        .filter(|candidate| {
                            candidate["comment"]["path"]
                                .as_str()
                                .is_some_and(|path| paths.iter().any(|file| file == path))
                        })
                        .map(|candidate| candidate["candidateId"].as_str().unwrap().to_string())
                        .collect();
                    pending.push_front(Task {
                        files: paths.to_vec(),
                        ids,
                    });
                }
            } else {
                let mut groups = BTreeMap::<String, Vec<String>>::new();
                for candidate in payload
                    .originals()
                    .iter()
                    .filter(|candidate| task.ids.iter().any(|id| candidate["candidateId"] == *id))
                {
                    groups
                        .entry(comment_key(&candidate["comment"]))
                        .or_default()
                        .push(candidate["candidateId"].as_str().unwrap().to_string());
                }
                if groups.len() < 2 {
                    return Err("A single mandatory review input cannot be partitioned within the execution envelope; inspect shared instructions or the individual source/report without dropping evidence".into());
                }
                let groups = groups.into_values().collect::<Vec<_>>();
                let middle = groups.len() / 2;
                for group in [&groups[middle..], &groups[..middle]] {
                    pending.push_front(Task {
                        files: task.files.clone(),
                        ids: group.iter().flatten().cloned().collect(),
                    });
                }
            }
            continue;
        }
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        for (name, value) in files {
            check_cancel(cancel)?;
            crate::sandbox::safe_path(&name)?;
            let destination = directory.join(name);
            fs::create_dir_all(destination.parent().ok_or("Integration input has no parent")?)
                .map_err(|error| error.to_string())?;
            match value {
                Input::Text(text) => fs::write(destination, text).map_err(|error| error.to_string())?,
                Input::File(path) => {
                    fs::copy(path, destination).map_err(|error| error.to_string())?;
                }
                Input::Source(path) => fs::write(
                    destination,
                    frozen
                        .archive
                        .resolve(&frozen.archive.scope().head_sha, &path)
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
            }
        }
        fs::write(&prompt_path, prompt).map_err(|error| error.to_string())?;
        validate_review_stage_inputs(&context, Some(&directory), &prompt_path)?;
        prepared.push(Prepared {
            directory,
            context,
            prompt: prompt_path,
            files: task.files,
            diff,
            candidate_ids: task.ids,
        });
    }
    let covered = prepared
        .iter()
        .flat_map(|stage| stage.files.iter())
        .collect::<BTreeSet<_>>();
    if covered != map.reviewable_files.iter().collect() {
        return Err("Incomplete integration file coverage".into());
    }
    let candidate_ids = prepared
        .iter()
        .flat_map(|stage| stage.candidate_ids.iter())
        .collect::<Vec<_>>();
    if candidate_ids.len() != payload.originals().len()
        || candidate_ids.iter().copied().collect::<BTreeSet<_>>().len() != candidate_ids.len()
    {
        return Err("Incomplete integration candidate coverage".into());
    }
    Ok(prepared)
}

pub(crate) struct BatchOutput {
    pub files: Vec<String>,
    pub body: String,
    pub comments: Vec<Value>,
    pub decisions: Vec<Value>,
    pub report: Value,
    pub evidence_warnings: Vec<crate::review::warnings::EvidenceWarning>,
}
