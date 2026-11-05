use super::*;

pub(crate) fn collect_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(collect_files(&path));
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

pub(crate) fn make_run_dir(pr_id: i64) -> Result<PathBuf, String> {
    // Two runs of one pull request in the same millisecond, from one process or two, must not
    // share a directory.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "ocr-review-pr{pr_id}-{stamp}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    Ok(dir)
}

pub(crate) fn save_review_debug_artifacts(
    inputs: &ReviewInputs,
    run_dir: &Path,
    result: &Result<ReviewOutput, String>,
) -> Option<PathBuf> {
    let base = std::env::var("OCR_REVIEW_DEBUG_DIR")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let run_name = run_dir.file_name().and_then(|name| name.to_str()).unwrap_or("run");
    let dest = PathBuf::from(base).join(format!("pr{}-run{}-{run_name}", inputs.pr_id, inputs.run_id));
    if dest.starts_with(run_dir) {
        return None;
    }
    let _ = fs::create_dir_all(&dest);
    copy_tree(run_dir, &dest);
    let status = match result {
        Ok(output) => serde_json::json!({
            "status": "ok",
            "summaryBytes": output.body.len(),
            "commentsBytes": output.comments_json.as_ref().map(|raw| raw.len()).unwrap_or(0),
            "usage": {
                "inputTokens": output.usage.input_tokens,
                "outputTokens": output.usage.output_tokens,
                "cacheReadTokens": output.usage.cache_read_tokens,
                "cacheCreationTokens": output.usage.cache_creation_tokens,
                "model": output.usage.model,
            }
        }),
        Err(error) => serde_json::json!({ "status": "error", "error": error }),
    };
    let _ = fs::write(
        dest.join("debug-result.json"),
        serde_json::to_string_pretty(&status).unwrap_or_default(),
    );
    Some(dest)
}

pub(crate) fn harness_log(run_dir: &Path, message: &str) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("harness.log"))
    {
        let _ = writeln!(file, "{stamp} {message}");
    }
}

pub(crate) fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("cancelled".to_string())
    } else {
        Ok(())
    }
}

pub(crate) fn no_change_phase_json() -> String {
    let mut phases: Vec<serde_json::Value> = serde_json::from_str(&phase_json("ready", "ok")).unwrap_or_default();
    for phase in &mut phases {
        if phase["id"] == "review" || phase["id"] == "validate" {
            phase["status"] = "skipped".into();
            phase["inferenceSkipped"] = true.into();
            phase["detail"] = "Nenhuma mudança nova do MR para revisar; sem execução de modelo".into();
        }
    }
    serde_json::to_string(&phases).unwrap_or_default()
}

pub(crate) fn index_phase_json(status: &str) -> String {
    phase_json("index", status)
}

pub(crate) fn phase_json(active: &str, status: &str) -> String {
    phase_json_for(
        active,
        status,
        &[
            ("index", "Preparar repositório"),
            ("context", "Contexto"),
            ("review", "Revisão"),
            ("validate", "Validar"),
            ("ready", "Pronto"),
        ],
    )
}

pub(crate) fn preparation_failure_phase(sharded: bool, cancelled: bool) -> String {
    let raw = if sharded {
        chunk_planning_failure_phase()
    } else {
        phase_json("review", "failed")
    };
    let mut phases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
    for phase in &mut phases {
        if matches!(phase["id"].as_str(), Some("review" | "chunks" | "final" | "validate")) {
            phase["inferenceSkipped"] = true.into();
        }
        if cancelled && phase["status"] == "failed" {
            phase["status"] = "cancelled".into();
        }
    }
    serde_json::to_string(&phases).unwrap_or(raw)
}

pub(crate) fn chunk_planning_failure_phase() -> String {
    let raw = sharded_phase_json("chunks", "failed");
    let mut phases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap_or_default();
    for phase in &mut phases {
        if phase["id"] == "chunks" || phase["id"] == "final" {
            phase["inferenceSkipped"] = true.into();
        }
        if phase["id"] == "chunks" {
            phase["detail"] = "Preparação dos chunks falhou antes da execução do modelo.".into();
            phase["chunks"] = serde_json::json!([]);
        }
    }
    serde_json::to_string(&phases).unwrap_or(raw)
}

