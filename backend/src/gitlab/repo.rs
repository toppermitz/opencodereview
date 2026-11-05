use super::*;

/// `glab api --paginate` merges REST array pages into one flat JSON array.
pub(crate) fn api_paginate(route: &str) -> Result<Vec<serde_json::Value>, String> {
    let bytes = transport::glab_capture(&["api", "--paginate", route])?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    match value {
        serde_json::Value::Array(items) => Ok(items),
        other => Err(format!("expected a JSON array from glab --paginate, got {other}")),
    }
}

pub fn list_workspaces() -> Result<Vec<WorkspaceInput>, String> {
    let user: GlUser = glab_json(&["api", "user"])?;
    let mut workspaces = vec![WorkspaceInput {
        login: user.username.clone(),
        kind: "user".to_string(),
    }];
    let groups = api_paginate("groups?membership=true&min_access_level=10&per_page=100")?;
    for group in groups {
        let path = group
            .get("full_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if path.is_empty() {
            continue;
        }
        workspaces.push(WorkspaceInput {
            login: path,
            kind: "org".to_string(),
        });
        let _ = group.get("id").and_then(|v| v.as_i64()).or(Some(user.id));
    }
    workspaces.sort_by(|a, b| a.login.cmp(&b.login));
    workspaces.dedup_by(|a, b| a.login == b.login);
    Ok(workspaces)
}

