use super::*;

pub(super) const NOTE: &str = "One backend retrieval attempt is available. Before emitting findings/questions, identify essential missing code. To request it, write the normal report with nonempty missingEvidence, optional requestedFiles (up to four unique repository-relative file paths), and empty findings/questions/contextQuestions, then exit. Example extra field: \"requestedFiles\":[\"src/code.ts\"]. Request only files absent from this chunk. Only the full frozen review snapshot is available: no commands, URLs, directories or external/current repositories. The 160,000-byte selection target does not block required retrieval; separate artifact and prompt safety limits still apply. Unavailable-file failures remain incomplete. A final report requires empty missingEvidence/requestedFiles and coverage of exactly the assigned paths, not supporting files.\n";
const FINAL_NOTE: &str = "\nThis is the single allowed context recovery attempt. Read the supplied evidence and review all assigned paths again. No further retrieval is available. Keep requestedFiles empty, and report any remaining missing evidence honestly. Do not include supporting paths in reviewedFiles.\n";

pub(super) struct Attempt {
    pub usage: RunUsage,
    pub metrics: StageMetrics,
    pub started_at: i64,
    pub finished_at: i64,
}

pub(super) fn record_attempts(
    inputs: &ReviewInputs,
    chunk: &ReviewChunk,
    attempts: &[Attempt],
    status: &str,
    cache_key: &str,
    potential_cache_hit: bool,
) -> RunUsage {
    for (index, attempt) in attempts.iter().enumerate() {
        let last = index + 1 == attempts.len();
        record_stage(
            inputs,
            "chunk",
            Some(chunk.id as i64),
            if last { status } else { "retried" },
            (index == 0).then(|| cache_key.to_string()),
            index == 0 && potential_cache_hit,
            &attempt.usage,
            attempt.metrics,
            attempt.started_at,
            if last { now_millis() } else { attempt.finished_at },
        );
    }
    sum_usage(attempts.iter().map(|attempt| attempt.usage.clone()).collect())
}

fn snapshot_file(root: &Path, area: &str, path: &str) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|_| "Incomplete review: frozen context unavailable")?;
    let source = root.join(area).join(path);
    let metadata =
        fs::symlink_metadata(&source).map_err(|_| "Incomplete review: requested file is not in the frozen snapshot")?;
    if !metadata.is_file() || source.canonicalize().map_err(|error| error.to_string())? != source {
        return Err("Incomplete review: non-regular or redirected snapshot evidence".into());
    }
    Ok(source)
}