pub(crate) fn sharded_phase_json(active: &str, status: &str) -> String {
    phase_json_for(
        active,
        status,
        &[
            ("index", "Preparar repositório"),
            ("context", "Contexto"),
            ("chunks", "Chunks de revisão"),
            ("merge", "Mesclar achados"),
            ("final", "Integração final"),
            ("validate", "Validar"),
            ("ready", "Pronto"),
        ],
    )
}

pub(crate) fn phase_json_for(active: &str, status: &str, phases: &[(&str, &str)]) -> String {
    let items: Vec<String> = phases
        .iter()
        .map(|(id, label)| {
            let state = if *id == active {
                status
            } else if reached(active, id, phases) {
                "ok"
            } else {
                "pending"
            };
            format!("{{\"id\":\"{id}\",\"label\":\"{label}\",\"status\":\"{state}\"}}")
        })
        .collect();
    format!("[{}]", items.join(","))
}

pub(crate) fn reached(active: &str, id: &str, phases: &[(&str, &str)]) -> bool {
    let active_idx = phases.iter().position(|(phase_id, _)| phase_id == &active).unwrap_or(0);
    let id_idx = phases.iter().position(|(phase_id, _)| phase_id == &id).unwrap_or(0);
    id_idx < active_idx
}

pub(crate) fn transcript_live_log(raw: &str) -> String {
    let events = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .collect::<Vec<_>>();
    let parts = events
        .iter()
        .filter(|event| event["type"] == "assistant" || event["type"] == "user")
        .flat_map(|event| event["message"]["content"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return String::new();
    }
    let mut lines = vec![format!("Claude live transcript · {} event(s)", parts.len())];
    for part in parts.iter().rev().take(50).rev() {
        match part["type"].as_str() {
            Some("text") => {
                let text = part["text"].as_str().unwrap_or_default().trim();
                if !text.is_empty() {
                    lines.push(format!("assistant: {}", one_line(text, 600)));
                }
            }
            Some("tool_use") => lines.push(format_tool_call(part)),
            Some("tool_result") => lines.extend(format_observation(&part["content"])),
            _ => {}
        }
    }
    lines.join("\n")
}

pub(crate) fn format_tool_call(call: &serde_json::Value) -> String {
    let name = call.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
    let Some(args) = call.get("input") else {
        return format!("tool: {name}");
    };
    if let Some(path) = args
        .get("file_path")
        .or_else(|| args.get("path"))
        .and_then(|v| v.as_str())
    {
        let range = match (args.get("offset"), args.get("limit")) {
            (Some(offset), Some(limit)) => format!(" offset={offset} limit={limit}"),
            _ => String::new(),
        };
        return format!("tool: {name} {}{}", short_path(path), range);
    }
    if let Some(command) = args.get("command").and_then(|v| v.as_str()) {
        return format!("tool: {name} `{}`", one_line(command, 180));
    }
    format!("tool: {name} {}", one_line(&args.to_string(), 220))
}

pub(crate) fn format_observation(observation: &serde_json::Value) -> Option<String> {
    let text = match observation {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(format!("output: {}", one_line(&text, 420)))
}

pub(crate) fn short_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() <= 4 {
        path.to_string()
    } else {
        format!("…/{}", parts[parts.len().saturating_sub(4)..].join("/"))
    }
}

pub(crate) fn one_line(text: &str, max: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::new();
    for ch in compact.chars().take(max) {
        out.push(ch);
    }
    if compact.chars().count() > max {
        out.push('…');
    }
    out
}

pub(crate) fn tail_of(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let start = text
        .char_indices()
        .rev()
        .map(|(idx, _)| idx)
        .find(|idx| text.len() - *idx <= max)
        .unwrap_or(0);
    format!("…{}", &text[start..])
}
