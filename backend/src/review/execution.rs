use super::*;

/// Run a full review off the main thread: build the index-backed context pack,
/// spawn Claude against the repo skill, supervise it (cancellable, timeout,
/// process-group kill), and parse its two artifacts. `progress(phase, tail)` is
/// called periodically so the caller can persist/emit live state.
pub fn execute(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
) -> Result<ReviewOutput, String> {
    let run_dir = make_run_dir(inputs.pr_id)?;
    harness_log(
        &run_dir,
        &format!(
            "start pr_id={} run_id={} repo={} number={} model={}",
            inputs.pr_id, inputs.run_id, inputs.full_name, inputs.number, inputs.model
        ),
    );
    let result = run_inner(inputs, cancel, progress, &run_dir).and_then(|mut output| {
        if let Some(expected) = &inputs.expected_scope {
            let live = gitlab::live_pr_state_cancellable(&inputs.full_name, inputs.number, cancel)?;
            if live.state != "OPEN"
                || !crate::review::scope::equivalent(&inputs.repo_path, expected, &live.review_scope(), cancel)
            {
                return Err(
                    "Escopo base/head do MR mudou durante a revisão. Atualize o MR e rode uma revisão completa.".into(),
                );
            }
        }
        let db = inputs.open_db()?;
        output.body = kept_review_body(&db, &output.body);
        let source_audit = db
            .load_finding_audit(inputs.run_id)?
            .ok_or("Finding evidence snapshot is unavailable")?;
        let raw_final = fs::read_to_string(run_dir.join("comments.json")).ok();
        let decisions = fs::read_to_string(run_dir.join("merge/finding_decisions.json")).ok();
        let mut audit = crate::review::findings::finish_audit(
            source_audit,
            raw_final.as_deref().or(output.comments_json.as_deref()),
            output.comments_json.as_deref(),
            &output.body,
            decisions.as_deref(),
            "",
        );
        audit.evidence_warnings = Some(output.evidence_warnings.clone());
        apply_evidence_warning_risk(&mut output.risk, &output.evidence_warnings);
        if output.no_new_changes {
            for candidate in &mut audit.candidates {
                candidate.ai_decision = "unmapped".to_string();
                candidate.ai_reason = None;
                candidate.origin = "inherited".to_string();
            }
        }
        db.store_finding_audit(inputs.run_id, &audit)?;
        Ok(output)
    });
    match &result {
        Ok(output) => harness_log(
            &run_dir,
            &format!(
                "result ok summary_bytes={} comments_bytes={} input_tokens={} output_tokens={}",
                output.body.len(),
                output.comments_json.as_ref().map(|raw| raw.len()).unwrap_or(0),
                output.usage.input_tokens,
                output.usage.output_tokens
            ),
        ),
        Err(error) => harness_log(&run_dir, &format!("result error {error}")),
    }
    harness_log(
        &run_dir,
        "cleanup pending: temp run dir will be removed after optional debug copy",
    );
    if let Ok(db) = inputs.open_db() {
        if result.is_err() {
            let _ = db.sync_review_stage_usage(inputs.run_id);
        }
        let _ = db.clear_finding_evidence(inputs.run_id);
    }
    if let Some(db_handle) = &inputs.db_handle {
        let _ = db_handle.open().and_then(|db| db.drop_snapshot_index(inputs.run_id));
    }
    let debug_dir = save_review_debug_artifacts(inputs, &run_dir, &result);
    if let Some(debug_dir) = &debug_dir {
        let _ = fs::write(
            debug_dir.join("debug-copy.txt"),
            format!("Copied from temp run dir {} before cleanup.\n", run_dir.display()),
        );
    }
    // Always wipe the per-run dir (prompt/config/context/transcript).
    let cleanup_text = match fs::remove_dir_all(&run_dir) {
        Ok(_) => "cleanup ok: temp run dir removed\n".to_string(),
        Err(error) => format!("cleanup failed: {error}\n"),
    };
    if let Some(debug_dir) = &debug_dir {
        let _ = fs::write(debug_dir.join("cleanup-result.txt"), cleanup_text);
    }
    result
}

