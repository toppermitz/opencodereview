use super::*;

pub(crate) fn should_chunk_prepared_review(
    inputs: &ReviewInputs,
    map: &ReviewMap,
    context: &Path,
    root: &Path,
) -> bool {
    should_run_sharded_review(inputs, map.reviewable_files.len())
        || inputs.frozen.is_some()
            && (dir_bytes(context) as usize > crate::review::policy::MAX_EVIDENCE_BYTES
                || map.reviewable_files.len() > 1000
                || render_prompt(inputs, context, &root.join("summary.md"), &map.reviewable_files).len()
                    > crate::review::policy::MAX_PROMPT_BYTES)
}

pub(crate) fn should_run_sharded_review(inputs: &ReviewInputs, reviewable_files: usize) -> bool {
    let chunk_size = if inputs.review_chunk_size == 0 {
        DEFAULT_REVIEW_CHUNK_SIZE
    } else {
        inputs.review_chunk_size.clamp(2, 20)
    };
    if reviewable_files <= chunk_size {
        return false;
    }
    match inputs.review_depth_mode.as_str() {
        "economic" => false,
        "deep" => true,
        "auto" => reviewable_files >= inputs.review_deep_threshold.max(chunk_size + 1),
        _ => reviewable_files >= DEFAULT_REVIEW_SHARD_THRESHOLD.max(chunk_size + 1),
    }
}

