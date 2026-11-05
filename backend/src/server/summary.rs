use super::*;

pub(crate) fn maybe_enqueue_pr_summary(db: &Database, inputs: &review::ReviewInputs) {
    let Ok(settings) = db.settings() else {
        return;
    };
    if !settings.pr_summary_comment_enabled {
        return;
    }
    let Ok((_repo_id, _full_name, _number, head_sha)) = db.pr_review_coords(inputs.pr_id) else {
        return;
    };
    if head_sha.is_empty() {
        return;
    }
    if matches!(db.latest_pr_summary_for_head(inputs.pr_id, &head_sha), Ok(Some(status)) if status == "running" || status == "posted")
    {
        return;
    }
    let Ok(summary_id) = db.start_pr_summary(inputs.pr_id, &head_sha) else {
        return;
    };
    let job = PrSummaryJob {
        db_handle: db.handle(),
        summary_id,
        pr_id: inputs.pr_id,
        repo_path: inputs.repo_path.clone(),
        full_name: inputs.full_name.clone(),
        number: inputs.number,
        base_ref: inputs.base_ref.clone(),
        head_sha,
        title: inputs.pr_title.clone(),
        body: inputs.pr_body.clone(),
    };
    thread::spawn(move || run_pr_summary_job(job));
}

#[derive(Clone)]
pub(crate) struct PrSummaryJob {
    db_handle: Db,
    summary_id: i64,
    pr_id: i64,
    repo_path: PathBuf,
    full_name: String,
    number: i64,
    base_ref: String,
    head_sha: String,
    title: String,
    body: String,
}

pub(crate) fn run_pr_summary_job(job: PrSummaryJob) {
    let result = build_pr_summary_body(&job).and_then(|(body, json)| {
        if let Ok(db) = job.db_handle.open() {
            if let Ok((_repo_id, _full_name, _number, current_head)) = db.pr_review_coords(job.pr_id) {
                if current_head != job.head_sha {
                    let _ = db.finish_pr_summary(
                        job.summary_id,
                        "skipped",
                        None,
                        None,
                        None,
                        Some("superseded by newer commit"),
                    );
                    return Err("superseded by newer commit".to_string());
                }
            }
        }
        let marker = pr_summary_marker(&job.full_name, job.number);
        gitlab::upsert_issue_comment_by_marker(&job.full_name, job.number, &marker, &body)
            .map(|posted| (body, json, posted.id))
    });
    if let Ok(db) = job.db_handle.open() {
        match result {
            Ok((body, json, comment_id)) => {
                let _ = db.finish_pr_summary(
                    job.summary_id,
                    "posted",
                    Some(&body),
                    Some(&json),
                    Some(comment_id),
                    None,
                );
            }
            Err(error) if error == "superseded by newer commit" => {}
            Err(error) => {
                let _ = db.finish_pr_summary(job.summary_id, "failed", None, None, None, Some(&error));
            }
        }
    }
}

pub(crate) fn build_pr_summary_body(job: &PrSummaryJob) -> Result<(String, String), String> {
    fetch_summary_refs(&job.repo_path, job.number, &job.base_ref)?;
    let base = format!(
        "origin/{}",
        job.base_ref.strip_prefix("origin/").unwrap_or(&job.base_ref)
    );
    let range = format!("{base}...{}", job.head_sha);
    let files = git_lines(&job.repo_path, &["diff", "--name-status", &range])?;
    let stat = git_lines(&job.repo_path, &["diff", "--shortstat", &range])
        .unwrap_or_default()
        .join(" ");
    let important = files
        .iter()
        .filter_map(|line| summary_file_row(line))
        .take(6)
        .collect::<Vec<_>>();
    let confidence = summary_confidence(important.len(), &stat);
    let marker = pr_summary_marker(&job.full_name, job.number);
    let summary = summary_bullets(job, &important, &stat);
    let flow = summary_flowchart(&important);
    let json = serde_json::json!({ "summary": &summary, "confidence": confidence.0, "confidenceReason": confidence.1, "importantFiles": &important, "flowchartMermaid": &flow }).to_string();
    let mut body = format!("{marker}\n\n<details>\n<summary><strong>OpenCodeReview Summary</strong> · confidence {}/5 · updated at {}</summary>\n\n## What changed\n\n{}\n\n## Confidence Score: {}/5\n\n{}\n\n## Important Files Changed\n\n| File | Overview |\n| --- | --- |\n", confidence.0, short_sha(&job.head_sha), summary.iter().map(|line| format!("- {line}")).collect::<Vec<_>>().join("\n"), confidence.0, confidence.1);
    for file in &important {
        body.push_str(&format!("| `{}` | {} |\n", file.path, file.overview));
    }
    if let Some(flow) = flow {
        body.push_str(&format!("\n## Flowchart\n\n```mermaid\n{flow}\n```\n"));
    }
    body.push_str(&format!(
        "\n</details>\n\n_Last updated by OpenCodeReview for commit `{}`._\n",
        short_sha(&job.head_sha)
    ));
    if body.len() > 6000 {
        let mut end = 5950;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        body.truncate(end);
        body.push_str("\n\n</details>\n\n_Trimmed by OpenCodeReview._\n");
    }
    Ok((body, json))
}