pub fn list_repos(workspace_login: &str) -> Result<Vec<RepoInput>, String> {
    let projects = api_paginate("projects?membership=true&simple=false&per_page=100")?;
    Ok(projects
        .into_iter()
        .filter_map(|project| {
            let full_name = project.get("path_with_namespace")?.as_str()?.to_string();
            let namespace = project
                .get("namespace")
                .and_then(|n| n.get("full_path"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let belongs = namespace == workspace_login
                || full_name == workspace_login
                || full_name.starts_with(&format!("{workspace_login}/"));
            if !belongs {
                return None;
            }
            // Keep subgroup identity in `owner` so checkout paths stay unique
            // (`acme/team-a/api` → owner `acme/team-a`, name `api`).
            let name = project
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| full_name.rsplit_once('/').map(|(_, name)| name.to_string()))
                .unwrap_or_else(|| full_name.clone());
            let owner = full_name
                .rsplit_once('/')
                .map(|(owner, _)| owner.to_string())
                .filter(|owner| !owner.is_empty())
                .unwrap_or_else(|| workspace_login.to_string());
            let visibility = project.get("visibility").and_then(|v| v.as_str()).unwrap_or("private");
            Some(RepoInput {
                workspace_login: workspace_login.to_string(),
                owner,
                name,
                full_name,
                description: project
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .filter(|value| !value.trim().is_empty()),
                is_private: visibility != "public",
                default_branch: project
                    .get("default_branch")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        })
        .collect())
}

pub fn list_open_prs(full_name: &str) -> Result<Vec<GhPullRequest>, String> {
    let limit = std::env::var("OCR_GITLAB_MR_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1000);
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests?state=opened&per_page=100");
    let items = api_paginate(&route)?;
    Ok(items
        .into_iter()
        .take(limit)
        .filter_map(|mr| parse_merge_request(&mr).ok())
        .collect())
}

pub fn list_open_prs_for_login(full_name: &str, login: &str) -> Result<Vec<GhPullRequest>, String> {
    let all = list_open_prs(full_name)?;
    let login_l = login.to_ascii_lowercase();
    Ok(all
        .into_iter()
        .filter(|mr| {
            mr.assignees.as_ref().is_some_and(|list| {
                list.iter()
                    .any(|a| a.login.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(&login_l)))
            }) || mr.review_requests.as_ref().is_some_and(|list| {
                list.iter()
                    .any(|a| a.login.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(&login_l)))
            })
        })
        .collect())
}

pub(crate) fn parse_merge_request(value: &serde_json::Value) -> Result<GhPullRequest, String> {
    let author = value.get("author").ok_or("Missing merge request author")?;
    let diff_refs = value.get("diff_refs");
    let head = value
        .get("sha")
        .and_then(|v| v.as_str())
        .or_else(|| diff_refs.and_then(|d| d.get("head_sha")).and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string();
    let base = diff_refs
        .and_then(|d| d.get("base_sha"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let assignees = value.get("assignees").and_then(|v| v.as_array()).map(|list| {
        list.iter()
            .filter_map(|a| {
                Some(GhPrPrincipal {
                    login: a.get("username").and_then(|v| v.as_str()).map(str::to_string),
                    name: a.get("name").and_then(|v| v.as_str()).map(str::to_string),
                    slug: None,
                })
            })
            .collect()
    });
    let reviewers = value.get("reviewers").and_then(|v| v.as_array()).map(|list| {
        list.iter()
            .filter_map(|a| {
                Some(GhPrPrincipal {
                    login: a.get("username").and_then(|v| v.as_str()).map(str::to_string),
                    name: a.get("name").and_then(|v| v.as_str()).map(str::to_string),
                    slug: None,
                })
            })
            .collect()
    });
    Ok(GhPullRequest {
        number: value
            .get("iid")
            .and_then(|v| v.as_i64())
            .ok_or("Missing merge request iid")?,
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        author: GhPrAuthor {
            login: author
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            avatar_url: author.get("avatar_url").and_then(|v| v.as_str()).map(str::to_string),
        },
        is_draft: value.get("draft").and_then(|v| v.as_bool()).unwrap_or(false)
            || value.get("work_in_progress").and_then(|v| v.as_bool()).unwrap_or(false),
        head_ref_oid: head,
        base_ref_oid: base,
        base_ref_name: value.get("target_branch").and_then(|v| v.as_str()).map(str::to_string),
        head_ref_name: value.get("source_branch").and_then(|v| v.as_str()).map(str::to_string),
        url: value.get("web_url").and_then(|v| v.as_str()).map(str::to_string),
        updated_at: value.get("updated_at").and_then(|v| v.as_str()).map(str::to_string),
        changed_files: value
            .get("changes_count")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .or_else(|| value.get("changes_count").and_then(|v| v.as_i64())),
        assignees,
        review_requests: reviewers,
    })
}

#[cfg(test)]
pub(crate) const LIVE_PR_JSON_FIELDS: &str = "state,sha,diff_refs,target_branch,merged_at";

#[derive(Debug, Clone)]
pub struct LivePrState {
    pub state: String,
    pub head_sha: String,
    pub base_ref: String,
    pub base_sha: String,
    pub merged_at: Option<i64>,
}

impl LivePrState {
    pub fn review_scope(&self) -> crate::review::scope::ReviewScope {
        crate::review::scope::ReviewScope {
            head_sha: self.head_sha.clone(),
            base_ref: self.base_ref.clone(),
            base_sha: self.base_sha.clone(),
        }
    }
}

pub fn live_pr_state(full_name: &str, number: i64) -> Result<LivePrState, String> {
    live_pr_state_cancellable(full_name, number, &std::sync::atomic::AtomicBool::new(false))
}

pub fn live_pr_state_cancellable(
    full_name: &str,
    number: i64,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<LivePrState, String> {
    let project = project_api_id(full_name);
    let route = format!("projects/{project}/merge_requests/{number}");
    let output = glab_capture_cancellable(&["api", &route], cancel)?;
    parse_live_pr_state(&output)
}

pub(crate) fn normalize_mr_state(state: &str) -> String {
    match state.to_ascii_lowercase().as_str() {
        "opened" | "open" | "locked" => "OPEN".into(),
        "merged" => "MERGED".into(),
        "closed" => "CLOSED".into(),
        other => other.to_ascii_uppercase(),
    }
}

pub(crate) fn parse_live_pr_state(output: &[u8]) -> Result<LivePrState, String> {
    let value: serde_json::Value = serde_json::from_slice(output).map_err(|error| error.to_string())?;
    let state = value
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or("Missing merge request state")?;
    let diff_refs = value.get("diff_refs");
    let head_sha = value
        .get("sha")
        .and_then(|v| v.as_str())
        .or_else(|| diff_refs.and_then(|d| d.get("head_sha")).and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string();
    let base_sha = diff_refs
        .and_then(|d| d.get("base_sha"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let base_ref = value
        .get("target_branch")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(LivePrState {
        state: normalize_mr_state(state),
        head_sha,
        base_ref,
        base_sha,
        merged_at: value
            .get("merged_at")
            .and_then(|v| v.as_str())
            .and_then(crate::gitlab::iso8601_to_epoch_ms),
    })
}