pub(crate) fn run_sharded_review(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    run_dir: &Path,
    context_dir: &Path,
    review_map: &ReviewMap,
    head_sha: &str,
    incremental_base: Option<&str>,
    base_rev: &str,
    full_pr_diff: &str,
    risk_plan: &ReviewContextPlan,
) -> Result<ReviewOutput, String> {
    progress(
        &sharded_phase_json("chunks", "running"),
        "Criando chunks de revisão com base em risco...",
    );
    let chunk_size = if inputs.review_chunk_size == 0 {
        DEFAULT_REVIEW_CHUNK_SIZE
    } else {
        inputs.review_chunk_size.clamp(2, 20)
    };
    let parallel_chunks = if inputs.review_parallel_chunks == 0 {
        DEFAULT_MAX_PARALLEL_REVIEW_CHUNKS
    } else {
        inputs.review_parallel_chunks.clamp(1, 20)
    };
    let planned: Result<Vec<ReviewChunk>, String> = (|| {
        let chunks = build_graph_chunks(review_map, context_dir, chunk_size, &|chunk| {
            planned_chunk_prompt(inputs, run_dir, chunk, head_sha, incremental_base).len()
        })?;
        if chunks.is_empty() {
            harness_log(run_dir, "sharding failed: no reviewable chunks");
            return Err("large review had no reviewable chunks".to_string());
        }
        Ok(chunks)
    })();
    let chunks = planned.inspect_err(|_| {
        progress(
            &chunk_planning_failure_phase(),
            "Preparação dos chunks falhou antes da execução do modelo.",
        );
    })?;
    harness_log(
        run_dir,
        &format!(
            "sharding planned reviewable_files={} chunks={} max_chunk_size={} max_parallel={}",
            review_map.reviewable_files.len(),
            chunks.len(),
            chunk_size,
            parallel_chunks
        ),
    );
    let _ = fs::write(
        run_dir.join("chunks.json"),
        serde_json::to_string_pretty(&chunks).unwrap_or_default(),
    );
    let config_path = run_dir.join("claude.settings.json");
    fs::write(&config_path, review_settings_json()).map_err(|error| error.to_string())?;
    let total_chunks = chunks.len();
    budget::with(inputs, |budget| {
        budget.plan(total_chunks, chunk_worker_count(inputs, total_chunks))
    });
    if inputs.frozen.is_some() {
        let prepared = (|| -> Result<(), String> {
            let mut stages = Vec::new();
            for chunk in &chunks {
                check_cancel(cancel)?;
                prepare_review_chunk(
                    inputs,
                    cancel,
                    run_dir,
                    context_dir,
                    review_map,
                    chunk,
                    head_sha,
                    incremental_base,
                    base_rev,
                )?;
                let directory = run_dir.join("chunks").join(format!("{:03}", chunk.id));
                stages.push((
                    directory.clone(),
                    directory.join("context"),
                    directory.join("prompt.md"),
                ));
            }
            let active_diff =
                fs::read_to_string(context_dir.join("PR_DIFF.patch")).map_err(|error| error.to_string())?;
            let final_stages = integration::prepare(
                inputs,
                context_dir,
                review_map,
                &active_diff,
                &run_dir.join("integration-static"),
                &integration::Payload::empty(head_sha),
                crate::review::policy::MAX_EVIDENCE_BYTES,
                cancel,
            )?;
            stages.extend(
                final_stages
                    .into_iter()
                    .map(|stage| (stage.directory, stage.context, stage.prompt)),
            );
            frozen::preflight(inputs, run_dir, &stages, cancel)
        })();
        prepared.inspect_err(|_| {
            progress(
                &preparation_failure_phase(true, cancel.load(Ordering::Relaxed)),
                "Global snapshot preflight failed; no inference started",
            )
        })?;
    }
    let mut results = Vec::new();
    for batch in chunks.chunks(chunks.len().max(1)) {
        check_cancel(cancel)?;
        progress(
            &sharded_phase_json("chunks", "running"),
            &format!(
                "progresso dos chunks: {}/{} concluídos, 0 em execução\n\nenfileirando {} chunks: {}",
                results.len(),
                total_chunks,
                batch.len(),
                batch
                    .iter()
                    .map(|chunk| chunk.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        let batch_results = run_review_chunk_batch(
            inputs,
            cancel,
            progress,
            run_dir,
            context_dir,
            review_map,
            batch,
            total_chunks,
            &config_path,
            head_sha,
            incremental_base,
            base_rev,
            results.len(),
        )?;
        for result in batch_results {
            harness_log(
                run_dir,
                &format!(
                    "chunk {} ok summary_bytes={} comments_bytes={} input_tokens={} output_tokens={}",
                    result.chunk.id,
                    result.summary.len(),
                    result.comments_json.as_ref().map(|raw| raw.len()).unwrap_or(0),
                    result.usage.input_tokens,
                    result.usage.output_tokens
                ),
            );
            results.push(result);
        }
    }
    check_cancel(cancel)?;
    progress(
        &sharded_phase_json("chunks", "ok"),
        &format!(
            "Concluídas {} revisões de chunk em {} arquivos revisáveis.",
            results.len(),
            review_map.reviewable_files.len()
        ),
    );
    if inputs.frozen.is_some() {
        return run_adaptive_final(
            inputs,
            cancel,
            progress,
            run_dir,
            context_dir,
            review_map,
            risk_plan,
            full_pr_diff,
            head_sha,
            &config_path,
            &results,
        );
    }
    progress(
        &sharded_phase_json("merge", "running"),
        "merge: deduplicating chunk findings...",
    );
    let merge_dir = run_dir.join("merge");
    fs::create_dir_all(&merge_dir).map_err(|error| error.to_string())?;
    let chunk_findings = merge_chunk_findings(&results, head_sha);
    inputs.open_db()?.store_finding_audit(
        inputs.run_id,
        &crate::review::findings::seed_audit(&chunk_findings, head_sha, full_pr_diff),
    )?;
    harness_log(
        run_dir,
        &format!(
            "merge start chunks={} findings={}",
            results.len(),
            chunk_findings
                .get("comments")
                .and_then(|v| v.as_array())
                .map(|items| items.len())
                .unwrap_or(0)
        ),
    );
    fs::write(
        merge_dir.join("chunk_findings.json"),
        serde_json::to_string_pretty(&chunk_findings).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    fs::write(merge_dir.join("chunk_summaries.md"), render_chunk_summaries(&results))
        .map_err(|error| error.to_string())?;
    fs::write(
        merge_dir.join("dedupe_candidates.json"),
        serde_json::to_string_pretty(&dedupe_candidates(&chunk_findings)).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    progress(
        &sharded_phase_json("merge", "ok"),
        &format!(
            "Collected {} chunk reports and {} inline candidates; prepared deduplication inputs for the final pass.",
            results.len(),
            chunk_findings
                .get("comments")
                .and_then(|value| value.as_array())
                .map_or(0, Vec::len)
        ),
    );
    progress(
        &sharded_phase_json("final", "queued"),
        "Preparando integração final e verificação de limites...",
    );
    let final_summary = run_dir.join("summary.md");
    let final_comments = run_dir.join("comments.json");
    fs::write(&final_summary, "").map_err(|error| error.to_string())?;
    fs::write(&final_comments, "").map_err(|error| error.to_string())?;
    fs::write(
        merge_dir.join("prompt.md"),
        render_final_prompt(inputs, context_dir, &merge_dir, review_map),
    )
    .map_err(|error| error.to_string())?;
    validate_review_stage_inputs(context_dir, Some(&merge_dir), &merge_dir.join("prompt.md")).inspect_err(|_| {
        let mut phases: Vec<serde_json::Value> =
            serde_json::from_str(&sharded_phase_json("final", "failed")).unwrap_or_default();
        for phase in &mut phases {
            if phase["id"] == "final" {
                phase["inferenceSkipped"] = true.into();
            }
        }
        progress(
            &serde_json::to_string(&phases).unwrap_or_default(),
            "Entradas finais excederam os limites do preflight; a inferência final não começou",
        );
    })?;
    let transcript = merge_dir.join("transcript.json");
    fs::write(&transcript, "").map_err(|error| error.to_string())?;
    harness_log(run_dir, "final integration start");
    let final_context_hash = context_fingerprint(context_dir)?;
    let final_started_at = now_millis();
    let mut final_progress = |phase: &str, tail: &str| {
        let detail = if tail.trim().is_empty() {
            "integração final: lendo mapa de revisão, diff completo, resumos dos chunks e candidatos a dedupe\n\nAguardando saída do Claude na passagem final...".to_string()
        } else {
            format!(
                "integração final: lendo mapa de revisão, diff completo, resumos dos chunks e candidatos a dedupe\n\n{tail}"
            )
        };
        progress(phase, &detail);
    };
    let execution = spawn_and_supervise(
        inputs,
        cancel,
        &mut final_progress,
        &SpawnPaths {
            config: &config_path,
            prompt: &merge_dir.join("prompt.md"),
            transcript: &transcript,
            stdout: &merge_dir.join("claude.stdout"),
            stderr: &merge_dir.join("claude.stderr"),
            progress_phase: "final",
            sharded_progress: true,
        },
    );
    let artifacts = execution.and_then(|()| {
        progress(&sharded_phase_json("final", "ok"), "Integração do Claude concluída. A saída está pronta para validação de âncoras e templates.");
        progress(&sharded_phase_json("validate", "running"), "Validando cobertura, evidências citadas, decisões e âncoras do diff");
        let mut evidence_warnings = results.iter().flat_map(|result| result.evidence_warnings.clone()).collect::<Vec<_>>();
        if context_fingerprint(context_dir)? != final_context_hash {
            return Err("Incomplete review: verifier modified the evidence snapshot.".into());
        }
        evidence_warnings.extend(frozen::materialize(inputs, &merge_dir.join("report.json"), context_dir, &review_map.reviewable_files, head_sha, full_pr_diff, &final_summary, &final_comments)?);
        let mut body = fs::read_to_string(&final_summary)
            .unwrap_or_default()
            .trim()
            .to_string();
        let comments_raw = fs::read_to_string(&final_comments).unwrap_or_default();
        let comments_json = match require_valid_comments(&comments_raw, head_sha, full_pr_diff) {
            Ok(comments_json) => comments_json,
            Err(error) => {
                harness_log(run_dir, &format!("validation error {error}"));
                return Err(error);
            }
        };
        // Safety net: the final integration pass must never silently drop findings
        // the chunk passes already validated. If it returns no inline comments, fall
        // back to the chunk findings (re-validated, anchored, and deduped the same
        // way) so real, diff-anchored findings are not lost to a final-pass miss.
        let decisions = fs::read_to_string(merge_dir.join("finding_decisions.json")).ok();
        let decisions = decisions.map(|raw| crate::review::evidence::remap_decisions(&merge_dir.join("report.json"), &raw, &comments_json)).transpose()?;
        crate::review::findings::validate_decisions(&chunk_findings, decisions.as_deref(), &comments_json)?;
        if let Some(raw) = decisions.as_deref() { frozen::decisions(inputs, raw, context_dir)?; }
        let explained_empty = crate::review::findings::all_candidates_discarded(&chunk_findings, decisions.as_deref());
        let (comments_json, used_fallback) = if comments_json_contains_items(Some(&comments_json)) || explained_empty {
            (comments_json, false)
        } else {
            let fallback = chunk_findings_as_comments(&chunk_findings, head_sha);
            match validate_final_comments(&fallback, head_sha, full_pr_diff) {
                Ok(from_chunks) if comments_json_contains_items(Some(&from_chunks)) => {
                    harness_log(
                        run_dir,
                        "final pass produced no inline comments; using validated chunk findings",
                    );
                    (from_chunks, true)
                }
                _ => (comments_json, false),
            }
        };
        let had_inline_comments = comments_json_contains_items(Some(&comments_json));
        let comments_json = sanitize_review_comments(Some(&comments_json))?
            .unwrap_or_else(|| "{\"comments\":[]}".to_string());
        // Synthesize the summary when it is empty, or when the fallback supplied the
        // inline comments: in the fallback case the final pass emitted no comments,
        // so its summary most likely says "nothing to flag" — which would contradict
        // the recovered findings we are about to post.
        if body.is_empty() || used_fallback {
            let comment_count = review_comment_count(Some(&comments_json));
            body = if comment_count == 0 {
                crate::review::NO_FINDINGS_SUMMARY.to_string()
            } else {
                format!("Produzi {comment_count} comentário(s) inline.")
            };
            harness_log(
                run_dir,
                "final integration fallback: synthesized summary for recovered findings",
            );
        }
        body = repair_body_only_review_point(body, Some(&comments_json));
        if !had_inline_comments || comments_json_contains_items(Some(&comments_json)) {
            validate_review_templates(&body, Some(&comments_json))?;
        }
        harness_log(
            run_dir,
            &format!(
                "final integration ok summary_bytes={} validated_comments_bytes={}",
                body.len(),
                comments_json.len()
            ),
        );
        progress(&sharded_phase_json("validate", "ok"), &format!("Validados {} achados inline: head SHA, âncoras do diff, JSON e templates de achado. Fallback de chunk usado: {}.", review_comment_count(Some(&comments_json)), used_fallback));
        Ok((body, comments_json, crate::review::warnings::merge(evidence_warnings)))
    });
    let final_usage = parse_transcript_usage(&transcript);
    record_stage(
        inputs,
        "final",
        None,
        match &artifacts {
            Ok(_) => "done",
            Err(error) if error == "cancelled" => "cancelled",
            Err(_) => "failed",
        },
        None,
        false,
        &final_usage,
        StageMetrics {
            prompt_bytes: file_bytes(&merge_dir.join("prompt.md")),
            diff_bytes: full_pr_diff.len() as i64,
            context_bytes: dir_bytes(&merge_dir),
            file_count: review_map.reviewable_files.len() as i64,
            related_file_count: review_map.related_files.len() as i64,
        },
        final_started_at,
        now_millis(),
    );
    let (body, comments_json, evidence_warnings) = artifacts?;
    let mut usage = sum_usage(results.iter().map(|r| r.usage.clone()).collect());
    usage = sum_usage(vec![usage, final_usage]);
    let risk = score_review_risk(risk_plan, full_pr_diff, Some(&comments_json));
    Ok(ReviewOutput {
        body,
        comments_json: Some(comments_json),
        evidence_warnings,
        usage,
        risk,
        no_new_changes: false,
    })
}

pub(crate) fn run_adaptive_final(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    run_dir: &Path,
    context: &Path,
    map: &ReviewMap,
    risk_plan: &ReviewContextPlan,
    full_diff: &str,
    head: &str,
    config: &Path,
    results: &[ChunkResult],
) -> Result<ReviewOutput, String> {
    let active_diff = fs::read_to_string(context.join("PR_DIFF.patch")).map_err(|error| error.to_string())?;
    let sources = merge_chunk_findings(results, head);
    inputs.open_db()?.store_finding_audit(
        inputs.run_id,
        &crate::review::findings::seed_audit(&sources, head, full_diff),
    )?;
    let payload = integration::Payload::from_results(results, sources)?;
    let merge = run_dir.join("merge");
    fs::create_dir_all(&merge).map_err(|error| error.to_string())?;
    fs::write(
        merge.join("chunk_findings.json"),
        serde_json::to_vec(&payload.sources).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    progress(
        &sharded_phase_json("merge", "running"),
        "Particionando a integração final sem perder candidatos nem cobertura de arquivos",
    );
    let stages = (|| -> Result<Vec<integration::Prepared>, String> {
        let stages = integration::prepare(
            inputs,
            context,
            map,
            &active_diff,
            &merge.join("batches"),
            &payload,
            crate::review::policy::MAX_EVIDENCE_BYTES,
            cancel,
        )?;
        let specs = stages
            .iter()
            .map(|stage| (stage.directory.clone(), stage.context.clone(), stage.prompt.clone()))
            .collect::<Vec<_>>();
        frozen::preflight_additional(inputs, &specs, cancel)?;
        Ok(stages)
    })()
    .inspect_err(|error| {
        let cancelled = error == "cancelled" || cancel.load(Ordering::Relaxed);
        let mut phases: Vec<serde_json::Value> = serde_json::from_str(&sharded_phase_json(
            "final",
            if cancelled { "cancelled" } else { "failed" },
        ))
        .unwrap_or_default();
        for phase in &mut phases {
            if phase["id"] == "final" {
                phase["inferenceSkipped"] = true.into();
            }
        }
        progress(
            &serde_json::to_string(&phases).unwrap_or_default(),
            if cancelled {
                "Preparação da integração final cancelada antes da inferência final"
            } else {
                "Preparação da integração final falhou antes da inferência final"
            },
        );
    })?;
    progress(
        &sharded_phase_json("merge", "ok"),
        &format!("Preparados {} lotes de integração final", stages.len()),
    );
    budget::with(inputs, |budget| budget.integrating());
    // Batches are independent by design, so they run side by side like chunks do.
    let (outputs, final_usage) = run_side_by_side(
        "final",
        stages.len(),
        chunk_worker_count(inputs, stages.len()),
        cancel,
        progress,
        |index, batch_cancel, batch_progress| {
            run_final_batch(
                inputs,
                batch_cancel,
                batch_progress,
                &stages[index],
                index,
                stages.len(),
                config,
                &payload,
                head,
            )
        },
    )?;
    let (body, comments_json, final_warnings) = integration::combine(
        inputs,
        &payload,
        outputs,
        &map.reviewable_files,
        head,
        full_diff,
        &merge,
    )?;
    let evidence_warnings = crate::review::warnings::merge(
        results
            .iter()
            .flat_map(|result| result.evidence_warnings.clone())
            .chain(final_warnings),
    );
    fs::write(run_dir.join("summary.md"), &body).map_err(|error| error.to_string())?;
    fs::write(run_dir.join("comments.json"), &comments_json).map_err(|error| error.to_string())?;
    progress(
        &sharded_phase_json("final", "ok"),
        &format!("Concluídos {} lotes de integração final", stages.len()),
    );
    progress(
        &sharded_phase_json("validate", "ok"),
        "Validados todos os arquivos e candidatos atribuídos nos lotes de integração final",
    );
    let usage = sum_usage(
        results
            .iter()
            .map(|result| result.usage.clone())
            .chain(final_usage)
            .collect(),
    );
    let risk = score_review_risk(risk_plan, full_diff, Some(&comments_json));
    Ok(ReviewOutput {
        body,
        comments_json: Some(comments_json),
        evidence_warnings,
        usage,
        risk,
        no_new_changes: false,
    })
}

enum BatchEvent<T> {
    Progress(usize, String, String),
    Done(usize, Result<T, String>, Option<RunUsage>),
}

type BatchRun<T> = (Result<T, String>, Option<RunUsage>);

/// Runs `count` independent executions, `workers` at a time, and returns their outputs in index
/// order with the usage of those that ran. The first failure cancels the rest and is the error.
/// While one execution runs, another reporting phase `phase_id` as queued (it waits for a runner)
/// leaves the shown phase as it was, so the review never looks idle while work is in progress.
pub(crate) fn run_side_by_side<T: Send>(
    phase_id: &str,
    count: usize,
    workers: usize,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    run: impl Fn(usize, &AtomicBool, &mut dyn FnMut(&str, &str)) -> BatchRun<T> + Sync,
) -> Result<(Vec<T>, Vec<RunUsage>), String> {
    let (tx, rx) = std::sync::mpsc::channel::<BatchEvent<T>>();
    let batch_cancel = AtomicBool::new(false);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut finished: BTreeMap<usize, BatchRun<T>> = BTreeMap::new();
    std::thread::scope(|scope| {
        for _ in 0..workers.clamp(1, count.max(1)) {
            let tx = tx.clone();
            let (batch_cancel, next, run) = (&batch_cancel, &next, &run);
            scope.spawn(move || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                if index >= count {
                    break;
                }
                let mut batch_progress = |phase: &str, tail: &str| {
                    let _ = tx.send(BatchEvent::Progress(index, phase.to_string(), tail.to_string()));
                };
                let (result, usage) = if batch_cancel.load(Ordering::Relaxed) {
                    (Err("cancelled".into()), None)
                } else {
                    run(index, batch_cancel, &mut batch_progress)
                };
                if result.is_err() {
                    batch_cancel.store(true, Ordering::Relaxed);
                }
                if tx.send(BatchEvent::Done(index, result, usage)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        let mut running = std::collections::BTreeSet::new();
        // The last waiting report of each batch whose wait was not shown because another ran.
        let mut waiting: BTreeMap<usize, (String, String)> = BTreeMap::new();
        let mut shown: Option<String> = None;
        while finished.len() < count {
            if cancel.load(Ordering::Relaxed) {
                batch_cancel.store(true, Ordering::Relaxed);
            }
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(BatchEvent::Progress(index, phase, tail)) => {
                    let status = serde_json::from_str::<Vec<serde_json::Value>>(&phase)
                        .ok()
                        .and_then(|phases| phases.into_iter().find(|item| item["id"] == phase_id))
                        .and_then(|item| item["status"].as_str().map(str::to_string));
                    // A batch waiting again (for example before an output repair) no longer runs.
                    if status.as_deref() == Some("queued") {
                        running.remove(&index);
                    }
                    waiting.remove(&index);
                    if status.as_deref() == Some("queued") && !running.is_empty() {
                        if let Some(shown) = &shown {
                            progress(shown, &tail);
                            waiting.insert(index, (phase, tail));
                            continue;
                        }
                    }
                    if matches!(status.as_deref(), Some("running" | "validating")) {
                        running.insert(index);
                    }
                    progress(&phase, &tail);
                    shown = Some(phase);
                }
                Ok(BatchEvent::Done(index, result, usage)) => {
                    running.remove(&index);
                    waiting.remove(&index);
                    finished.insert(index, (result, usage));
                    // The last running batch finished while others still wait: show that they wait.
                    if running.is_empty() {
                        if let Some((phase, tail)) = waiting.values().next_back().cloned() {
                            progress(&phase, &tail);
                            shown = Some(phase);
                            waiting.clear();
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    check_cancel(cancel)?;
    let mut outputs = Vec::new();
    let mut usages = Vec::new();
    let mut errors = Vec::new();
    for (result, usage) in finished.into_values() {
        usages.extend(usage);
        match result {
            Ok(output) => outputs.push(output),
            Err(error) => errors.push(error),
        }
    }
    // The execution that failed first cancels the others; report its error, not theirs.
    if let Some(error) = errors.iter().find(|error| *error != "cancelled").or(errors.first()) {
        return Err(error.clone());
    }
    if outputs.len() != count {
        return Err("Integração final terminou antes de todos os lotes reportarem".into());
    }
    Ok((outputs, usages))
}

/// Runs one final integration batch. Returns its output and, when it ran the model, its usage.
#[allow(clippy::too_many_arguments)]
fn run_final_batch(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    stage: &integration::Prepared,
    index: usize,
    total: usize,
    config: &Path,
    payload: &integration::Payload,
    head: &str,
) -> (Result<integration::BatchOutput, String>, Option<RunUsage>) {
    if cancel.load(Ordering::Relaxed) {
        return (Err("cancelled".into()), None);
    }
    if !budget::allows(inputs, budget::Step::FinalBatch) {
        if let Ok(output) = integration::unconfirmed(stage, payload, head) {
            progress(
                &sharded_phase_json("final", "running"),
                &format!(
                    "Lote de integração final {}/{} pulado: o tempo da revisão acabou, então os achados ficam como validados nos chunks",
                    index + 1,
                    total
                ),
            );
            return (Ok(output), None);
        }
    }
    let integrating = Instant::now();
    let transcript = stage.directory.join("transcript.json");
    let fingerprint = match context_fingerprint(&stage.context) {
        Ok(fingerprint) => fingerprint,
        Err(error) => return (Err(error), None),
    };
    let started = now_millis();
    progress(
        &sharded_phase_json("final", "queued"),
        &format!("Lote de integração final {}/{}", index + 1, total),
    );
    let mut batch_progress = |phase: &str, tail: &str| {
        progress(
            phase,
            &format!("Lote de integração final {}/{}\n{tail}", index + 1, total),
        )
    };
    let mut invalid = false;
    let result = spawn_and_supervise(
        inputs,
        cancel,
        &mut batch_progress,
        &SpawnPaths {
            config,
            prompt: &stage.prompt,
            transcript: &transcript,
            stdout: &stage.directory.join("claude.stdout"),
            stderr: &stage.directory.join("claude.stderr"),
            progress_phase: "final",
            sharded_progress: true,
        },
    )
    .and_then(|()| {
        if context_fingerprint(&stage.context)? != fingerprint {
            return Err("Incomplete review: verifier modified the evidence snapshot.".into());
        }
        output_repair::validate(
            inputs,
            cancel,
            &mut batch_progress,
            &SpawnPaths {
                config,
                prompt: &stage.prompt,
                transcript: &transcript,
                stdout: &stage.directory.join("claude.stdout"),
                stderr: &stage.directory.join("claude.stderr"),
                progress_phase: "final",
                sharded_progress: true,
            },
            &stage.context,
            || integration::validate_output(inputs, stage, payload, head),
        )
        .inspect_err(|error| invalid = error != "cancelled")
    });
    let usage = parse_transcript_usage(&transcript);
    record_stage(
        inputs,
        "final",
        (total > 1).then_some((index + 1) as i64),
        match &result {
            Ok(_) => "done",
            Err(error) if error == "cancelled" => "cancelled",
            Err(_) => "failed",
        },
        None,
        false,
        &usage,
        StageMetrics {
            prompt_bytes: file_bytes(&stage.prompt),
            diff_bytes: stage.diff.len() as i64,
            context_bytes: dir_bytes(&stage.context),
            file_count: stage.files.len() as i64,
            related_file_count: 0,
        },
        started,
        now_millis(),
    );
    budget::with(inputs, |budget| budget.batch_done(integrating.elapsed()));
    // A batch whose output never validates leaves its candidates as their chunks validated
    // them, marked unconfirmed, as when the final pass leaves one undecided.
    let result = match result {
        Err(error) if invalid => integration::unconfirmed(stage, payload, head)
            .inspect(|_| {
                progress(
                    &sharded_phase_json("final", "running"),
                    &format!(
                        "Lote de integração final {}/{} não validou, então os achados ficam como validados nos chunks: {error}",
                        index + 1,
                        total
                    ),
                )
            })
            .map_err(|_| error),
        result => result,
    };
    (result, Some(usage))
}

// Each chunk sends one Done over the channel, so boxing its result would only add an allocation.
#[allow(clippy::large_enum_variant)]
pub(crate) enum ChunkEvent {
    Progress {
        id: usize,
        phase: String,
        tail: String,
    },
    Done {
        id: usize,
        finished_at: i64,
        result: Result<ChunkResult, String>,
    },
}

pub(crate) fn chunk_worker_count(inputs: &ReviewInputs, count: usize) -> usize {
    let requested = if inputs.review_parallel_chunks == 0 {
        DEFAULT_MAX_PARALLEL_REVIEW_CHUNKS
    } else {
        inputs.review_parallel_chunks
    };
    count.min(requested.clamp(1, crate::review::policy::MAX_PROCESSES_PER_REVIEW))
}

pub(crate) fn run_review_chunk_batch(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    run_dir: &Path,
    context_dir: &Path,
    review_map: &ReviewMap,
    batch: &[ReviewChunk],
    total_chunks: usize,
    config_path: &Path,
    head_sha: &str,
    incremental_base: Option<&str>,
    base_rev: &str,
    completed_before: usize,
) -> Result<Vec<ChunkResult>, String> {
    let (tx, rx) = std::sync::mpsc::channel::<ChunkEvent>();
    let batch_cancel = AtomicBool::new(false);
    let mut snapshots = batch
        .iter()
        .map(|chunk| {
            (
                chunk.id,
                crate::review::progress::ChunkProgress::queued(chunk.id, chunk.files.len(), now_millis()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    progress(
        &crate::review::progress::with_chunks(&sharded_phase_json("chunks", "running"), &snapshots),
        &format!("progresso dos chunks: {completed_before}/{total_chunks} concluídos, 0 em execução"),
    );
    let next_chunk = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..chunk_worker_count(inputs, batch.len()) {
            let tx = tx.clone();
            let worker_cancel = &batch_cancel;
            let next_chunk = &next_chunk;
            handles.push(scope.spawn(move || {
                while let Some(chunk) = batch.get(next_chunk.fetch_add(1, Ordering::Relaxed)) {
                    let mut chunk_progress = |phase: &str, tail: &str| {
                        let _ = tx.send(ChunkEvent::Progress {
                            id: chunk.id,
                            phase: phase.to_string(),
                            tail: tail.to_string(),
                        });
                    };
                    let result = if cancel.load(Ordering::Relaxed) || worker_cancel.load(Ordering::Relaxed) {
                        Err("cancelled".into())
                    } else {
                        harness_log(
                            run_dir,
                            &format!("chunk {} start files={}", chunk.id, chunk.files.join(",")),
                        );
                        run_review_chunk(
                            inputs,
                            worker_cancel,
                            &mut chunk_progress,
                            run_dir,
                            context_dir,
                            review_map,
                            chunk,
                            total_chunks,
                            config_path,
                            head_sha,
                            incremental_base,
                            base_rev,
                        )
                    };
                    let failed = result.is_err();
                    if tx
                        .send(ChunkEvent::Done {
                            id: chunk.id,
                            finished_at: now_millis(),
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    if failed {
                        worker_cancel.store(true, Ordering::Relaxed);
                    }
                }
            }));
        }
        drop(tx);
        let mut results = Vec::new();
        let mut done = 0usize;
        let mut first_failure = None;
        let mut saw_cancellation = false;
        let mut statuses: BTreeMap<usize, &'static str> = BTreeMap::new();
        let mut started_at: BTreeMap<usize, Instant> = BTreeMap::new();
        let mut finished_at: BTreeMap<usize, Instant> = BTreeMap::new();
        let mut last_tails: BTreeMap<usize, String> = BTreeMap::new();
        let mut last_progress_id = None;
        for chunk in batch {
            statuses.insert(chunk.id, "queued");
        }
        while done < batch.len() {
            if cancel.load(Ordering::Relaxed) {
                batch_cancel.store(true, Ordering::Relaxed);
                saw_cancellation = true;
            }
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(ChunkEvent::Progress { id, phase, tail }) => {
                    if let Some(snapshot) = snapshots.get_mut(&id) {
                        snapshot.observe(&phase, &tail, now_millis());
                        let state = match snapshot.status.as_str() {
                            "running" => "running",
                            "validating" => "validating",
                            _ => "queued",
                        };
                        statuses.insert(id, state);
                        if snapshot.started_at.is_some() {
                            started_at.entry(id).or_insert_with(Instant::now);
                        }
                    }
                    last_progress_id = Some(id);
                    last_tails.insert(id, tail.clone());
                    progress(
                        &crate::review::progress::with_chunks(&phase, &snapshots),
                        &format!(
                            "progresso dos chunks: {}/{} concluídos, {} em execução\n{}\nchunk atual: {}\n\n{}",
                            completed_before + results.len(),
                            total_chunks,
                            statuses.values().filter(|status| **status == "running").count(),
                            chunk_status_tail(total_chunks, completed_before, &statuses, &started_at, &finished_at),
                            id,
                            tail
                        ),
                    );
                }
                Ok(ChunkEvent::Done {
                    id,
                    finished_at: completed_at,
                    result,
                }) => {
                    done += 1;
                    let cached = result.as_ref().is_ok_and(|result| result.cached);
                    let error = result.as_ref().err().map(String::as_str);
                    if let Some(snapshot) = snapshots.get_mut(&id) {
                        snapshot.finish(cached, error, completed_at);
                        if error == Some("cancelled") && !cancel.load(Ordering::Relaxed) && first_failure.is_some() {
                            snapshot.error = Some("Cancelled after another chunk failed".into());
                        }
                    }
                    statuses.insert(
                        id,
                        match error {
                            Some("cancelled") => "cancelled",
                            Some(_) => "failed",
                            None if cached => "cached",
                            None => "done",
                        },
                    );
                    finished_at.insert(id, Instant::now());
                    match result {
                        Ok(result) => {
                            if let Some(comments) = &result.comments_json {
                                budget::with(inputs, |budget| budget.validated(comments));
                            }
                            results.push(result)
                        }
                        Err(error) => {
                            batch_cancel.store(true, Ordering::Relaxed);
                            if error == "cancelled" {
                                saw_cancellation = true;
                            } else {
                                first_failure.get_or_insert(error);
                            }
                        }
                    }
                    progress(
                        &crate::review::progress::with_chunks(&sharded_phase_json("chunks", "running"), &snapshots),
                        &format!(
                            "progresso dos chunks: {}/{} concluídos, {} em execução\n{}\nchunk {} concluído",
                            completed_before + results.len(),
                            total_chunks,
                            statuses.values().filter(|status| **status == "running").count(),
                            chunk_status_tail(total_chunks, completed_before, &statuses, &started_at, &finished_at),
                            id
                        ),
                    );
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    let active_detail = chunk_active_tail(batch, &last_tails, last_progress_id);
                    progress(
                        &crate::review::progress::with_chunks(&sharded_phase_json("chunks", "running"), &snapshots),
                        &format!(
                            "progresso dos chunks: {}/{} concluídos, {} em execução\n{}\n{}",
                            completed_before + results.len(),
                            total_chunks,
                            statuses.values().filter(|status| **status == "running").count(),
                            chunk_status_tail(total_chunks, completed_before, &statuses, &started_at, &finished_at),
                            active_detail
                        ),
                    );
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        for handle in handles {
            if handle.join().is_err() {
                first_failure.get_or_insert_with(|| "chunk worker panicked".to_string());
            }
        }
        if done != batch.len() {
            first_failure.get_or_insert_with(|| "Chunk worker exited without a result".into());
            for snapshot in snapshots.values_mut().filter(|snapshot| snapshot.finished_at.is_none()) {
                snapshot.finish(false, Some("Worker exited without a result"), now_millis());
            }
        }
        let batch_error = if cancel.load(Ordering::Relaxed) {
            Some("cancelled".to_string())
        } else {
            first_failure.or_else(|| saw_cancellation.then(|| "cancelled".to_string()))
        };
        if let Some(error) = batch_error {
            let status = if error == "cancelled" { "cancelled" } else { "failed" };
            progress(
                &crate::review::progress::with_chunks(&sharded_phase_json("chunks", status), &snapshots),
                &error,
            );
            return Err(error);
        }
        check_cancel(cancel)?;
        results.sort_by_key(|result| result.chunk.id);
        Ok(results)
    })
}

pub(crate) fn chunk_active_tail(
    batch: &[ReviewChunk],
    last_tails: &BTreeMap<usize, String>,
    last_progress_id: Option<usize>,
) -> String {
    let Some(id) = last_progress_id else {
        return format!(
            "aguardando chunks {}",
            batch
                .iter()
                .map(|chunk| chunk.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    };
    let tail = last_tails.get(&id).map(String::as_str).unwrap_or("");
    if tail.trim().is_empty() {
        format!("chunk atual: {id}\n\nAguardando saída do Claude neste chunk...")
    } else {
        format!("chunk atual: {id}\n\n{tail}")
    }
}

pub(crate) fn chunk_status_tail(
    total_chunks: usize,
    completed_before: usize,
    statuses: &BTreeMap<usize, &'static str>,
    started_at: &BTreeMap<usize, Instant>,
    finished_at: &BTreeMap<usize, Instant>,
) -> String {
    let now = Instant::now();
    let mut lines = Vec::with_capacity(total_chunks + 1);
    lines.push("status dos chunks:".to_string());
    for id in 1..=total_chunks {
        if id <= completed_before {
            lines.push(format!("{id} concluído"));
        } else if let Some(status) = statuses.get(&id) {
            let label = match *status {
                "running" => "em execução",
                "queued" => "na fila",
                "failed" => "falhou",
                "done" => "concluído",
                "cancelled" => "cancelado",
                "validating" => "validando",
                other => other,
            };
            let start = started_at.get(&id).copied().unwrap_or(now);
            let end = finished_at.get(&id).copied().unwrap_or(now);
            lines.push(format!(
                "{id} {label} {}",
                format_duration_secs(end.duration_since(start).as_secs())
            ));
        } else {
            lines.push(format!("{id} na fila"));
        }
    }
    lines.join("\n")
}

pub(crate) fn format_duration_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

pub(crate) fn prepare_review_chunk(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    run_dir: &Path,
    context_dir: &Path,
    review_map: &ReviewMap,
    chunk: &ReviewChunk,
    head_sha: &str,
    incremental_base: Option<&str>,
    base_rev: &str,
) -> Result<(), String> {
    let chunk_dir = run_dir.join("chunks").join(format!("{:03}", chunk.id));
    let chunk_context = chunk_dir.join("context");
    fs::create_dir_all(&chunk_context).map_err(|error| error.to_string())?;
    fs::write(chunk_context.join("CHUNK_SCOPE"), CHUNK_SCOPE_NOTE).map_err(|error| error.to_string())?;
    if inputs.frozen.is_none() {
        fs::write(chunk_context.join("CONTEXT_RETRIEVAL.md"), recovery::NOTE).map_err(|error| error.to_string())?;
    }
    let chunk_json = serde_json::to_string_pretty(chunk).unwrap_or_default();
    let _ = fs::write(chunk_dir.join("chunk.json"), &chunk_json);
    harness_log(
        run_dir,
        &format!("chunk {} context dir {}", chunk.id, chunk_context.display()),
    );
    write_review_map(&chunk_context, &chunk_review_map(review_map, chunk))?;
    fs::copy(context_dir.join("PR.md"), chunk_context.join("PR.md"))
        .map_err(|error| format!("Incomplete review: could not copy the canonical PR description: {error}"))?;
    copy_if_exists(
        &context_dir.join("RELATED_ISSUES.md"),
        &chunk_context.join("RELATED_ISSUES.md"),
    );
    copy_if_exists(
        &context_dir.join("PENDING_REVIEW.md"),
        &chunk_context.join("PENDING_REVIEW.md"),
    );
    copy_dir_subset(
        &context_dir.join("changed"),
        &chunk_context.join("changed"),
        &chunk.files,
    );
    copy_dir_subset(
        &context_dir.join("related"),
        &chunk_context.join("related"),
        &chunk.related_files,
    );
    copy_dir_subset(
        &context_dir.join("changed"),
        &chunk_context.join("global"),
        &chunk.global_context_files,
    );
    copy_chunk_conventions(context_dir, &chunk_context, chunk)?;
    copy_tree(&context_dir.join("skill"), &chunk_context.join("skill"));
    let chunk_diff_path = chunk_context.join("PR_DIFF.patch");
    let patch = if let Some(frozen) = &inputs.frozen {
        let range = format!(
            "{}..{head_sha}",
            incremental_base.unwrap_or(&frozen.archive.scope().merge_base_sha)
        );
        // Paths are names, not globs: `[key].ts` must not match `k.ts`.
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--full-index",
            range.as_str(),
            "--",
        ];
        args.extend(chunk.files.iter().map(String::as_str));
        // A file in another encoding, such as Latin-1, never fails the review, as in diff_local_paths.
        String::from_utf8_lossy(&indexer::git_frozen_with_input(&inputs.repo_path, &args, cancel, &[])?).into_owned()
    } else {
        diff_local_paths(
            &inputs.repo_path,
            &review_range(incremental_base, base_rev, head_sha),
            &chunk.files,
            cancel,
        )
    };
    fs::write(&chunk_diff_path, &patch).map_err(|error| error.to_string())?;
    let publication = crate::review::evidence::publication_diff(context_dir, &patch)?;
    if context_dir.join("PUBLICATION_DIFF.patch").exists() {
        fs::write(
            chunk_context.join("PUBLICATION_DIFF.patch"),
            integration::scoped_diff(&publication, &chunk.files, &review_map.changed_files),
        )
        .map_err(|error| error.to_string())?;
    }
    fs::write(
        chunk_context.join("CONTEXT.md"),
        render_chunk_context(review_map, chunk),
    )
    .map_err(|error| error.to_string())?;
    if let Some(frozen) = &inputs.frozen {
        frozen::prepare_stage(
            inputs,
            &chunk_context,
            &chunk.files,
            &patch,
            incremental_base.unwrap_or(&frozen.archive.scope().merge_base_sha),
        )?;
    }
    let summary = chunk_dir.join("summary.md");
    let comments = chunk_dir.join("comments.json");
    let prompt = chunk_dir.join("prompt.md");
    let transcript = chunk_dir.join("transcript.json");
    fs::write(&summary, "").map_err(|error| error.to_string())?;
    fs::write(&comments, "").map_err(|error| error.to_string())?;
    fs::write(&transcript, "").map_err(|error| error.to_string())?;
    fs::write(
        &prompt,
        planned_chunk_prompt(inputs, run_dir, chunk, head_sha, incremental_base),
    )
    .map_err(|error| error.to_string())?;
    validate_review_stage_inputs(&chunk_context, None, &prompt)
}

pub(crate) fn planned_chunk_prompt(
    inputs: &ReviewInputs,
    run_dir: &Path,
    chunk: &ReviewChunk,
    head_sha: &str,
    incremental_base: Option<&str>,
) -> String {
    let directory = run_dir.join("chunks").join(format!("{:03}", chunk.id));
    render_chunk_prompt(
        inputs,
        &directory.join("context"),
        &directory.join("summary.md"),
        head_sha,
        incremental_base,
        chunk,
    )
}

pub(crate) fn run_review_chunk(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    run_dir: &Path,
    context_dir: &Path,
    review_map: &ReviewMap,
    chunk: &ReviewChunk,
    total_chunks: usize,
    config_path: &Path,
    head_sha: &str,
    incremental_base: Option<&str>,
    base_rev: &str,
) -> Result<ChunkResult, String> {
    if inputs.frozen.is_none() {
        prepare_review_chunk(
            inputs,
            cancel,
            run_dir,
            context_dir,
            review_map,
            chunk,
            head_sha,
            incremental_base,
            base_rev,
        )?;
    }
    let chunk_json = serde_json::to_string_pretty(chunk).unwrap_or_default();
    let chunk_dir = run_dir.join("chunks").join(format!("{:03}", chunk.id));
    let chunk_context = chunk_dir.join("context");
    let summary = chunk_dir.join("summary.md");
    let comments = chunk_dir.join("comments.json");
    let prompt = chunk_dir.join("prompt.md");
    let chunk_diff_path = chunk_context.join("PR_DIFF.patch");
    let prompt_hash = sha256_hex(
        fs::read_to_string(&prompt)
            .map_err(|error| error.to_string())?
            .replace(&run_dir.to_string_lossy().to_string(), "$RUN")
            .as_bytes(),
    );
    let context_hash = context_fingerprint(&chunk_context)?;
    let chunk_diff = fs::read_to_string(&chunk_diff_path).map_err(|error| error.to_string())?;
    let diff_hash = sha256_hex(&fs::read(&chunk_diff_path).unwrap_or_default());
    let skill_hash = sha256_hex(inputs.skill_content.as_bytes());
    let cache_payload = serde_json::json!({
        "version": 2,
        "archiveId": inputs.frozen.as_ref().map(|frozen| frozen.archive.descriptor().id),
        "repoId": inputs.repo_id,
        "headSha": head_sha,
        "priorSha": incremental_base,
        "model": inputs.model,
        "promptVersion": REVIEW_PROMPT_VERSION,
        "skillHash": skill_hash,
        "indexSpecVersion": INDEX_SPEC_VERSION,
        "chunk": chunk,
        "promptHash": prompt_hash,
        "contextHash": context_hash,
        "diffHash": diff_hash,
        "lensPass": lens::enabled(inputs),
    });
    let cache_key = sha256_hex(serde_json::to_string(&cache_payload).unwrap_or_default().as_bytes());
    let potential_cache_hit = inputs
        .open_db()
        .and_then(|db| db.review_chunk_cache_exists(&cache_key))
        .unwrap_or(false);
    if should_reuse_chunk_results(inputs) {
        if let Some((report, _)) = inputs.open_db()?.cached_review_chunk(&cache_key)? {
            fs::write(chunk_dir.join("report.json"), &report).map_err(|error| error.to_string())?;
            let evidence_warnings = frozen::materialize(
                inputs,
                &chunk_dir.join("report.json"),
                &chunk_context,
                &chunk.files,
                head_sha,
                &chunk_diff,
                &summary,
                &comments,
            )?;
            let comments_json = fs::read_to_string(&comments).map_err(|error| error.to_string())?;
            let usage = RunUsage::default();
            record_stage(
                inputs,
                "chunk",
                Some(chunk.id as i64),
                "cached",
                Some(cache_key),
                false,
                &usage,
                StageMetrics {
                    prompt_bytes: file_bytes(&prompt),
                    diff_bytes: chunk_diff.len() as i64,
                    context_bytes: dir_bytes(&chunk_context),
                    file_count: chunk.files.len() as i64,
                    related_file_count: chunk.related_files.len() as i64,
                },
                now_millis(),
                now_millis(),
            );
            progress(
                &sharded_phase_json("chunks", "running"),
                &format!("Chunk {} restaurado do cache de snapshot validado", chunk.id),
            );
            return Ok(ChunkResult {
                cached: true,
                chunk: chunk.clone(),
                summary: report,
                comments_json: Some(comments_json),
                evidence_warnings,
                usage,
            });
        }
    }
    let chunk_files = chunk.files.join(", ");
    let mut chunk_progress = |phase: &str, tail: &str| {
        let detail = if tail.trim().is_empty() {
            format!(
                "chunk {}/{}: revisando {} arquivo(s)\n{}\n\nAguardando saída do Claude neste chunk...",
                chunk.id,
                total_chunks,
                chunk.files.len(),
                chunk_files
            )
        } else {
            format!(
                "chunk {}/{}: revisando {} arquivo(s)\n{}\n\n{}",
                chunk.id,
                total_chunks,
                chunk.files.len(),
                chunk_files,
                tail
            )
        };
        progress(phase, &detail);
    };
    let mut attempts = Vec::new();
    let mut launch = |directory: &Path, effective: &ReviewChunk| {
        let started_at = now_millis();
        if directory != chunk_dir {
            chunk_progress(
                &crate::review::progress::execution_phase(
                    &sharded_phase_json("chunks", "running"),
                    "chunks",
                    "queued",
                    started_at,
                ),
                "Evidência congelada recuperada; enfileirando a única tentativa adicional de revisão",
            );
        }
        let execution = spawn_and_supervise(
            inputs,
            cancel,
            &mut chunk_progress,
            &SpawnPaths {
                config: config_path,
                prompt: &directory.join("prompt.md"),
                transcript: &directory.join("transcript.json"),
                stdout: &directory.join("claude.stdout"),
                stderr: &directory.join("claude.stderr"),
                progress_phase: "chunks",
                sharded_progress: true,
            },
        );
        attempts.push(recovery::Attempt {
            usage: parse_transcript_usage(&directory.join("transcript.json")),
            metrics: StageMetrics {
                prompt_bytes: file_bytes(&directory.join("prompt.md")),
                diff_bytes: file_bytes(&chunk_diff_path),
                context_bytes: dir_bytes(&directory.join("context")),
                file_count: chunk.files.len() as i64,
                related_file_count: effective.related_files.len() as i64,
            },
            started_at,
            finished_at: now_millis(),
        });
        execution
    };
    let reviewing = Instant::now();
    let execution = if inputs.frozen.is_none() {
        recovery::run(
            inputs,
            context_dir,
            review_map,
            chunk,
            &chunk_dir,
            head_sha,
            incremental_base,
            cancel,
            &mut launch,
        )
    } else {
        launch(&chunk_dir, chunk).map(|()| (chunk_dir.clone(), chunk.clone()))
    };
    let mut lens_usage = RunUsage::default();
    let artifacts = execution.and_then(|(directory, _)| {
        if context_fingerprint(&chunk_context)? != context_hash {
            return Err("Incomplete review: reviewer modified the evidence snapshot.".into());
        }
        let evidence_warnings = output_repair::validate_chunk(
            inputs,
            cancel,
            &mut chunk_progress,
            &SpawnPaths {
                config: config_path,
                prompt: &directory.join("prompt.md"),
                transcript: &directory.join("transcript.json"),
                stdout: &directory.join("claude.stdout"),
                stderr: &directory.join("claude.stderr"),
                progress_phase: "chunks",
                sharded_progress: true,
            },
            &directory.join("context"),
            || {
                frozen::materialize(
                    inputs,
                    &directory.join("report.json"),
                    &directory.join("context"),
                    &chunk.files,
                    head_sha,
                    &chunk_diff,
                    &summary,
                    &comments,
                )
            },
        )?;
        budget::with(inputs, |budget| budget.reviewed(reviewing.elapsed()));
        let materialize_to = |report: &Path, context: &Path, summary: &Path, comments: &Path| {
            frozen::materialize(
                inputs,
                report,
                context,
                &chunk.files,
                head_sha,
                &chunk_diff,
                summary,
                comments,
            )
        };
        let (lensed, usage) = lens::run(
            inputs,
            cancel,
            &mut chunk_progress,
            &SpawnPaths {
                config: config_path,
                prompt: &directory.join("prompt.md"),
                transcript: &directory.join("transcript.json"),
                stdout: &directory.join("claude.stdout"),
                stderr: &directory.join("claude.stderr"),
                progress_phase: "chunks",
                sharded_progress: true,
            },
            &directory.join("context"),
            Some(chunk.id as i64),
            materialize_to,
        )?;
        lens_usage = usage;
        let evidence_warnings = if lensed {
            materialize_to(
                &directory.join("report.json"),
                &directory.join("context"),
                &summary,
                &comments,
            )?
        } else {
            evidence_warnings
        };
        fs::copy(directory.join("report.json"), &summary).map_err(|error| error.to_string())?;
        let summary_text = fs::read_to_string(&summary).unwrap_or_default().trim().to_string();
        if summary_text.is_empty() {
            return Err(format!("Chunk {} did not produce a summary", chunk.id));
        }
        let comments_text = fs::read_to_string(&comments).unwrap_or_default();
        let publication = crate::review::evidence::publication_diff(&chunk_context, &chunk_diff)?;
        let comments_json = Some(require_valid_comments(&comments_text, head_sha, &publication)?);
        Ok((summary_text, comments_json, evidence_warnings))
    });
    if inputs.frozen.is_some() {
        if let Some(attempt) = attempts.last_mut() {
            attempt.usage = parse_transcript_usage(&chunk_dir.join("transcript.json"));
            attempt.finished_at = now_millis();
        }
    }
    let status = match &artifacts {
        Ok(_) => "done",
        Err(error) if error == "cancelled" => "cancelled",
        Err(_) => "failed",
    };
    let usage = recovery::record_attempts(inputs, chunk, &attempts, status, &cache_key, potential_cache_hit);
    let usage = if lens_usage.by_model.is_empty() {
        usage
    } else {
        sum_usage(vec![usage, lens_usage])
    };
    let (summary_text, comments_json, evidence_warnings) = artifacts?;
    let metrics = attempts
        .last()
        .ok_or("Incomplete review: no chunk execution recorded")?
        .metrics;
    if let Some(db) = (attempts.len() == 1).then(|| inputs.open_db().ok()).flatten() {
        let _ = db.upsert_review_chunk_cache(&ReviewChunkCacheInput {
            cache_key,
            repo_id: inputs.repo_id,
            pr_id: inputs.pr_id,
            run_id: inputs.run_id,
            chunk_id: chunk.id as i64,
            head_sha: head_sha.to_string(),
            prior_sha: incremental_base.map(str::to_string),
            model: inputs.model.clone(),
            prompt_version: REVIEW_PROMPT_VERSION.to_string(),
            skill_hash,
            index_spec_version: INDEX_SPEC_VERSION,
            chunk_json,
            summary: summary_text.clone(),
            comments_json: comments_json.clone(),
            usage_json: serde_json::to_string(&serde_json::json!({
                "inputTokens": usage.input_tokens,
                "outputTokens": usage.output_tokens,
                "cacheReadTokens": usage.cache_read_tokens,
                "cacheCreationTokens": usage.cache_creation_tokens,
                "model": usage.model.clone(),
            }))
            .unwrap_or_default(),
            prompt_bytes: metrics.prompt_bytes,
            diff_bytes: metrics.diff_bytes,
            context_bytes: metrics.context_bytes,
            now: now_millis(),
        });
    }
    Ok(ChunkResult {
        cached: false,
        chunk: chunk.clone(),
        summary: summary_text,
        comments_json,
        evidence_warnings,
        usage,
    })
}

pub(crate) fn frozen_omittable_paths(inputs: &ReviewInputs, changed: &[String]) -> std::collections::BTreeSet<String> {
    frozen_paths_where(inputs, changed, false)
}

/// Changed paths the archive left out as generated, binary or oversized: skipped without review.
/// Private paths and offline submodules or symlinks are not among them, so a pull request that
/// only changes those still fails explicitly.
pub(crate) fn frozen_unreviewable_paths(
    inputs: &ReviewInputs,
    changed: &[String],
) -> std::collections::BTreeSet<String> {
    frozen_paths_where(inputs, changed, true)
}

fn frozen_paths_where(
    inputs: &ReviewInputs,
    changed: &[String],
    exclude_private: bool,
) -> std::collections::BTreeSet<String> {
    let Some(frozen) = &inputs.frozen else {
        return std::collections::BTreeSet::new();
    };
    let head = frozen.archive.scope().head_sha.as_str();
    let entries = frozen.archive.entries();
    let private = |entry: &crate::review_archive::Entry| {
        exclude_private
            && matches!(
                entry.exclusion,
                Some(
                    crate::review_archive::Exclusion::PrivateConfiguration
                        | crate::review_archive::Exclusion::PrivatePath
                        | crate::review_archive::Exclusion::NonRegular
                )
            )
    };
    let omittable = |entry: &crate::review_archive::Entry| !private(entry) && omittable_entry(inputs, entry);
    changed
        .iter()
        .filter(|path| {
            let path = path.as_str();
            match entries
                .iter()
                .find(|entry| entry.source.revision == head && entry.source.path == path)
            {
                Some(entry) => omittable(entry),
                None => entries
                    .iter()
                    .any(|entry| entry.source.path == path && omittable(entry)),
            }
        })
        .cloned()
        .collect()
}

fn omittable_entry(inputs: &ReviewInputs, entry: &crate::review_archive::Entry) -> bool {
    // Offline, a submodule or symlink has no content to review: nothing pins what it points at.
    (inputs.offline && matches!(entry.exclusion, Some(crate::review_archive::Exclusion::NonRegular)))
        || matches!(
            entry.exclusion,
            Some(
                crate::review_archive::Exclusion::PrivateConfiguration
                    | crate::review_archive::Exclusion::PrivatePath
                    | crate::review_archive::Exclusion::Generated
                    | crate::review_archive::Exclusion::BinaryPath
                    | crate::review_archive::Exclusion::Binary
                    | crate::review_archive::Exclusion::NonUtf8
                    | crate::review_archive::Exclusion::Oversized
            )
        )
}

pub(crate) fn build_review_map(
    inputs: &ReviewInputs,
    plan: &ReviewContextPlan,
    head_sha: &str,
    changed: &[String],
    diff: &str,
) -> ReviewMap {
    let (added, removed) = diff_churn(diff);
    let omittable_paths = frozen_omittable_paths(inputs, changed);
    let omitted_noise_files: Vec<String> = changed
        .iter()
        .filter(|path| omittable_paths.contains(path.as_str()) || (is_noise_path(path) && !is_risk_bearing_path(path)))
        .cloned()
        .collect();
    // When every file is noise, the noise is reviewed, but never a generated, binary or oversized
    // path the archive excluded; a private one stays so its preflight fails explicitly.
    let reviewable_files = if omitted_noise_files.len() == changed.len() {
        let unreviewable = frozen_unreviewable_paths(inputs, changed);
        changed
            .iter()
            .filter(|path| !unreviewable.contains(path.as_str()))
            .cloned()
            .collect()
    } else {
        changed
            .iter()
            .filter(|path| !omitted_noise_files.contains(path))
            .cloned()
            .collect()
    };
    let mut risk_triggers = Vec::new();
    let mut data_lifecycle_triggers = Vec::new();
    let mut public_contract_triggers = Vec::new();
    // A file the archive excluded cannot be read, so it never becomes a trigger to check.
    for path in changed.iter().filter(|path| !omittable_paths.contains(path.as_str())) {
        if is_config_path(path) {
            risk_triggers.push(format!("runtime/config: {path}"));
        }
        if is_risky_path(path) {
            risk_triggers.push(format!("security/auth/destructive path: {path}"));
        }
        if path.to_ascii_lowercase().contains("migration") || path.to_ascii_lowercase().contains("schema") {
            data_lifecycle_triggers.push(path.clone());
        }
        if looks_public_contract(path) {
            public_contract_triggers.push(path.clone());
        }
    }
    ReviewMap {
        dependency_edges: plan.dependency_edges.clone(),
        pr_number: inputs.number,
        title: inputs.pr_title.clone(),
        description: inputs.pr_body.clone(),
        head_sha: head_sha.to_string(),
        base_ref: inputs.base_ref.clone(),
        changed_files: changed.to_vec(),
        reviewable_files,
        omitted_noise_files,
        diff_stats: DiffStats {
            added: added.max(0) as usize,
            removed: removed.max(0) as usize,
            files: changed.len(),
        },
        categories: changed
            .iter()
            .map(|path| FileCategory {
                path: path.clone(),
                category: categorize_path(path).to_string(),
            })
            .collect(),
        packages: plan
            .packages
            .iter()
            .map(|package| format!("{} ({}, {})", package.package_path, package.name, package.kind))
            .collect(),
        dependent_packages: plan.dependent_packages.clone(),
        related_files: plan.related_files.clone(),
        convention_files: plan.convention_files.clone(),
        http_refs: extract_http_refs(diff),
        risk_triggers,
        data_lifecycle_triggers,
        public_contract_triggers,
        test_relationship_hints: test_relationship_hints(changed),
    }
}

pub(crate) fn write_review_map(context_dir: &Path, review_map: &ReviewMap) -> Result<(), String> {
    fs::write(
        context_dir.join("review_map.json"),
        serde_json::to_string_pretty(review_map).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    fs::write(context_dir.join("REVIEW_MAP.md"), render_review_map_md(review_map)).map_err(|error| error.to_string())
}

pub(crate) fn render_review_map_md(map: &ReviewMap) -> String {
    let mut out = format!(
        "# Review map\n\n- PR: #{}\n- Title: {}\n- Head SHA: {}\n- Base ref: {}\n- Diff stats: {} added / {} removed / {} file(s)\n\n",
        map.pr_number, map.title, map.head_sha, map.base_ref, map.diff_stats.added, map.diff_stats.removed, map.diff_stats.files
    );
    out.push_str("## Reviewable files\n");
    for path in &map.reviewable_files {
        out.push_str(&format!("- {path}\n"));
    }
    out.push_str("\n## Omitted noise files\n");
    for path in &map.omitted_noise_files {
        out.push_str(&format!("- {path}\n"));
    }
    out.push_str("\n## Categories\n");
    for cat in &map.categories {
        out.push_str(&format!("- {} — {}\n", cat.path, cat.category));
    }
    out.push_str("\n## Related files\n");
    for file in &map.related_files {
        out.push_str(&format!("- {} — {}\n", file.path, file.reason));
    }
    out.push_str("\n## HTTP/API references\n");
    for r in &map.http_refs {
        out.push_str(&format!("- {} {} in {} ({})\n", r.method, r.route, r.path, r.side));
    }
    out.push_str("\n## Risk triggers\n");
    for trigger in &map.risk_triggers {
        out.push_str(&format!("- {trigger}\n"));
    }
    out.push_str("\n## Data lifecycle triggers\n");
    for trigger in &map.data_lifecycle_triggers {
        out.push_str(&format!("- {trigger}\n"));
    }
    out.push_str("\n## Public contract triggers\n");
    for trigger in &map.public_contract_triggers {
        out.push_str(&format!("- {trigger}\n"));
    }
    out.push_str("\n## Test relationship hints\n");
    for hint in &map.test_relationship_hints {
        out.push_str(&format!("- {hint}\n"));
    }
    out
}

pub(crate) fn chunk_review_map(map: &ReviewMap, chunk: &ReviewChunk) -> ReviewMap {
    let available = chunk
        .files
        .iter()
        .chain(&chunk.related_files)
        .chain(&chunk.global_context_files)
        .collect::<std::collections::BTreeSet<_>>();
    let mut scoped = map.clone();
    scoped.description =
        "See PR.md for the MR description and RELATED_ISSUES.md (when present) for linked GitLab issue intent.".into();
    scoped
        .dependency_edges
        .retain(|(from, to)| available.contains(from) && available.contains(to));
    scoped
        .related_files
        .retain(|file| chunk.related_files.contains(&file.path));
    scoped
        .convention_files
        .retain(|path| !chunk.omitted_convention_files.contains(path));
    scoped.http_refs.retain(|reference| available.contains(&reference.path));
    scoped.categories.retain(|category| available.contains(&category.path));
    scoped.risk_triggers.retain(|trigger| {
        trigger
            .split_once(": ")
            .is_some_and(|(_, path)| available.iter().any(|file| file.as_str() == path))
    });
    scoped.data_lifecycle_triggers.retain(|path| available.contains(path));
    scoped.public_contract_triggers.retain(|path| available.contains(path));
    scoped.test_relationship_hints.retain(|hint| {
        hint.split_once(" may test ").is_some_and(|(test, source)| {
            available.iter().any(|path| path.as_str() == test) && available.iter().any(|path| path.as_str() == source)
        })
    });
    scoped
}

pub(crate) fn chunk_diff_bytes(map: &ReviewMap, diff: &str) -> (BTreeMap<String, usize>, usize) {
    let mut sizes = BTreeMap::new();
    let mut shared = 0;
    let known_paths = map
        .changed_files
        .iter()
        .chain(&map.reviewable_files)
        .collect::<std::collections::BTreeSet<_>>();
    for section in diff
        .split_inclusive('\n')
        .fold(Vec::<String>::new(), |mut sections, line| {
            if line.starts_with("diff --git ") || sections.is_empty() {
                sections.push(String::new());
            }
            sections.last_mut().unwrap().push_str(line);
            sections
        })
    {
        let header = section.lines().next().unwrap_or_default();
        let paths = known_paths
            .iter()
            .copied()
            .filter(|path| header.ends_with(&format!(" b/{path}")))
            .collect::<Vec<_>>();
        if paths.is_empty() {
            shared += section.len();
        }
        for path in paths.into_iter().filter(|path| map.reviewable_files.contains(path)) {
            *sizes.entry(path.clone()).or_insert(0) += section.len();
        }
    }
    (sizes, shared)
}

pub(crate) struct ChunkContextBudget {
    pub(crate) metadata: usize,
    pub(crate) shared: usize,
    pub(crate) files: usize,
    pub(crate) diff: usize,
    pub(crate) prompt: usize,
}

impl ChunkContextBudget {
    pub(crate) fn evidence(&self) -> usize {
        self.metadata + self.shared + self.files + self.diff
    }
    pub(crate) fn total(&self) -> usize {
        self.evidence() + self.prompt
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        crate::review::policy::validate_prepared_inputs(self.evidence(), self.prompt)
    }
}

#[cfg(test)]
pub(crate) fn chunk_context_bytes(
    map: &ReviewMap,
    context: &Path,
    chunk: &ReviewChunk,
    patches: &(BTreeMap<String, usize>, usize),
    prompt_bytes: usize,
) -> Result<usize, String> {
    chunk_context_budget(map, context, chunk, patches, prompt_bytes).map(|budget| budget.total())
}

pub(crate) fn chunk_convention_paths(context: &Path, chunk: &ReviewChunk) -> Vec<PathBuf> {
    let root = context.join("conventions");
    collect_files(&root)
        .into_iter()
        .filter(|path| {
            !chunk
                .omitted_convention_files
                .iter()
                .any(|omitted| *path == root.join(omitted))
        })
        .collect()
}

pub(crate) fn copy_chunk_conventions(context: &Path, destination: &Path, chunk: &ReviewChunk) -> Result<(), String> {
    for path in chunk_convention_paths(context, chunk) {
        let relative = path.strip_prefix(context).map_err(|error| error.to_string())?;
        let target = destination.join(relative);
        fs::create_dir_all(target.parent().ok_or("Missing convention directory")?)
            .map_err(|error| error.to_string())?;
        fs::copy(path, target).map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn chunk_context_budget(
    map: &ReviewMap,
    context: &Path,
    chunk: &ReviewChunk,
    patches: &(BTreeMap<String, usize>, usize),
    prompt_bytes: usize,
) -> Result<ChunkContextBudget, String> {
    let scoped = chunk_review_map(map, chunk);
    let protocol_bytes = if context.join("FROZEN.json").is_file() {
        let mut stage: crate::review_archive::Stage =
            serde_json::from_slice(&fs::read(context.join("STAGE.json")).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        stage.files = chunk.files.clone();
        stage.context_hashes.retain(|path, _| {
            if let Some(path) = path.strip_prefix("context/changed/") {
                return chunk.files.iter().any(|file| file == path);
            }
            if let Some(path) = path.strip_prefix("context/related/") {
                return chunk.related_files.iter().any(|file| file == path);
            }
            if let Some(path) = path.strip_prefix("context/conventions/") {
                return !chunk.omitted_convention_files.iter().any(|file| file == path);
            }
            path.starts_with("context/skill/")
                || matches!(
                    path.as_str(),
                    "context/PR.md"
                        | "context/RELATED_ISSUES.md"
                        | "context/PENDING_REVIEW.md"
                        | "context/FROZEN.json"
                        | "context/PR_DIFF.patch"
                        | "context/PUBLICATION_DIFF.patch"
                        | "context/review_map.json"
                        | "context/REVIEW_MAP.md"
                        | "context/CONTEXT.md"
                )
        });
        for path in &chunk.global_context_files {
            if context.join("changed").join(path).is_file() {
                stage
                    .context_hashes
                    .insert(format!("context/global/{path}"), "0".repeat(64));
            }
        }
        stage
            .context_hashes
            .insert("context/CHUNK_SCOPE".into(), "0".repeat(64));
        file_bytes(&context.join("FROZEN.json")) as usize
            + serde_json::to_vec(&stage).map_err(|error| error.to_string())?.len()
    } else {
        recovery::NOTE.len()
    };
    let metadata = serde_json::to_vec_pretty(&scoped)
        .map_err(|error| error.to_string())?
        .len()
        + render_review_map_md(&scoped).len()
        + render_chunk_context(map, chunk).len()
        + CHUNK_SCOPE_NOTE.len()
        + protocol_bytes;
    let shared = ["PR.md", "RELATED_ISSUES.md", "PENDING_REVIEW.md"]
        .iter()
        .map(|name| file_bytes(&context.join(name)) as usize)
        .sum::<usize>()
        + dir_bytes(&context.join("skill")) as usize
        + chunk_convention_paths(context, chunk)
            .iter()
            .map(|path| file_bytes(path) as usize)
            .sum::<usize>();
    let files = [
        ("changed", &chunk.files),
        ("related", &chunk.related_files),
        ("changed", &chunk.global_context_files),
    ]
    .iter()
    .map(|(area, paths)| {
        paths
            .iter()
            .map(|path| file_bytes(&context.join(area).join(path)) as usize)
            .sum::<usize>()
    })
    .sum::<usize>();
    let publication_bytes = if context.join("PUBLICATION_DIFF.patch").exists() {
        integration::scoped_diff(
            &crate::review::evidence::publication_diff(context, "")?,
            &chunk.files,
            &map.changed_files,
        )
        .len()
    } else {
        0
    };
    let diff = patches.1
        + chunk
            .files
            .iter()
            .map(|path| patches.0.get(path).copied().unwrap_or(0))
            .sum::<usize>()
        + publication_bytes;
    Ok(ChunkContextBudget {
        metadata,
        shared,
        files,
        diff,
        prompt: prompt_bytes,
    })
}

pub(crate) fn build_graph_chunks(
    map: &ReviewMap,
    context: &Path,
    chunk_size: usize,
    prompt_bytes: &dyn Fn(&ReviewChunk) -> usize,
) -> Result<Vec<ReviewChunk>, String> {
    let patches = chunk_diff_bytes(
        map,
        &fs::read_to_string(context.join("PR_DIFF.patch")).map_err(|error| error.to_string())?,
    );
    let mut neighbours: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (from, to) in &map.dependency_edges {
        neighbours.entry(from.clone()).or_default().push(to.clone());
        neighbours.entry(to.clone()).or_default().push(from.clone());
    }
    for related in neighbours.values_mut() {
        related.sort();
        related.dedup();
    }
    let mut remaining = map
        .reviewable_files
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let mut chunks = Vec::new();
    while let Some(seed) = remaining.pop_first() {
        let id = chunks.len() + 1;
        let make_chunk = |files: Vec<String>| {
            let risks = files
                .iter()
                .filter(|path| is_risk_bearing_path(path))
                .cloned()
                .collect::<Vec<_>>();
            ReviewChunk {
                id,
                cross_chunk_group_ids: chunk_group_ids(id, &files, &risks),
                files,
                global_context_files: vec![],
                related_files: vec![],
                omitted_convention_files: vec![],
                risk_triggers: risks,
            }
        };
        let mut chunk = make_chunk(vec![seed]);
        let budget = chunk_context_budget(map, context, &chunk, &patches, prompt_bytes(&chunk))?;
        budget.validate().map_err(|error| {
            format!(
                "{error} Chunk {}: source/evidence {}, diff {}, metadata {}, shared instructions/context {} bytes.",
                chunk.id, budget.files, budget.diff, budget.metadata, budget.shared
            )
        })?;
        while chunk.files.len() < chunk_size {
            let mut candidates = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            let mut add_candidate = |path: &String| {
                if remaining.contains(path) && seen.insert(path.clone()) {
                    candidates.push(path.clone());
                }
            };
            for path in chunk
                .files
                .iter()
                .flat_map(|path| neighbours.get(path).into_iter().flatten())
            {
                add_candidate(path);
            }
            for path in remaining
                .iter()
                .filter(|path| chunk.files.iter().any(|file| test_source_pair(file, path)))
            {
                add_candidate(path);
            }
            for path in &remaining {
                add_candidate(path);
            }
            let mut selected = None;
            let mut soft_fallback = None;
            for next in candidates {
                let mut candidate = make_chunk(chunk.files.iter().cloned().chain([next.clone()]).collect());
                candidate.omitted_convention_files = chunk.omitted_convention_files.clone();
                let budget = chunk_context_budget(map, context, &candidate, &patches, prompt_bytes(&candidate))?;
                if budget.validate().is_err() {
                    continue;
                }
                if budget.total() <= crate::review::policy::TARGET_CONTEXT_BYTES {
                    selected = Some((next, candidate));
                    break;
                }
                if budget.prompt < crate::review::policy::TARGET_CONTEXT_BYTES
                    && budget.files.saturating_add(budget.diff) <= crate::review::policy::TARGET_CONTEXT_BYTES
                    && soft_fallback.is_none()
                {
                    soft_fallback = Some((next, candidate));
                }
            }
            let Some((next, candidate)) = selected.or(soft_fallback) else {
                break;
            };
            remaining.remove(&next);
            chunk = candidate;
        }
        let direct = chunk
            .files
            .iter()
            .flat_map(|path| neighbours.get(path).into_iter().flatten())
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut seen = std::collections::BTreeSet::new();
        let related = direct
            .iter()
            .cloned()
            .chain(select_related_files(map, &chunk.files))
            .filter(|path| !chunk.files.contains(path) && seen.insert(path.clone()))
            .collect::<Vec<_>>();
        for path in related {
            let is_direct = direct.contains(&path);
            let global = map.reviewable_files.contains(&path) && context.join("changed").join(&path).is_file();
            if !global && !context.join("related").join(&path).is_file() {
                continue;
            }
            if global {
                chunk.global_context_files.push(path);
            } else {
                chunk.related_files.push(path);
            }
            let budget = chunk_context_budget(map, context, &chunk, &patches, prompt_bytes(&chunk))?;
            if budget.validate().is_err()
                || (!is_direct && budget.total() > crate::review::policy::TARGET_CONTEXT_BYTES)
            {
                if global {
                    chunk.global_context_files.pop();
                } else {
                    chunk.related_files.pop();
                }
            }
        }
        chunks.push(chunk);
    }
    Ok(chunks)
}

#[cfg(test)]
pub(crate) fn build_review_chunks(map: &ReviewMap, chunk_size: usize) -> Vec<ReviewChunk> {
    let mut remaining = map.reviewable_files.clone();
    remaining.sort_by(|a, b| chunk_sort_key(a).cmp(&chunk_sort_key(b)).then_with(|| a.cmp(b)));
    let bridge_files: Vec<String> = remaining
        .iter()
        .filter(|path| is_bridge_context_path(path))
        .cloned()
        .collect();
    let mut chunks = Vec::new();
    while !remaining.is_empty() {
        let seed = remaining.remove(0);
        let mut files = vec![seed];
        while files.len() < chunk_size {
            let Some((index, score)) = remaining
                .iter()
                .enumerate()
                .map(|(index, candidate)| {
                    let score = files
                        .iter()
                        .map(|file| chunk_affinity(file, candidate))
                        .max()
                        .unwrap_or(0);
                    (index, score)
                })
                .max_by_key(|(_, score)| *score)
            else {
                break;
            };
            if score == 0 {
                break;
            }
            files.push(remaining.remove(index));
        }
        while !remaining.is_empty() && files.len() < chunk_size {
            files.push(remaining.remove(0));
        }
        let risks: Vec<String> = files
            .iter()
            .filter(|path| is_risk_bearing_path(path))
            .map(|path| format!("{}: {}", categorize_path(path), path))
            .collect();
        let group_ids = chunk_group_ids(chunks.len() + 1, &files, &risks);
        let mut global_context_files = bridge_files
            .iter()
            .filter(|path| !files.contains(path))
            .cloned()
            .collect::<Vec<_>>();
        global_context_files.sort();
        global_context_files.dedup();
        chunks.push(ReviewChunk {
            id: chunks.len() + 1,
            related_files: select_related_files(map, &files),
            global_context_files,
            omitted_convention_files: vec![],
            files,
            risk_triggers: risks,
            cross_chunk_group_ids: group_ids,
        });
    }
    chunks
}

#[cfg(test)]
pub(crate) fn chunk_sort_key(path: &str) -> String {
    format!("{}:{}:{}", package_key(path), directory_key(path), path_stem_key(path))
}

pub(crate) fn chunk_affinity(a: &str, b: &str) -> i32 {
    if a == b {
        return 1_000;
    }
    let mut score = 0;
    if package_key(a) == package_key(b) {
        score += 120;
    }
    if directory_key(a) == directory_key(b) {
        score += 90;
    }
    if test_source_pair(a, b) {
        score += 100;
    }
    let a_stem = path_stem_key(a);
    let b_stem = path_stem_key(b);
    if related_by_name(&a_stem, b) || related_by_name(&b_stem, a) {
        score += 80;
    }
    score += 18 * shared_topic_tokens(a, b).min(5) as i32;
    if risk_group_match(a, b) {
        score += 50;
    }
    if categorize_path(a) == categorize_path(b) {
        score += 25;
    }
    score
}

pub(crate) fn select_related_files(map: &ReviewMap, files: &[String]) -> Vec<String> {
    let mut scored = map
        .related_files
        .iter()
        .map(|file| {
            let score = files
                .iter()
                .map(|changed| chunk_affinity(changed, &file.path))
                .max()
                .unwrap_or(0);
            (score, file.path.clone())
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(MAX_RELATED_CONTEXT)
        .map(|(_, path)| path)
        .collect()
}

pub(crate) fn chunk_group_ids(index: usize, files: &[String], risks: &[String]) -> Vec<String> {
    let mut groups = Vec::new();
    if !risks.is_empty() {
        groups.push(format!("risk-{index}"));
    }
    let package = package_key(files.first().map(String::as_str).unwrap_or_default());
    if files.iter().all(|path| package_key(path) == package) && !package.is_empty() {
        groups.push(format!("pkg-{package}"));
    }
    if files.iter().any(|path| is_test_path(path)) && files.iter().any(|path| !is_test_path(path)) {
        groups.push(format!("test-source-{index}"));
    }
    groups
}

pub(crate) fn render_chunk_context(_map: &ReviewMap, chunk: &ReviewChunk) -> String {
    let mut context = format!(
        "# Chunk {} context\n\n## Files\n{}\n## Bridge/global changed files\n{}\n## Related snapshot files\n{}\n## Risk triggers\n{}\n## Cross chunk groups\n{}\n\nUse REVIEW_MAP.md for global orientation. Inline findings must anchor only to this chunk's changed files. Bridge/global files are evidence for cross-file reasoning, not inline anchors for this chunk. Related/global evidence is selected by relevance; files not listed were not supplied. The 160,000-byte selection target is not a model context limit: required files and direct dependencies may exceed it. Read files progressively and target relevant diff hunks rather than loading this entire snapshot. Missing context is not proof of missing implementation. Report incomplete coverage when omitted evidence is required.",
        chunk.id,
        chunk.files.iter().map(|f| format!("- {f}\n")).collect::<String>(),
        chunk.global_context_files.iter().map(|f| format!("- {f}\n")).collect::<String>(),
        chunk.related_files.iter().map(|f| format!("- {f}\n")).collect::<String>(),
        chunk.risk_triggers.iter().map(|f| format!("- {f}\n")).collect::<String>(),
        chunk.cross_chunk_group_ids.iter().map(|f| format!("- {f}\n")).collect::<String>(),
    );
    if !chunk.omitted_convention_files.is_empty() {
        context.push_str(&format!("\n\n## Omitted general documentation (byte budget)\n{}\nThese base-version documents remain in the full review context for the final pass. Do not infer missing implementation from their absence here. Report missing evidence if they are needed.\n", chunk.omitted_convention_files.iter().map(|path| format!("- conventions/{path}\n")).collect::<String>()));
    }
    context
}

pub(crate) fn render_chunk_prompt(
    inputs: &ReviewInputs,
    context_dir: &Path,
    summary_path: &Path,
    head_sha: &str,
    incremental_base: Option<&str>,
    chunk: &ReviewChunk,
) -> String {
    format!(
        "Review range: {}. {}\n{}",
        review_range(incremental_base, &inputs.base_ref, head_sha),
        if incremental_base.is_some() {
            "Só mudanças novas do MR filho desde a revisão anterior."
        } else {
            "Diff completo do MR contra a base imediata, não o branch padrão."
        },
        frozen::prompt(
            inputs,
            context_dir,
            &chunk.files,
            &summary_path.with_file_name("report.json")
        )
    )
}

pub(crate) fn render_final_prompt(
    inputs: &ReviewInputs,
    context_dir: &Path,
    merge_dir: &Path,
    map: &ReviewMap,
) -> String {
    let prompt = frozen::prompt(
        inputs,
        context_dir,
        &map.reviewable_files,
        &merge_dir.join("report.json"),
    );
    let example = frozen::decision_example(inputs);
    format!("{prompt}\nThis is independent verification. Write every title, detail and action in Brazilian Portuguese (português brasileiro); keep paths, identifiers, code quotes and JSON field names unchanged. Read {}/chunk_findings.json and chunk_summaries.md. Verify candidates against targeted snapshot reads, including cross-file risks even when chunks found nothing. Reject refuted hypotheses, do not merely carry them forward. Include findingDecisions alongside report in your final JSON response. Include exactly one decision per candidateId. decision is keep, discard, or needs_context. To keep a candidate exactly as its chunk validated it, use keep with \"unchanged\": true and no commentIndex: its validated comment is posted as is, so do not write it again in report.json findings. Write a finding in report.json only for a candidate you change (line, wording, impact or severity) or merge, and for a new problem; keep then requires commentIndex into report.json findings. discard requires concrete counter-evidence and a counterEvidence array using the same reference schema as report.json, including the exact frozen revision for code. Do not cite chunk reports or candidate summaries as counter-evidence: they are proposals, not source evidence. Replace every example path, line and quote with inspected snapshot bytes; retain the supplied contractVersion and snapshotId for frozen output. For conventions, include source:convention and the supplied convention revision; only external/*, PR.md, RELATED_ISSUES.md and SCOPE_CONTEXT.md omit revision. For duplicates, write the merged finding once and map both candidates with keep to the same commentIndex; candidates in the same file with the same root cause and the same fix are duplicates too, kept as one comment at the place that matters most whose detail names the other lines and each distinct impact; candidates in different files, or needing different fixes, stay separate comments. Missing decisions/evidence make the review incomplete. Do not write formatted comments or summary.\nCandidate decision JSON example:\n{example}\nEnd candidate decision example.\n", merge_dir.display())
}