pub(crate) fn run_inner(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    run_dir: &Path,
) -> Result<ReviewOutput, String> {
    progress(&phase_json("context", "running"), "Buscando branch do MR via git...");
    check_cancel(cancel)?;

    // Fetch the PR head into local git objects via the git transport (github.com),
    // never the REST API. Working tree/branch untouched.
    let head_sha = match (&inputs.fixed_head_sha, inputs.offline) {
        (Some(head), true) => resolve_rev_sha(&inputs.repo_path, &format!("{head}^{{commit}}"), cancel)
            .filter(|resolved| resolved == head)
            .ok_or("Pinned head commit is missing from the local clone")?,
        (None, true) => return Err("An offline review needs a pinned head commit".into()),
        (fixed, false) => {
            let fetched_head_sha = fetch_pr_head(&inputs.repo_path, inputs.number, cancel)?;
            fixed.clone().unwrap_or(fetched_head_sha)
        }
    };

    if inputs
        .expected_scope
        .as_ref()
        .is_some_and(|scope| scope.head_sha != head_sha)
    {
        return Err("Head do MR mudou antes da preparação. Atualize o MR e tente de novo.".into());
    }

    // Changed files + diff. For an incremental re-review we want ONLY the author's
    // new PR changes since the last review — NOT a main catch-up merged into the
    // branch. So we intersect the raw delta (`old..head`) with the PR's net diff vs
    // base (`base...head`): files that came in purely from merging main are in the
    // delta but NOT in the PR diff (they're in base now), so they're excluded.
    progress(
        &phase_json("context", "running"),
        "Calculando arquivos alterados (git local)...",
    );
    // ALWAYS diff against the FRESH remote base (`origin/<base>`), never a bare local
    // branch name. The PR's base may be ANOTHER PR's branch (a stacked PR) whose local
    // branch is absent or stale — diffing that would pull the PARENT PR's changes into
    // this review (comments would land on files not in this PR's real diff). `origin/<base>`,
    // refreshed first, matches GitHub's PR diff exactly (stacked or not).
    let base_rev =
        if inputs.fixed_head_sha.is_some() && inputs.expected_scope.is_none() && looks_like_sha(&inputs.base_ref) {
            resolve_rev_sha(&inputs.repo_path, &inputs.base_ref, cancel).ok_or("Historical base SHA is unavailable")?
        } else {
            resolve_base_rev(&inputs.repo_path, &inputs.base_ref, cancel)?
        };
    let current_scope = crate::review::scope::ReviewScope {
        head_sha: head_sha.clone(),
        base_ref: inputs.base_ref.clone(),
        base_sha: base_rev.clone(),
    };
    if inputs
        .expected_scope
        .as_ref()
        .is_some_and(|expected| !crate::review::scope::equivalent(&inputs.repo_path, expected, &current_scope, cancel))
    {
        return Err(
            "Escopo base/head do MR mudou ao buscar o snapshot. Atualize o MR e rode uma revisão completa.".into(),
        );
    }
    let reviewed_base_sha = Some(base_rev.clone());
    let merge_base_sha = crate::review::scope::merge_base(&inputs.repo_path, &base_rev, &head_sha, cancel)?;
    let historical_prior_head = inputs
        .prior_scope
        .as_ref()
        .filter(|prior| inputs.last_reviewed_sha.as_deref() == Some(prior.head_sha.as_str()))
        .filter(|prior| {
            crate::review::scope::can_review_incrementally(&inputs.repo_path, prior, &current_scope, cancel)
        })
        .map(|prior| prior.head_sha.as_str());
    let full_pr_range = format!("{base_rev}...{head_sha}");
    let full_pr_changed = changed_files_local(&inputs.repo_path, &full_pr_range, cancel)?;
    let full_pr_diff = diff_local(&inputs.repo_path, &full_pr_range, cancel)?;
    let reviewable_file_count = changed_file_targets_local(&inputs.repo_path, &full_pr_range, cancel)?
        .iter()
        .filter(|path| crate::review::counts_toward_file_limit(path))
        .count();
    let db = inputs.open_db()?;
    db.set_pr_changed_file_count(inputs.pr_id, Some(reviewable_file_count as i64))?;
    let max_files = db.settings()?.review_max_files;
    if reviewable_file_count as i64 > max_files {
        return Err(format!(
            "review file limit exceeded: {reviewable_file_count} reviewable changed files exceeds the {max_files}-file review limit"
        ));
    }
    inputs.open_db()?.store_finding_audit(
        inputs.run_id,
        &crate::review::findings::seed_audit(&serde_json::json!({ "comments": [] }), &head_sha, &full_pr_diff),
    )?;
    let scope_error = |error: String| {
        if cancel.load(Ordering::Relaxed) {
            "cancelled".into()
        } else {
            format!("Não é seguro reutilizar a revisão anterior: {error}. Escolha Reexecutar completa para revisar o MR inteiro. Nenhuma inferência iniciada.")
        }
    };
    if should_reuse_review_coverage(inputs)
        && inputs.last_reviewed_sha.is_some()
        && db
            .review_coverage_patch_ids_for_pr(inputs.pr_id)
            .map_err(&scope_error)?
            .is_none()
    {
        return Err(scope_error("malformed persisted coverage was quarantined".into()));
    }
    let coverage_snapshot = if should_reuse_review_coverage(inputs) {
        inputs
            .last_reviewed_sha
            .as_deref()
            .map(|head| {
                crate::review::coverage::snapshot_for_reuse(
                    &inputs.repo_path,
                    &db,
                    inputs.repo_id,
                    inputs.pr_id,
                    head,
                    cancel,
                )
            })
            .transpose()
            .map_err(&scope_error)?
            .flatten()
    } else {
        None
    };
    let scope_reuse = crate::review::scope::prepare_scope_reuse(
        &inputs.repo_path,
        inputs.prior_scope.as_ref(),
        inputs.last_reviewed_sha.as_deref(),
        &current_scope,
        coverage_snapshot.as_ref(),
        inputs.force_full,
        cancel,
    )
    .map_err(&scope_error)?;
    let coverage_resolution =
        if should_reuse_review_coverage(inputs) && scope_reuse.is_none() && historical_prior_head.is_none() {
            crate::review::coverage::resolve(
                &inputs.repo_path,
                inputs.db_handle.as_ref().ok_or("Review storage unavailable")?,
                inputs.repo_id,
                &base_rev,
                &head_sha,
                &full_pr_diff,
                historical_prior_head,
                cancel,
            )
            .map_err(&scope_error)?
        } else {
            None
        };
    let usable_resolution = coverage_resolution.as_ref().filter(|resolution| resolution.usable);
    let prior_reviewed_head = coverage_snapshot
        .as_ref()
        .filter(|_| coverage_resolution.as_ref().is_none_or(|resolution| resolution.usable))
        .and(historical_prior_head);
    let mut reviewed_since = if scope_reuse.is_some() {
        None
    } else {
        usable_resolution
            .and_then(|resolution| resolution.review_base.clone())
            .or_else(|| prior_reviewed_head.map(str::to_string))
    };
    let (mut changed, mut diff, mut scope) = if let Some(selected) = &scope_reuse {
        (
            selected.changed_files.clone(),
            selected.diff.clone(),
            format!(
                "base synchronization; {} file(s) reuse verified coverage",
                selected.selection.reused_files.len()
            ),
        )
    } else if usable_resolution.is_some_and(|resolution| resolution.fully_covered) {
        (
            Vec::new(),
            String::new(),
            "verified coverage reused; no new PR changes".to_string(),
        )
    } else if let Some(old) = &reviewed_since {
        let (changed, diff) = incremental_review_changes(&inputs.repo_path, old, &head_sha, &full_pr_changed, cancel)
            .map_err(&scope_error)?;
        (changed, diff, format!("incremental since {}", &old[..old.len().min(8)]))
    } else {
        if !inputs.force_full && (inputs.last_reviewed_sha.is_some() || inputs.prior_scope.is_some()) {
            return Err(scope_error(
                "verified coverage or a compatible prior scope is unavailable".into(),
            ));
        }
        (full_pr_changed.clone(), full_pr_diff.clone(), "full PR".to_string())
    };
    let mut selection = scope_reuse
        .as_ref()
        .map(|selected| selected.selection.clone())
        .unwrap_or_else(|| crate::review::scope::ScopeSelection {
            mode: if reviewed_since.is_some() {
                "incremental"
            } else {
                "full"
            }
            .into(),
            requested_full: inputs.force_full,
            source_run_id: coverage_snapshot
                .as_ref()
                .filter(|coverage| reviewed_since.as_deref() == Some(coverage.scope.head_sha.as_str()))
                .map(|coverage| coverage.run_id),
            full_pr_files: full_pr_changed.clone(),
            reused_files: full_pr_changed
                .iter()
                .filter(|path| !changed.contains(path))
                .cloned()
                .collect(),
            excluded_files: Vec::new(),
        });
    let stacked_sources = if should_reuse_review_coverage(inputs) {
        crate::review::coverage::reuse_stacked_scope(
            &inputs.repo_path,
            &db,
            inputs.repo_id,
            inputs.pr_id,
            coverage_snapshot.as_ref(),
            &current_scope,
            &mut changed,
            &mut selection,
            cancel,
        )
        .map_err(&scope_error)?
    } else {
        Vec::new()
    };
    if !stacked_sources.is_empty() {
        diff = integration::scoped_diff(&full_pr_diff, &changed, &full_pr_changed);
        reviewed_since = None;
        selection.source_run_id = selection.source_run_id.or_else(|| stacked_sources.first().copied());
        scope = format!(
            "content-verified stacked coverage; {} file(s) reuse verified coverage",
            selection.reused_files.len()
        );
    }
    if coverage_snapshot.is_some() {
        crate::review::scope::exclude_policy_paths(&mut changed, &mut selection).map_err(&scope_error)?;
        diff = integration::scoped_diff(&diff, &changed, &full_pr_changed);
    }
    let reprocessed_source = if inputs.reprocess && !inputs.force_full && changed.is_empty() {
        let (source_run, previous) = db
            .reprocess_context_plan(inputs.pr_id, inputs.run_id)
            .map_err(&scope_error)?
            .ok_or_else(|| scope_error("previous nonempty analysis scope is unavailable".into()))?;
        if !crate::review::scope::plan_belongs_to(&previous, inputs.repo_id, source_run) {
            return Err(scope_error("previous scope belongs to another repository".into()));
        }
        (changed, selection) =
            crate::review::scope::reprocess_selection(&previous.changed_files, &full_pr_changed, source_run)
                .map_err(&scope_error)?;
        diff = integration::scoped_diff(&full_pr_diff, &changed, &full_pr_changed);
        reviewed_since = None;
        scope = format!(
            "explicit re-run of review {source_run}; fresh analysis against the current base, result cache disabled"
        );
        Some(source_run)
    } else {
        None
    };
    let deferred = crate::review::scope::focus_files(
        &mut changed,
        &diff,
        usize::try_from(db.settings()?.review_focus_files).unwrap_or(0),
    );
    if !deferred.is_empty() {
        diff = integration::scoped_diff(&diff, &changed, &full_pr_changed);
    }
    // A review that deferred files supplies no coverage at all (`deferred_files_warning`).
    let coverage_ids = if changed.is_empty() && !selection.excluded_files.is_empty() {
        Vec::new()
    } else {
        crate::review::coverage::covered_ids(&inputs.repo_path, &base_rev, &head_sha, &full_pr_diff, cancel)?
    };
    let deferred_note = deferred_files_note(&deferred);
    if scope_reuse.is_some() && reprocessed_source.is_none() {
        scope = format!(
            "{}; {} file(s) reuse verified coverage; {} excluded, not reviewed",
            selection.mode,
            selection.reused_files.len(),
            selection.excluded_files.len()
        );
    }
    let coverage_source = if scope_reuse.is_some()
        || reprocessed_source.is_some()
        || !selection.excluded_files.is_empty()
        || !stacked_sources.is_empty()
    {
        let mut source = serde_json::to_value(&selection).map_err(|error| error.to_string())?;
        if !stacked_sources.is_empty() {
            source["stackedSourceRunIds"] = serde_json::json!(stacked_sources);
        }
        Some(source.to_string())
    } else {
        crate::review::coverage::source_json(coverage_resolution.as_ref())
    };
    db.begin_review_coverage(&crate::db::ReviewCoverageInput {
        repo_id: inputs.repo_id,
        pr_id: inputs.pr_id,
        run_id: inputs.run_id,
        base_ref: &inputs.base_ref,
        base_sha: &base_rev,
        merge_base_sha: &merge_base_sha,
        head_sha: &head_sha,
        changed_files: &full_pr_changed,
        covered_patch_ids: &coverage_ids,
        diff_hash: &crate::review::coverage::diff_hash(&full_pr_diff),
        diff_text: &full_pr_diff,
        source_json: coverage_source.as_deref(),
    })?;
    progress(
        &phase_json("context", "running"),
        &format!(
            "{} arquivo(s) alterado(s) ({scope}). Calculando diff (git local)...",
            changed.len()
        ),
    );

    // Index-backed selection of related context (own DB connection in this thread).
    progress(
        &phase_json("context", "running"),
        "Selecionando arquivos relacionados no índice...",
    );
    let plan = {
        let (mut plan, base_sha) =
            build_snapshot_context_plan(inputs, &changed, &head_sha, &base_rev, cancel, progress)?;
        let db = inputs.open_db()?;
        let _ = db.set_review_run_context_metadata(
            inputs.run_id,
            reviewed_base_sha.as_deref(),
            &head_sha,
            base_sha.as_deref(),
            Some(&inputs.base_ref),
            reviewed_base_sha.as_deref(),
        );
        // Audit: exactly what range/scope this run reviewed.
        plan.base_ref = Some(inputs.base_ref.clone());
        plan.reviewed_base_sha = Some(base_rev.clone());
        plan.merge_base_sha = Some(merge_base_sha.clone());
        plan.scope_reason = Some(if reprocessed_source.is_some() {
            "Reexecução explícita: repetir o escopo de análise anterior contra a base atual; cache de resultados desativado"
        } else if selection.mode == "content_reuse" {
            "Reuso verificado por conteúdo: comparar base/head anterior e atual; reanalisar arquivos alterados ou não comprovados, independente da ordem dos commits"
        } else if scope_reuse.is_some() {
            "Base sincronizada: revisar arquivos afetados contra a base atual; reutilizar cobertura verificada por conteúdo nos inalterados"
        } else if reviewed_since.is_some() {
            "Mudanças incrementais do MR com cobertura de revisão verificada"
        } else if inputs.force_full {
            "Revisão completa pedida explicitamente; reuso de cobertura desativado"
        } else {
            "Primeira revisão contra a base imediata do MR"
        }.into());
        plan.scope_selection = Some(selection.clone());
        plan.incremental = reviewed_since.is_some();
        plan.range = match reviewed_since.as_deref() {
            Some(old) => format!("{old}..{head_sha}"),
            None => format!("{base_rev}...{head_sha}"),
        };
        plan.head_sha = Some(head_sha.clone());
        plan.base_sha = base_sha.clone();
        plan.prior_sha = reviewed_since
            .clone()
            .or_else(|| inputs.prior_scope.as_ref().map(|scope| scope.head_sha.clone()))
            .or_else(|| inputs.last_reviewed_sha.clone());
        let manifest = serde_json::to_string(&plan).unwrap_or_default();
        let _ = db.save_review_context_snapshot(
            inputs.repo_id,
            inputs.pr_id,
            Some(inputs.run_id),
            &head_sha,
            base_sha.as_deref(),
            None,
            &manifest,
            plan.changed_files.len() as i64,
            plan.related_files.len() as i64,
        );
        plan
    };
    let risk_plan = if full_pr_changed == changed {
        plan.clone()
    } else {
        build_snapshot_context_plan(inputs, &full_pr_changed, &head_sha, &base_rev, cancel, progress)?.0
    };

    for name in [
        "head-context.sqlite3",
        "head-context.sqlite3-wal",
        "head-context.sqlite3-shm",
    ] {
        match fs::remove_file(run_dir.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Could not clean up snapshot index: {error}")),
        }
    }
    if (reviewed_since.is_some() || scope_reuse.is_some() || !stacked_sources.is_empty()) && changed.is_empty() {
        progress(
            &no_change_phase_json(),
            "Nenhuma mudança nova do MR para revisar. Resultado anterior preservado; exclusões de política não entram na revisão.",
        );
        let excluded = selection
            .excluded_files
            .iter()
            .map(|file| format!("\n- `{}`: {:?} — excluído, não revisado", file.path, file.reason))
            .collect::<String>();
        return Ok(preserved_outcome(
            inputs,
            &head_sha,
            &excluded,
            &risk_plan,
            &full_pr_diff,
        ));
    }

    if !changed.is_empty() && diff.trim().is_empty() {
        return Err(format!(
            "computed {} changed file(s), but PR diff is empty for range {base_rev}...{head_sha}",
            changed.len()
        ));
    }
    let context_dir = run_dir.join("context");
    fs::create_dir_all(&context_dir).map_err(|error| error.to_string())?;
    fs::write(context_dir.join("PR_DIFF.patch"), &diff).map_err(|error| error.to_string())?;
    if reviewed_since.is_some() {
        fs::write(
            context_dir.join("PUBLICATION_DIFF.patch"),
            integration::scoped_diff(&full_pr_diff, &changed, &full_pr_changed),
        )
        .map_err(|error| error.to_string())?;
    }
    // The author's PR title + description = stated intent/scope. Lets the reviewer
    // avoid flagging intentionally-deferred work (e.g. "consumer lands in a later PR").
    let pr_body = if inputs.pr_body.trim().is_empty() {
        "(no description provided)"
    } else {
        inputs.pr_body.trim()
    };
    fs::write(
        context_dir.join("PR.md"),
        format!("# {}\n\n{}\n", inputs.pr_title.trim(), pr_body),
    )
    .map_err(|error| error.to_string())?;
    if let Some(body) = inputs
        .unresolved_review_body
        .as_deref()
        .filter(|body| !body.trim().is_empty())
    {
        fs::write(
            context_dir.join("PENDING_REVIEW.md"),
            format!(
                "# Previously generated review awaiting decision\n\nHead: {}\n\n{}\n",
                inputs.unresolved_review_head.as_deref().unwrap_or("unknown"),
                body.trim()
            ),
        )
        .map_err(|error| error.to_string())?;
    }
    progress(
        &phase_json("context", "running"),
        &format!(
            "Lendo {} alterados + {} relacionados (git local)...",
            changed.len(),
            plan.related_files.len()
        ),
    );
    materialize_files(
        &inputs.repo_path,
        &base_rev,
        &head_sha,
        &changed,
        &plan,
        &context_dir,
        &inputs.skill_content,
        true,
        cancel,
    )?;
    let mut prepared_inputs = inputs.clone();
    progress(
        &phase_json("context", "running"),
        "Congelando o arquivo de código permitido antes da inferência",
    );
    prepared_inputs.frozen = Some(frozen::capture(inputs, &plan, &head_sha, cancel).inspect_err(|_| {
        progress(
            &preparation_failure_phase(false, cancel.load(Ordering::Relaxed)),
            "Preparação do arquivo falhou antes da inferência",
        )
    })?);
    let inputs = &prepared_inputs;
    let _ = cleanup_pr_ref(&inputs.repo_path, inputs.number);
    let excluded = frozen_unreviewable_paths(inputs, &changed);
    // Deferred files were not frozen for review, so a run that deferred any has more to review.
    if !changed.is_empty() && deferred.is_empty() && changed.iter().all(|path| excluded.contains(path)) {
        // A re-review whose new changes are all excluded keeps the previous outcome, like one with
        // no new changes; only a first or full review is skipped.
        if reviewed_since.is_some() || scope_reuse.is_some() || !stacked_sources.is_empty() {
            progress(
                &no_change_phase_json(),
                "Nenhuma mudança nova do MR para revisar. Resultado anterior preservado; exclusões de política não entram na revisão.",
            );
            // Nothing was reviewed, so this run covers no patch, like a re-review whose files were
            // excluded before the freeze.
            inputs
                .open_db()?
                .begin_review_coverage(&crate::db::ReviewCoverageInput {
                    repo_id: inputs.repo_id,
                    pr_id: inputs.pr_id,
                    run_id: inputs.run_id,
                    base_ref: &inputs.base_ref,
                    base_sha: &base_rev,
                    merge_base_sha: &merge_base_sha,
                    head_sha: &head_sha,
                    changed_files: &full_pr_changed,
                    covered_patch_ids: &[],
                    diff_hash: &crate::review::coverage::diff_hash(&full_pr_diff),
                    diff_text: &full_pr_diff,
                    source_json: coverage_source.as_deref(),
                })?;
            let excluded = changed
                .iter()
                .map(|path| format!("\n- `{path}`: excluído da evidência de revisão, não revisado"))
                .collect::<String>();
            return Ok(preserved_outcome(
                inputs,
                &head_sha,
                &excluded,
                &risk_plan,
                &full_pr_diff,
            ));
        }
        let sample = changed.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
        return Err(format!(
            "{NOTHING_REVIEWABLE}: every changed file is excluded from review evidence (generated, vendored, lockfile, binary or oversized): {sample}{}",
            if changed.len() > 5 { ", …" } else { "" }
        ));
    }
    progress(
        &phase_json("context", "ok"),
        &format!(
            "Contexto pronto: {} arquivo(s) para revisar, {} reutilizados, {} relacionados, {} pacotes.",
            plan.changed_files.len(),
            selection.reused_files.len(),
            plan.related_files.len(),
            plan.packages.len()
        ),
    );
    let review_map = build_review_map(inputs, &plan, &head_sha, &changed, &diff);
    let context_md = render_context_md(inputs, &plan, &head_sha, &changed);
    fs::write(context_dir.join("CONTEXT.md"), &context_md).map_err(|error| error.to_string())?;
    write_review_map(&context_dir, &review_map)?;

    check_cancel(cancel)?;
    if let Some(frozen) = &inputs.frozen {
        frozen::prepare_stage(
            inputs,
            &context_dir,
            &review_map.reviewable_files,
            &diff,
            reviewed_since
                .as_deref()
                .unwrap_or(&frozen.archive.scope().merge_base_sha),
        )
        .inspect_err(|_| {
            progress(
                &preparation_failure_phase(false, cancel.load(Ordering::Relaxed)),
                "Fontes atribuídas falharam no preflight; nenhuma inferência iniciada",
            )
        })?;
    }
    if should_chunk_prepared_review(inputs, &review_map, &context_dir, run_dir) {
        let mut output = run_sharded_review(
            inputs,
            cancel,
            progress,
            run_dir,
            &context_dir,
            &review_map,
            &head_sha,
            reviewed_since.as_deref(),
            &base_rev,
            &full_pr_diff,
            &risk_plan,
        )?;
        output.risk = score_review_risk(&risk_plan, &full_pr_diff, output.comments_json.as_deref());
        output.body.push_str(&deferred_note);
        output.evidence_warnings.extend(deferred_files_warning(&deferred));
        progress(
            &sharded_phase_json("ready", "ok"),
            "Revisão em chunks pronta para aprovação.",
        );
        return Ok(output);
    }
    progress(
        &phase_json("review", "queued"),
        "Aguardando um runner livre. As revisões compartilham os runners e começam na vez.",
    );

    let summary_path = run_dir.join("summary.md");
    let comments_path = run_dir.join("comments.json");
    fs::write(&summary_path, "").map_err(|error| error.to_string())?;
    fs::write(&comments_path, "").map_err(|error| error.to_string())?;

    let config_path = run_dir.join("claude.settings.json");
    fs::write(&config_path, review_settings_json()).map_err(|error| error.to_string())?;

    let prompt_path = run_dir.join("prompt.md");
    fs::write(
        &prompt_path,
        render_prompt(inputs, &context_dir, &summary_path, &review_map.reviewable_files),
    )
    .map_err(|error| error.to_string())?;

    crate::review::policy::validate_prepared_inputs(
        dir_bytes(&context_dir) as usize,
        file_bytes(&prompt_path) as usize,
    )?;
    let context_hash = context_fingerprint(&context_dir)?;
    let transcript_path = run_dir.join("transcript.json");
    fs::write(&transcript_path, "").map_err(|error| error.to_string())?;
    let stdout_path = run_dir.join("claude.stdout");
    let stderr_path = run_dir.join("claude.stderr");

    frozen::preflight(
        inputs,
        run_dir,
        &[(run_dir.to_path_buf(), context_dir.clone(), prompt_path.clone())],
        cancel,
    )
    .inspect_err(|_| {
        progress(
            &preparation_failure_phase(false, cancel.load(Ordering::Relaxed)),
            "Preflight do snapshot falhou antes da inferência",
        )
    })?;
    let stage_started_at = now_millis();
    let reviewing = Instant::now();
    budget::with(inputs, |budget| budget.plan(1, 1));
    let execution = spawn_and_supervise(
        inputs,
        cancel,
        progress,
        &SpawnPaths {
            config: &config_path,
            prompt: &prompt_path,
            transcript: &transcript_path,
            stdout: &stdout_path,
            stderr: &stderr_path,
            progress_phase: "review",
            sharded_progress: false,
        },
    );

    // Parse artifacts. The summary is required; comments are optional.
    let mut lens_usage = RunUsage::default();
    let artifacts = execution.and_then(|()| {
        progress(&phase_json("review", "ok"), "Saída da IA gerada");
        progress(
            &phase_json("validate", "running"),
            "Validando cobertura, evidências citadas e âncoras do diff",
        );
        if context_fingerprint(&context_dir)? != context_hash {
            return Err("Incomplete review: reviewer modified the evidence snapshot.".into());
        }
        let evidence_warnings = output_repair::validate(
            inputs,
            cancel,
            progress,
            &SpawnPaths {
                config: &config_path,
                prompt: &prompt_path,
                transcript: &transcript_path,
                stdout: &stdout_path,
                stderr: &stderr_path,
                progress_phase: "review",
                sharded_progress: false,
            },
            &context_dir,
            || {
                frozen::materialize(
                    inputs,
                    &run_dir.join("report.json"),
                    &context_dir,
                    &review_map.reviewable_files,
                    &head_sha,
                    if inputs.frozen.is_some() { &diff } else { &full_pr_diff },
                    &summary_path,
                    &comments_path,
                )
            },
        )?;
        budget::with(inputs, |budget| budget.reviewed(reviewing.elapsed()));
        let materialize_to = |report: &Path, context: &Path, summary: &Path, comments: &Path| {
            frozen::materialize(
                inputs,
                report,
                context,
                &review_map.reviewable_files,
                &head_sha,
                &diff,
                summary,
                comments,
            )
        };
        let (lensed, usage) = lens::run(
            inputs,
            cancel,
            progress,
            &SpawnPaths {
                config: &config_path,
                prompt: &prompt_path,
                transcript: &transcript_path,
                stdout: &stdout_path,
                stderr: &stderr_path,
                progress_phase: "review",
                sharded_progress: false,
            },
            &context_dir,
            None,
            materialize_to,
        )?;
        lens_usage = usage;
        let evidence_warnings = if lensed {
            materialize_to(
                &run_dir.join("report.json"),
                &context_dir,
                &summary_path,
                &comments_path,
            )?
        } else {
            evidence_warnings
        };
        let body = fs::read_to_string(&summary_path).unwrap_or_default().trim().to_string();
        if body.is_empty() {
            let stderr = fs::read_to_string(&stderr_path).unwrap_or_default();
            let tail = tail_of(&stderr, 600);
            return Err(format!("Claude did not produce a review summary. {tail}")
                .trim()
                .to_string());
        }
        let comments_raw = fs::read_to_string(&comments_path).unwrap_or_default();
        let comments_json = Some(require_valid_comments(&comments_raw, &head_sha, &full_pr_diff)?);
        let had_inline_comments = comments_json_contains_items(comments_json.as_deref());
        let comments_json = sanitize_review_comments(comments_json.as_deref())?;
        let body = repair_body_only_review_point(body, comments_json.as_deref());
        if !had_inline_comments || comments_json_contains_items(comments_json.as_deref()) {
            validate_review_templates(&body, comments_json.as_deref())?;
        }

        Ok((body, comments_json, evidence_warnings))
    });

    // Token usage for this run, parsed from the export transcript before cleanup.
    let usage = parse_transcript_usage(&transcript_path);
    record_stage(
        inputs,
        "single",
        None,
        match &artifacts {
            Ok(_) => "done",
            Err(error) if error == "cancelled" => "cancelled",
            Err(_) => "failed",
        },
        None,
        false,
        &usage,
        StageMetrics {
            prompt_bytes: file_bytes(&prompt_path),
            diff_bytes: file_bytes(&context_dir.join("PR_DIFF.patch")),
            context_bytes: dir_bytes(&context_dir),
            file_count: review_map.reviewable_files.len() as i64,
            related_file_count: review_map.related_files.len() as i64,
        },
        stage_started_at,
        now_millis(),
    );

    let (body, comments_json, mut evidence_warnings) = artifacts?;
    evidence_warnings.extend(deferred_files_warning(&deferred));
    let usage = if lens_usage.by_model.is_empty() {
        usage
    } else {
        sum_usage(vec![usage, lens_usage])
    };
    let risk = score_review_risk(&risk_plan, &full_pr_diff, comments_json.as_deref());
    progress(
        &phase_json("validate", "ok"),
        &format!(
            "Revisão concluída com {} achados inline. Saída processada e risco pontuado.",
            review_comment_count(comments_json.as_deref())
        ),
    );
    progress(&phase_json("ready", "ok"), "Revisão pronta para aprovação.");
    Ok(ReviewOutput {
        body: body + &deferred_note,
        comments_json,
        evidence_warnings,
        usage,
        risk,
        no_new_changes: false,
    })
}

