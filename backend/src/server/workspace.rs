use super::*;

pub(crate) fn select_workspaces(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    let payload: SelectWorkspacesRequest = serde_json::from_slice(&request.body).map_err(|error| error.to_string())?;
    with_db(state, |db| db.set_selected_workspaces(&payload.logins))
}

pub(crate) fn list_workspace_repos(state: &SharedState, login: &str) -> Result<serde_json::Value, String> {
    with_db_read(state, |db| db.list_repos(login))
}

pub(crate) fn refresh_workspace_repos(state: &SharedState, login: &str) -> Result<serde_json::Value, String> {
    let repos = gitlab::list_repos(login)?;
    with_db(state, |db| db.upsert_repos(&repos))
}

pub(crate) fn subscribe_repo(
    request: &Request,
    state: &SharedState,
    repo_id: i64,
) -> Result<serde_json::Value, String> {
    let payload: SubscribeRepoRequest = serde_json::from_slice(&request.body).map_err(|error| error.to_string())?;
    let repo = with_db_raw(state, |db| {
        let repo = db.repo_by_id(repo_id)?;
        db.save_repo_subscription(
            repo_id,
            payload.enabled,
            "manual",
            payload.skip_draft_prs.unwrap_or(repo.skip_draft_prs),
            payload.skip_self_prs.unwrap_or(repo.skip_self_prs),
        )?;
        db.repo_by_id(repo_id)
    })?;
    // Indexing clones the repository in the background, so saving a selection stays instant.
    if payload.enabled {
        with_db_raw(state, |db| db.enqueue_repo_index(repo_id))?;
    }
    serde_json::to_value(repo).map_err(|error| error.to_string())
}
