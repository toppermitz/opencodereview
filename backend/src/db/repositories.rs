use super::*;

impl Database {
    pub fn ensure_temp_repo_for_index(&self, repo_id: i64, full_name: &str) -> Result<(), String> {
        let now = now_millis();
        let (owner, name) = full_name.rsplit_once('/').unwrap_or(("unknown", full_name));
        self.conn
            .execute(
                "INSERT INTO repos (id, owner, name, full_name, is_private, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, 1, $5, $5)
                 ON CONFLICT(id) DO UPDATE SET owner = excluded.owner, name = excluded.name, full_name = excluded.full_name, updated_at = excluded.updated_at",
                params![repo_id, owner, name, full_name, now],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn monitored_repos(&self) -> Result<Vec<Repo>, String> {
        let mut stmt = self.conn.prepare(
      "SELECT r.id, r.workspace_id, r.owner, r.name, r.full_name, r.description, r.is_private, r.default_branch,
              r.skill_path, r.skill_status, r.skill_checked_at, r.skill_error,
              COALESCE(s.enabled, 0), COALESCE(s.review_mode, 'manual'),
              COALESCE(s.skip_draft_prs, 1), COALESCE(s.skip_self_prs, 1)
       FROM repos r JOIN subscriptions s ON s.repo_id = r.id
       WHERE s.enabled = 1
       ORDER BY r.full_name ASC",
    ).map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Repo {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    owner: row.get(2)?,
                    name: row.get(3)?,
                    full_name: row.get(4)?,
                    description: row.get(5)?,
                    is_private: row.get::<_, i64>(6)? == 1,
                    default_branch: row.get(7)?,
                    skill_path: row.get(8)?,
                    skill_status: row.get(9)?,
                    skill_checked_at: row.get(10)?,
                    skill_error: row.get(11)?,
                    subscription_enabled: row.get::<_, i64>(12)? == 1,
                    review_mode: row.get(13)?,
                    skip_draft_prs: row.get::<_, i64>(14)? == 1,
                    skip_self_prs: row.get::<_, i64>(15)? == 1,
                    poll_error: None,
                    poll_error_url: None,
                    poll_error_at: None,
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
    }

    pub fn upsert_pull_request(&self, pr: &PullRequestInput) -> Result<i64, String> {
        self.upsert_pull_request_with_github_updated_at(pr, None)
    }

    pub fn upsert_pull_request_with_github_updated_at(
        &self,
        pr: &PullRequestInput,
        github_updated_at: Option<i64>,
    ) -> Result<i64, String> {
        let now = now_millis();
        let is_stacked = pr.default_branch.as_deref().is_some_and(|default| {
            pr.base_ref
                .as_deref()
                .is_some_and(|base| !base.is_empty() && base != default)
        });
        self.conn.execute(
      "INSERT INTO pull_requests (repo_id, number, title, author, author_avatar_url, state, is_draft, head_sha, base_sha, base_ref, head_ref, default_branch, is_stacked, url, review_lane, status_reason, last_synced_at, created_at, updated_at, github_updated_at)
       VALUES ($1, $2, $3, $4, $5, 'open', $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $16, $16, $17)
       ON CONFLICT(repo_id, number) DO UPDATE SET
         title = excluded.title,
         author = excluded.author,
         author_avatar_url = excluded.author_avatar_url,
         state = excluded.state,
         is_draft = excluded.is_draft,
         head_sha = excluded.head_sha,
         base_sha = excluded.base_sha,
         base_ref = excluded.base_ref,
         head_ref = excluded.head_ref,
         default_branch = excluded.default_branch,
         is_stacked = excluded.is_stacked,
         url = excluded.url,
         status_reason = CASE
           WHEN pull_requests.review_lane = 'manual_required'
            AND pull_requests.status_reason = 'Review cancelled'
            AND pull_requests.head_sha = excluded.head_sha
           THEN pull_requests.status_reason
           ELSE excluded.status_reason
         END,
         last_synced_at = excluded.last_synced_at,
         updated_at = excluded.updated_at,
         github_updated_at = COALESCE(excluded.github_updated_at, pull_requests.github_updated_at)",
      params![pr.repo_id, pr.number, pr.title, pr.author, pr.author_avatar_url, pr.is_draft as i64, pr.head_sha, pr.base_sha, pr.base_ref, pr.head_ref, pr.default_branch, is_stacked as i64, pr.url, pr.lane, pr.status_reason, now, github_updated_at],
    ).map_err(|error| error.to_string())?;
        self.conn
            .query_row(
                "SELECT id FROM pull_requests WHERE repo_id = $1 AND number = $2",
                params![pr.repo_id, pr.number],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
    }

    pub fn set_pr_changed_file_count(&self, pr_id: i64, count: Option<i64>) -> Result<(), String> {
        self.conn.execute(
            "INSERT INTO pr_review_file_counts (pr_id, changed_file_count, updated_at) VALUES ($1, $2, $3)
             ON CONFLICT(pr_id) DO UPDATE SET changed_file_count = excluded.changed_file_count, updated_at = excluded.updated_at",
            params![pr_id, count, now_millis()],
        ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn pr_changed_file_count(&self, pr_id: i64) -> Result<Option<i64>, String> {
        self.conn
            .query_row(
                "SELECT changed_file_count FROM pr_review_file_counts WHERE pr_id = $1",
                params![pr_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
    }

    pub fn list_workspaces(&self) -> Result<Vec<Workspace>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, login, kind, selected, repo_count, review_skill_repo_count, refreshed_at, monitor_all
       FROM workspaces ORDER BY selected DESC, login ASC",
            )
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Workspace {
                    id: row.get(0)?,
                    login: row.get(1)?,
                    kind: row.get(2)?,
                    selected: row.get::<_, i64>(3)? == 1,
                    repo_count: row.get(4)?,
                    review_skill_repo_count: row.get(5)?,
                    refreshed_at: row.get(6)?,
                    monitor_all: row.get::<_, i64>(7)? == 1,
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
    }

    pub fn upsert_workspaces(&self, workspaces: &[WorkspaceInput]) -> Result<Vec<Workspace>, String> {
        let now = now_millis();
        for workspace in workspaces {
            self.conn
                .execute(
                    "INSERT INTO workspaces (provider, login, kind, selected, created_at, updated_at)
         VALUES ('github', $1, $2, 0, $3, $3)
         ON CONFLICT(login) DO UPDATE SET kind = excluded.kind, updated_at = excluded.updated_at",
                    params![workspace.login, workspace.kind, now],
                )
                .map_err(|error| error.to_string())?;
        }
        self.list_workspaces()
    }

    pub fn set_selected_workspaces(&self, logins: &[String]) -> Result<Vec<Workspace>, String> {
        self.conn
            .execute(
                "UPDATE workspaces SET selected = 0, updated_at = $1",
                params![now_millis()],
            )
            .map_err(|error| error.to_string())?;
        for login in logins {
            self.conn
                .execute(
                    "UPDATE workspaces SET selected = 1, updated_at = $1 WHERE login = $2",
                    params![now_millis(), login],
                )
                .map_err(|error| error.to_string())?;
        }
        self.list_workspaces()
    }

    pub fn set_monitor_all_workspaces(&self, logins: &[String]) -> Result<(), String> {
        self.conn
            .execute("UPDATE workspaces SET monitor_all = 0 WHERE monitor_all = 1", [])
            .map_err(|error| error.to_string())?;
        for login in logins {
            self.conn
                .execute("UPDATE workspaces SET monitor_all = 1 WHERE login = $1", params![login])
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn monitor_all_logins(&self) -> Result<Vec<String>, String> {
        Ok(self
            .list_workspaces()?
            .into_iter()
            .filter(|workspace| workspace.selected && workspace.monitor_all)
            .map(|workspace| workspace.login)
            .collect())
    }

    pub fn workspace_repo_ids(&self, workspace_id: i64) -> Result<Vec<i64>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM repos WHERE workspace_id = $1")
            .map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(params![workspace_id], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
    }

    pub fn enable_monitor_all_repos(&self) -> Result<usize, String> {
        let mut enabled = 0;
        for workspace in self
            .list_workspaces()?
            .into_iter()
            .filter(|workspace| workspace.selected && workspace.monitor_all)
        {
            for id in self.workspace_repo_ids(workspace.id)? {
                let repo = self.repo_by_id(id)?;
                if repo.subscription_enabled {
                    continue;
                }
                self.save_repo_subscription(
                    repo.id,
                    true,
                    &repo.review_mode,
                    repo.skip_draft_prs,
                    repo.skip_self_prs,
                )?;
                self.enqueue_repo_index(repo.id)?;
                enabled += 1;
            }
        }
        Ok(enabled)
    }

    pub fn list_repos(&self, workspace_login: &str) -> Result<Vec<Repo>, String> {
        let mut stmt = self.conn.prepare(
      "SELECT r.id, r.workspace_id, r.owner, r.name, r.full_name, r.description, r.is_private, r.default_branch,
              r.skill_path, r.skill_status, r.skill_checked_at, r.skill_error,
              COALESCE(s.enabled, 0), COALESCE(s.review_mode, 'manual'),
              COALESCE(s.skip_draft_prs, 1), COALESCE(s.skip_self_prs, 1),
              r.poll_error, r.poll_error_url, r.poll_error_at
       FROM repos r
       JOIN workspaces w ON w.id = r.workspace_id
       LEFT JOIN subscriptions s ON s.repo_id = r.id
       WHERE w.login = $1
       ORDER BY r.name ASC",
    ).map_err(|error| error.to_string())?;
        let rows = stmt
            .query_map(params![workspace_login], |row| {
                Ok(Repo {
                    id: row.get(0)?,
                    workspace_id: row.get(1)?,
                    owner: row.get(2)?,
                    name: row.get(3)?,
                    full_name: row.get(4)?,
                    description: row.get(5)?,
                    is_private: row.get::<_, i64>(6)? == 1,
                    default_branch: row.get(7)?,
                    skill_path: row.get(8)?,
                    skill_status: row.get(9)?,
                    skill_checked_at: row.get(10)?,
                    skill_error: row.get(11)?,
                    subscription_enabled: row.get::<_, i64>(12)? == 1,
                    review_mode: row.get(13)?,
                    skip_draft_prs: row.get::<_, i64>(14)? == 1,
                    skip_self_prs: row.get::<_, i64>(15)? == 1,
                    poll_error: row.get(16)?,
                    poll_error_url: row.get(17)?,
                    poll_error_at: row.get(18)?,
                })
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())
    }

    pub fn upsert_repos(&self, repos: &[RepoInput]) -> Result<Vec<Repo>, String> {
        let now = now_millis();
        let mut last_workspace = None;
        for repo in repos {
            last_workspace = Some(repo.workspace_login.clone());
            let workspace_id: i64 = self
                .conn
                .query_row(
                    "SELECT id FROM workspaces WHERE login = $1",
                    params![repo.workspace_login],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            self.conn.execute(
        "INSERT INTO repos (workspace_id, owner, name, full_name, description, is_private, default_branch, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
         ON CONFLICT(full_name) DO UPDATE SET
           workspace_id = excluded.workspace_id,
           owner = excluded.owner,
           name = excluded.name,
           description = excluded.description,
           is_private = excluded.is_private,
           default_branch = excluded.default_branch,
           removed_at = NULL,
           updated_at = excluded.updated_at",
        params![workspace_id, repo.owner, repo.name, repo.full_name, repo.description, repo.is_private as i64, repo.default_branch, now],
      ).map_err(|error| error.to_string())?;
        }
        if let Some(workspace_login) = last_workspace {
            let names: HashSet<&str> = repos.iter().map(|repo| repo.full_name.as_str()).collect();
            let workspace_id: i64 = self
                .conn
                .query_row(
                    "SELECT id FROM workspaces WHERE login = $1",
                    params![workspace_login],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            let mut stmt = self
                .conn
                .prepare("SELECT id, full_name FROM repos WHERE workspace_id = $1")
                .map_err(|error| error.to_string())?;
            let rows = stmt
                .query_map(params![workspace_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?;
            for row in rows {
                let (repo_id, full_name) = row.map_err(|error| error.to_string())?;
                if !names.contains(full_name.as_str()) {
                    self.conn
                        .execute("DELETE FROM subscriptions WHERE repo_id = $1", params![repo_id])
                        .map_err(|error| error.to_string())?;
                    // The repository leaves the workspace but keeps its review history; it comes
                    // back as it was if the workspace lists it again.
                    self.conn
                        .execute(
                            "UPDATE repos SET workspace_id = NULL, removed_at = $2, updated_at = $2 WHERE id = $1",
                            params![repo_id, now_millis()],
                        )
                        .map_err(|error| error.to_string())?;
                    // Nothing can be done about its open notices anymore (its subscription is gone).
                    self.conn
                        .execute(
                            "UPDATE pending_actions SET status = 'resolved', resolved_at = $2
                             WHERE status != 'resolved'
                               AND (repo_id = $1 OR pr_id IN (SELECT id FROM pull_requests WHERE repo_id = $1))",
                            params![repo_id, now_millis()],
                        )
                        .map_err(|error| error.to_string())?;
                }
            }
            self.refresh_workspace_counts(&workspace_login)?;
            self.list_repos(&workspace_login)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn save_repo_subscription(
        &self,
        repo_id: i64,
        enabled: bool,
        review_mode: &str,
        skip_draft_prs: bool,
        skip_self_prs: bool,
    ) -> Result<(), String> {
        let now = now_millis();
        self.conn.execute(
      "INSERT INTO subscriptions (repo_id, enabled, review_mode, skip_draft_prs, skip_self_prs, created_at, updated_at)
       VALUES ($1, $2, $3, $4, $5, $6, $6)
       ON CONFLICT(repo_id) DO UPDATE SET
         enabled = excluded.enabled,
         review_mode = excluded.review_mode,
         skip_draft_prs = excluded.skip_draft_prs,
         skip_self_prs = excluded.skip_self_prs,
         updated_at = excluded.updated_at",
      params![repo_id, enabled as i64, review_mode, skip_draft_prs as i64, skip_self_prs as i64, now],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }

    pub fn update_repo_skill(
        &self,
        full_name: &str,
        status: &str,
        path: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.conn.execute(
      "UPDATE repos SET skill_status = $1, skill_path = $2, skill_error = $3, skill_checked_at = $4, updated_at = $4 WHERE full_name = $5",
      params![status, path, error, now_millis(), full_name],
    ).map_err(|error| error.to_string())?;
        let workspace_login: String = self
            .conn
            .query_row(
                "SELECT w.login FROM repos r JOIN workspaces w ON w.id = r.workspace_id WHERE r.full_name = $1",
                params![full_name],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        self.refresh_workspace_counts(&workspace_login)
    }

    pub fn repo_by_id(&self, repo_id: i64) -> Result<Repo, String> {
        let mut stmt = self.conn.prepare(
      "SELECT r.id, r.workspace_id, r.owner, r.name, r.full_name, r.description, r.is_private, r.default_branch,
              r.skill_path, r.skill_status, r.skill_checked_at, r.skill_error,
              COALESCE(s.enabled, 0), COALESCE(s.review_mode, 'manual'),
              COALESCE(s.skip_draft_prs, 1), COALESCE(s.skip_self_prs, 1),
              r.poll_error, r.poll_error_url, r.poll_error_at
       FROM repos r LEFT JOIN subscriptions s ON s.repo_id = r.id WHERE r.id = $1",
    ).map_err(|error| error.to_string())?;
        stmt.query_row(params![repo_id], |row| {
            Ok(Repo {
                id: row.get(0)?,
                workspace_id: row.get(1)?,
                owner: row.get(2)?,
                name: row.get(3)?,
                full_name: row.get(4)?,
                description: row.get(5)?,
                is_private: row.get::<_, i64>(6)? == 1,
                default_branch: row.get(7)?,
                skill_path: row.get(8)?,
                skill_status: row.get(9)?,
                skill_checked_at: row.get(10)?,
                skill_error: row.get(11)?,
                subscription_enabled: row.get::<_, i64>(12)? == 1,
                review_mode: row.get(13)?,
                skip_draft_prs: row.get::<_, i64>(14)? == 1,
                skip_self_prs: row.get::<_, i64>(15)? == 1,
                poll_error: row.get(16)?,
                poll_error_url: row.get(17)?,
                poll_error_at: row.get(18)?,
            })
        })
        .map_err(|error| error.to_string())
    }

    pub fn save_repo_default_branch(&self, repo_id: i64, default_branch: &str) -> Result<Repo, String> {
        let now = now_millis();
        self.conn
            .execute(
                "UPDATE repos SET default_branch = $1, updated_at = $2 WHERE id = $3",
                params![default_branch, now, repo_id],
            )
            .map_err(|error| error.to_string())?;
        self.repo_by_id(repo_id)
    }

    pub(super) fn refresh_workspace_counts(&self, login: &str) -> Result<(), String> {
        self.conn.execute(
      "UPDATE workspaces SET
         repo_count = (SELECT COUNT(*) FROM repos r WHERE r.workspace_id = workspaces.id),
         review_skill_repo_count = (SELECT COUNT(*) FROM repos r WHERE r.workspace_id = workspaces.id AND r.skill_status = 'found'),
         refreshed_at = $1,
         updated_at = $1
       WHERE login = $2",
      params![now_millis(), login],
    ).map_err(|error| error.to_string())?;
        Ok(())
    }
}