/// `body` as the review keeps it, in its stored body and its audit. Reviews still list
/// suggestions, so low-priority points never turn into findings, but only the
/// `review_suggestions_enabled` setting (off by default) keeps them for posting.
/// The output of a re-review with no new reviewable changes: no model ran, and the previous
/// outcome stands; this is never a new clean review.
fn preserved_outcome(
    inputs: &ReviewInputs,
    head_sha: &str,
    excluded: &str,
    risk_plan: &crate::db::ReviewContextPlan,
    full_pr_diff: &str,
) -> ReviewOutput {
    let pending = inputs.unresolved_review_body.as_deref().map(|body| format!("\n\nRevisão pendente anterior\nEste MR ainda tem uma revisão gerada sem resolver de `{}`. Leve isso em conta antes de marcar o MR como revisado.\n\n{}", inputs.unresolved_review_head.as_deref().unwrap_or("head anterior"), body.trim())).unwrap_or_default();
    let comments_json = format!(r#"{{"commit_id":"{head_sha}","comments":[]}}"#);
    ReviewOutput {
        body: format!(
            "OpenCodeReview — re-revisão incremental do MR #{}\n\nResumo\n- Head SHA: {head_sha}\n- Nenhuma mudança nova do MR para revisar; arquivos inalterados reutilizam cobertura verificada.\n- Nenhum modelo executado e nenhum achado novo validado.\n\nExclusões de política (não revisadas){excluded}\n\nEncerramento\n- Resultado da revisão anterior preservado; isto não é uma revisão limpa nova.{pending}",
            inputs.number,
        ),
        risk: score_review_risk(risk_plan, full_pr_diff, Some(comments_json.as_str())),
        comments_json: Some(comments_json),
        evidence_warnings: Vec::new(),
        usage: RunUsage::default(),
        no_new_changes: true,
    }
}

pub(crate) fn kept_review_body(db: &crate::db::Database, body: &str) -> String {
    if db.settings().is_ok_and(|settings| settings.review_suggestions_enabled) {
        body.to_string()
    } else {
        crate::review::without_suggestions(body)
    }
}

/// The summary section naming the files a focused review left out; empty when it reviewed all.
/// A review that deferred files is limited: like any review with an evidence warning it supplies
/// no coverage, never counts as the last reviewed head and is never clean, so nothing treats the
/// deferred files as reviewed.
fn deferred_files_warning(deferred: &[String]) -> Option<crate::review::warnings::EvidenceWarning> {
    (!deferred.is_empty()).then(|| crate::review::warnings::EvidenceWarning {
        reason: "files_not_reviewed".into(),
        path: None,
        reference: Some(format!("{} changed file(s) above review_focus_files", deferred.len())),
    })
}

pub(crate) fn deferred_files_note(deferred: &[String]) -> String {
    if deferred.is_empty() {
        return String::new();
    }
    let listed = deferred
        .iter()
        .take(50)
        .map(|path| format!("\n- `{path}`"))
        .collect::<String>();
    let more = deferred
        .len()
        .checked_sub(50)
        .filter(|more| *more > 0)
        .map(|more| format!("\n- and {more} more"))
        .unwrap_or_default();
    format!(
        "\n\nNot reviewed ({} file(s) above the review focus limit; raise review_focus_files and re-run to cover them){listed}{more}",
        deferred.len()
    )
}

pub(crate) struct SpawnPaths<'a> {
    pub(crate) config: &'a Path,
    pub(crate) prompt: &'a Path,
    pub(crate) transcript: &'a Path,
    pub(crate) stdout: &'a Path,
    pub(crate) stderr: &'a Path,
    pub(crate) progress_phase: &'a str,
    pub(crate) sharded_progress: bool,
}