#[derive(Serialize)]
pub(crate) struct SummaryFile {
    path: String,
    overview: String,
}

pub(crate) fn summary_file_row(line: &str) -> Option<SummaryFile> {
    let path = line.split_whitespace().last()?.to_string();
    if path.is_empty() || is_low_value_summary_path(&path) {
        return None;
    }
    let overview = if path.contains("test") || path.contains("spec") {
        "Updates test coverage or expected behavior."
    } else if path.ends_with(".tsx") || path.ends_with(".jsx") {
        "Updates UI/component behavior."
    } else if path.ends_with(".ts") || path.ends_with(".js") || path.ends_with(".rs") {
        "Updates application logic."
    } else if path.contains("config") || path.ends_with(".json") || path.ends_with(".toml") {
        "Updates configuration or metadata."
    } else {
        "Changed as part of this PR."
    };
    Some(SummaryFile {
        path,
        overview: overview.to_string(),
    })
}

pub(crate) fn is_low_value_summary_path(path: &str) -> bool {
    path.contains("lock") || path.contains("dist/") || path.contains("target/") || path.contains("node_modules/")
}

pub(crate) fn summary_bullets(job: &PrSummaryJob, files: &[SummaryFile], stat: &str) -> Vec<String> {
    let mut out = vec![job.title.trim().to_string().trim().trim_end_matches('.').to_string()];
    if !job.body.trim().is_empty() {
        out.push(
            job.body
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .chars()
                .take(220)
                .collect(),
        );
    }
    out.push(format!(
        "Touches {} important file{}{}.",
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        if stat.is_empty() {
            "".to_string()
        } else {
            format!(" ({stat})")
        }
    ));
    out.into_iter().filter(|line| !line.is_empty()).take(4).collect()
}

pub(crate) fn summary_confidence(file_count: usize, stat: &str) -> (i64, &'static str) {
    if file_count == 0 {
        return (
            2,
            "Limited confidence because OpenCodeReview could not identify important changed source files.",
        );
    }
    if stat.contains("1000") || file_count >= 6 {
        return (
            3,
            "Medium confidence: the PR spans multiple files or areas, so the overview may omit details.",
        );
    }
    if file_count <= 3 {
        return (
            5,
            "High confidence: the PR is compact and the changed files are coherent.",
        );
    }
    (
        4,
        "Good confidence: the PR shape is clear, with a moderate number of changed files.",
    )
}

pub(crate) fn summary_flowchart(files: &[SummaryFile]) -> Option<String> {
    if files.is_empty() || files.len() > 6 {
        return None;
    }
    let mut lines = vec![
        "flowchart TD".to_string(),
        "  A[\"PR changes\"] --> B[\"Updated files\"]".to_string(),
    ];
    for (index, file) in files.iter().take(4).enumerate() {
        lines.push(format!("  B --> F{}[\"{}\"]", index + 1, file.path.replace('"', "'")));
    }
    Some(lines.join("\n"))
}

pub(crate) fn fetch_summary_refs(repo_path: &Path, number: i64, base_ref: &str) -> Result<(), String> {
    let branch = base_ref.strip_prefix("origin/").unwrap_or(base_ref);
    // With the gh token, as every other fetch: a private repository has no other credentials here.
    let fetch = |refspec: &str, force: bool| {
        let mut command = Command::new("git");
        command
            .current_dir(repo_path)
            .args(["fetch", "origin", refspec, "--no-tags", "--quiet"]);
        if force {
            command.arg("--force");
        }
        crate::gitlab::apply_git_https_auth(&mut command);
        command
    };
    let _ = fetch(&format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"), false).output();
    let status = fetch(
        &format!("refs/merge-requests/{number}/head:refs/ocr/summary-{number}"),
        true,
    )
    .status()
    .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("could not fetch merge request summary refs".to_string())
    }
}

pub(crate) fn git_lines(repo_path: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let mut command = Command::new("git");
    command.current_dir(repo_path).args(args);
    // A partial clone fetches missing objects on demand, which needs the token too.
    crate::gitlab::apply_git_https_auth(&mut command);
    let output = command.output().map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

pub(crate) fn pr_summary_marker(full_name: &str, number: i64) -> String {
    format!("<!-- ocr-pr-summary:v1 repo={full_name} pr={number} -->")
}

pub(crate) fn short_sha(sha: &str) -> &str {
    sha.get(0..8).unwrap_or(sha)
}

pub(crate) fn review_post_marker(
    full_name: &str,
    number: i64,
    run_id: i64,
    head_sha: &str,
    installation_id: &str,
) -> String {
    format!(
        "<!-- ocr-review-post:v1 repo={full_name} pr={number} run={run_id} head={head_sha} installation={installation_id} -->"
    )
}

pub(crate) enum LiveHeadValidationError {
    Stale(String),
    Unavailable(String),
}