fn prepare(
    inputs: &ReviewInputs,
    context: &Path,
    map: &ReviewMap,
    original: &ReviewChunk,
    directory: &Path,
    head: &str,
    prior: Option<&str>,
    requested: &[String],
    cancel: &AtomicBool,
) -> Result<ReviewChunk, String> {
    check_cancel(cancel)?;
    let destination = directory.join("retrieval-1");
    let target = destination.join("context");
    let prompt = |chunk: &ReviewChunk| {
        format!(
            "{}{FINAL_NOTE}",
            render_chunk_prompt(inputs, &target, &destination.join("summary.md"), head, prior, chunk)
        )
    };
    let diff = fs::read_to_string(directory.join("context/PR_DIFF.patch")).map_err(|error| error.to_string())?;
    let patches = chunk_diff_bytes(map, &diff);
    let mut chunk = original.clone();
    chunk.related_files.clear();
    chunk.global_context_files.clear();
    for path in requested {
        if original.files.contains(path)
            || original.related_files.contains(path)
            || original.global_context_files.contains(path)
        {
            return Err(
                "Incomplete review: requested evidence is already supplied; no repeated inference without new context"
                    .into(),
            );
        }
        let area = if map.reviewable_files.contains(path) {
            "changed"
        } else if map.related_files.iter().any(|file| file.path == *path) {
            "related"
        } else {
            return Err(
                "Incomplete review: requested file is outside the frozen review snapshot; no model retry started"
                    .into(),
            );
        };
        snapshot_file(context, area, path)?;
        if area == "changed" {
            chunk.global_context_files.push(path.clone());
        } else {
            chunk.related_files.push(path.clone());
        }
    }
    chunk_context_budget(map, context, &chunk, &patches, prompt(&chunk).len())?
        .validate()
        .map_err(|error| {
            format!(
                "{error} Recovery for chunk {}; requested snapshot files: {}.",
                original.id,
                requested.join(", ")
            )
        })?;
    for (global, paths) in [(true, &original.global_context_files), (false, &original.related_files)] {
        for path in paths {
            if global {
                chunk.global_context_files.push(path.clone());
            } else {
                chunk.related_files.push(path.clone());
            }
            if chunk_context_budget(map, context, &chunk, &patches, prompt(&chunk).len())?
                .validate()
                .is_err()
            {
                if global {
                    chunk.global_context_files.pop();
                } else {
                    chunk.related_files.pop();
                }
            }
        }
    }
    check_cancel(cancel)?;
    fs::create_dir(&destination)
        .map_err(|_| "Incomplete review: recovery output directory already exists or cannot be created")?;
    fs::create_dir(&target).map_err(|error| error.to_string())?;
    write_review_map(&target, &chunk_review_map(map, &chunk))?;
    fs::copy(context.join("PR.md"), target.join("PR.md"))
        .map_err(|error| format!("Incomplete review: could not copy the canonical PR description: {error}"))?;
    for name in ["RELATED_ISSUES.md", "PENDING_REVIEW.md"] {
        if context.join(name).exists() {
            fs::copy(context.join(name), target.join(name)).map_err(|error| error.to_string())?;
        }
    }
    for (from, to, paths) in [
        ("changed", "changed", &chunk.files),
        ("changed", "global", &chunk.global_context_files),
        ("related", "related", &chunk.related_files),
    ] {
        for path in paths {
            check_cancel(cancel)?;
            if from == "changed" && to == "changed" && !context.join(from).join(path).exists() {
                continue;
            }
            let source = snapshot_file(context, from, path)?;
            let output = target.join(to).join(path);
            fs::create_dir_all(output.parent().ok_or("Missing snapshot parent")?).map_err(|error| error.to_string())?;
            fs::copy(source, output).map_err(|error| error.to_string())?;
        }
    }
    copy_chunk_conventions(context, &target, &chunk)?;
    for path in collect_files(&context.join("skill")) {
        let relative = path
            .strip_prefix(context.join("skill"))
            .map_err(|error| error.to_string())?;
        let output = target.join("skill").join(relative);
        fs::create_dir_all(output.parent().ok_or("Missing skill parent")?).map_err(|error| error.to_string())?;
        fs::copy(path, output).map_err(|error| error.to_string())?;
    }
    fs::write(target.join("CHUNK_SCOPE"), CHUNK_SCOPE_NOTE).map_err(|error| error.to_string())?;
    fs::write(target.join("CONTEXT.md"), render_chunk_context(map, &chunk)).map_err(|error| error.to_string())?;
    fs::write(target.join("PR_DIFF.patch"), diff).map_err(|error| error.to_string())?;
    fs::write(destination.join("prompt.md"), prompt(&chunk)).map_err(|error| error.to_string())?;
    validate_review_stage_inputs(&target, None, &destination.join("prompt.md"))?;
    Ok(chunk)
}

pub(super) fn run(
    inputs: &ReviewInputs,
    context: &Path,
    map: &ReviewMap,
    chunk: &ReviewChunk,
    directory: &Path,
    head: &str,
    prior: Option<&str>,
    cancel: &AtomicBool,
    launch: &mut dyn FnMut(&Path, &ReviewChunk) -> Result<(), String>,
) -> Result<(PathBuf, ReviewChunk), String> {
    check_cancel(cancel)?;
    let source_fingerprint = context_fingerprint(context)?;
    crate::review::policy::validate_prepared_inputs(dir_bytes(context) as usize, 0)?;
    let mut current = directory.to_path_buf();
    let mut effective = chunk.clone();
    for attempt in 0..=1 {
        check_cancel(cancel)?;
        let snapshot = current.join("context");
        let fingerprint = context_fingerprint(&snapshot)?;
        if attempt > 0 && context_fingerprint(context)? != source_fingerprint {
            return Err("Incomplete review: frozen source changed during context recovery".into());
        }
        launch(&current, &effective)?;
        if context_fingerprint(&snapshot)? != fingerprint || context_fingerprint(context)? != source_fingerprint {
            return Err("Incomplete review: reviewer modified the evidence snapshot.".into());
        }
        let requested = crate::review::evidence::context_requests(&current.join("report.json"))?;
        if requested.is_empty() {
            return Ok((current, effective));
        }
        if attempt == 1 || !snapshot.join("CONTEXT_RETRIEVAL.md").is_file() {
            return Err(
                "Incomplete review: context retrieval limit reached or retrieval unavailable; review manually".into(),
            );
        }
        effective = prepare(inputs, context, map, chunk, directory, head, prior, &requested, cancel)?;
        current = directory.join("retrieval-1");
    }
    unreachable!()
}