pub(crate) fn spawn_and_supervise(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    paths: &SpawnPaths,
) -> Result<(), String> {
    if inputs.frozen.is_some() {
        return frozen::execute(inputs, cancel, progress, paths);
    }
    run_claude(inputs, cancel, progress, paths)
}

pub(crate) fn spawn_phase_json(paths: &SpawnPaths) -> String {
    if paths.sharded_progress {
        sharded_phase_json(paths.progress_phase, "running")
    } else {
        phase_json(paths.progress_phase, "running")
    }
}

/// Per-run settings used for every local review pass (single, chunk, and final):
/// reads and a fixed set of read-only shell commands are pre-approved, edits are
/// limited to the run workspace (the summary/comments outputs), and delegation,
/// web access and MCP stay unavailable. Anything else is denied in `dontAsk` mode.
pub(crate) fn review_settings_json() -> String {
    let reads = [
        "cat", "ls", "find", "grep", "rg", "head", "tail", "wc", "sed", "awk", "cut", "sort", "uniq", "nl", "tr",
        "pwd", "echo", "tree", "stat", "file", "diff", "basename", "dirname",
    ];
    let mut allow = vec![
        "Read".to_string(),
        "Grep".into(),
        "Glob".into(),
        "Edit(./**)".into(),
        "Write(./**)".into(),
    ];
    allow.extend(reads.iter().map(|command| format!("Bash({command}:*)")));
    serde_json::json!({
        "permissions": {
            "defaultMode": "dontAsk",
            "deny": ["Agent", "Task", "WebFetch", "WebSearch", "mcp__*"],
            "allow": allow
        },
        "enableAllProjectMcpServers": false,
        "includeCoAuthoredBy": false,
        "cleanupPeriodDays": 1
    })
    .to_string()
}

const LOCAL_TOOLS: &str = "Read,Grep,Glob,Bash,Write,Edit";

pub(crate) fn run_claude(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    paths: &SpawnPaths,
) -> Result<(), String> {
    progress(
        &crate::review::progress::execution_phase(
            &spawn_phase_json(paths),
            paths.progress_phase,
            "queued",
            now_millis(),
        ),
        "Aguardando uma vaga de processo de revisão",
    );
    let _permit = crate::review::policy::acquire(
        crate::review::policy::MAX_PROCESSES,
        inputs.review_parallel_chunks.max(1),
        cancel,
        inputs.pr_id,
    )?;
    let workspace_dir = paths
        .prompt
        .parent()
        .ok_or_else(|| "prompt path has no parent".to_string())?;
    let home = workspace_dir
        .parent()
        .ok_or_else(|| "run dir has no parent".to_string())?
        .join(format!(
            ".claude-home-{}-{}",
            std::process::id(),
            CLAUDE_HOME_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
    let result = supervise_claude(inputs, cancel, progress, paths, workspace_dir, &home);
    let _ = fs::remove_dir_all(&home);
    result
}

fn supervise_claude(
    inputs: &ReviewInputs,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, &str),
    paths: &SpawnPaths,
    workspace_dir: &Path,
    home: &Path,
) -> Result<(), String> {
    fs::write(paths.stdout, "").map_err(|error| error.to_string())?;
    let mut command = crate::providers::claude::local_command(home)?;
    command
        .current_dir(workspace_dir)
        .args(crate::providers::claude::print_args(
            paths.config,
            LOCAL_TOOLS,
            crate::providers::claude::MODEL,
            crate::providers::claude::EFFORT,
        ))
        .stdin(fs::File::open(paths.prompt).map_err(|error| error.to_string())?)
        .stdout(fs::File::create(paths.transcript).map_err(|error| error.to_string())?)
        .stderr(fs::File::create(paths.stderr).map_err(|error| error.to_string())?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start claude: {error}"))?;
    let pid = child.id();
    progress(
        &crate::review::progress::execution_phase(
            &spawn_phase_json(paths),
            paths.progress_phase,
            "running",
            now_millis(),
        ),
        &format!("Processo Claude iniciado com {}", inputs.model),
    );
    let started = Instant::now();
    let mut saw_output = false;
    let mut last_progress = Instant::now() - Duration::from_secs(1);
    let mut transcript_stamp = None;
    let mut transcript_log = String::new();

    loop {
        if cancel.load(Ordering::Relaxed) {
            kill_tree(pid, &mut child);
            return Err("cancelled".to_string());
        }
        if started.elapsed() > Duration::from_secs(DEFAULT_TIMEOUT_SECS) {
            kill_tree(pid, &mut child);
            return Err("Claude review timed out.".to_string());
        }
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                kill_tree(pid, &mut child);
                return Err(error.to_string());
            }
        };
        match status {
            Some(status) => {
                if status.success() {
                    if paths.progress_phase == "chunks" {
                        progress(
                            &crate::review::progress::execution_phase(
                                &spawn_phase_json(paths),
                                "chunks",
                                "validating",
                                now_millis(),
                            ),
                            "Validando cobertura, evidências e âncoras do chunk",
                        );
                    }
                    return Ok(());
                }
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if status.signal() == Some(9) {
                        return Err("Claude was killed with SIGKILL, likely due to memory pressure. Try Economic mode, a higher Auto threshold, or fewer parallel chunks.".to_string());
                    }
                }
                let transcript = fs::read_to_string(paths.transcript).unwrap_or_default();
                let stderr = fs::read_to_string(paths.stderr).unwrap_or_default();
                return Err(format!(
                    "Claude exited with {status}. {}",
                    crate::providers::claude::failure(&transcript, &stderr)
                ));
            }
            None => {
                if last_progress.elapsed() < Duration::from_secs(1) {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                last_progress = Instant::now();
                let stamp = fs::metadata(paths.transcript)
                    .ok()
                    .map(|meta| (meta.len(), meta.modified().ok()));
                if stamp != transcript_stamp {
                    transcript_log = transcript_live_log(&fs::read_to_string(paths.transcript).unwrap_or_default());
                    transcript_stamp = stamp;
                }
                let stderr = read_output_tail(paths.stderr);
                let combined = format!("{}\n{}", transcript_log, stderr);
                if !combined.trim().is_empty() {
                    saw_output = true;
                }
                if !saw_output && started.elapsed() > Duration::from_secs(NO_OUTPUT_TIMEOUT_SECS) {
                    kill_tree(pid, &mut child);
                    return Err(format!(
                        "Claude produced no output for {NO_OUTPUT_TIMEOUT_SECS}s using model `{}`.",
                        inputs.model
                    ));
                }
                let tail = tail_of(&combined, TAIL_MAX_CHARS);
                progress(&spawn_phase_json(paths), &tail);
                std::thread::sleep(Duration::from_millis(400));
            }
        }
    }
}
