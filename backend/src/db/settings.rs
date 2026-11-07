use super::*;

impl Database {
    pub fn settings(&self) -> Result<Settings, String> {
        let theme = self.setting("theme")?.unwrap_or_else(|| "dark".to_string());
        let font_size = self
            .setting("font_size")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(14);
        let sidebar_collapsed = self
            .setting("sidebar_collapsed")?
            .map(|value| value == "true")
            .unwrap_or(false);
        let mission_workspace_configured = self
            .setting("mission_workspace_configured")?
            .map(|value| value == "true")
            .unwrap_or(false);
        let mission_index_done = self
            .setting("mission_index_done")?
            .map(|value| value == "true")
            .unwrap_or(false);
        let poll_interval_seconds = self
            .setting("poll_interval_seconds")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS);
        let auto_skip_keywords = self
            .setting("auto_skip_keywords")?
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        let review_auto_enabled = self
            .setting("review_auto_enabled")?
            .map(|value| value == "true")
            .unwrap_or(true);
        let review_concurrency = self
            .setting("review_concurrency")?
            .and_then(|value| value.parse().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_REVIEW_CONCURRENCY);
        let review_depth_mode = self
            .setting("review_depth_mode")?
            .filter(|value| matches!(value.as_str(), "economic" | "auto" | "deep"))
            .unwrap_or_else(|| "auto".to_string());
        let review_deep_threshold = self
            .setting("review_deep_threshold")?
            .and_then(|value| value.parse().ok())
            .filter(|value| *value >= 2)
            .unwrap_or(10);
        let review_chunk_size = self
            .setting("review_chunk_size")?
            .and_then(|value| value.parse().ok())
            .filter(|value| (2..=20).contains(value))
            .unwrap_or(5);
        let review_parallel_chunks = self
            .setting("review_parallel_chunks")?
            .and_then(|value| value.parse().ok())
            .filter(|value| (1..=20).contains(value))
            .unwrap_or(6);
        let review_max_files = self
            .setting("review_max_files")?
            .and_then(|value| value.parse().ok())
            .filter(|value| *value > 0)
            .unwrap_or(100);
        let review_focus_files = self
            .setting("review_focus_files")?
            .and_then(|value| value.parse().ok())
            .filter(|value| *value >= 0)
            .unwrap_or(0);
        let review_lens_pass = self.setting("review_lens_pass")?.is_some_and(|value| value == "true");
        let review_suggestions_enabled = self
            .setting("review_suggestions_enabled")?
            .map(|value| value == "true")
            .unwrap_or(false);
        let dismiss_stale_approvals = self
            .setting("dismiss_stale_approvals")?
            .map(|value| value == "true")
            .unwrap_or(true);
        let auto_index_stale_repos = self
            .setting("auto_index_stale_repos")?
            .map(|value| value == "true")
            .unwrap_or(false);
        let auto_post_mode = self
            .setting("auto_post_mode")?
            .filter(|mode| matches!(mode.as_str(), "off" | "comment" | "request_changes"))
            .unwrap_or_else(|| "comment".to_string());
        let review_assigned_only = self
            .setting("review_assigned_only")?
            .map(|value| value == "true")
            .unwrap_or(false);
        let review_target_filter_enabled = self
            .setting("review_target_filter_enabled")?
            .map(|value| value == "true")
            .unwrap_or(false);
        let review_target_logins = self
            .setting("review_target_logins")?
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        let review_target_teams = self
            .setting("review_target_teams")?
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        let auto_approve_enabled = self
            .setting("auto_approve_enabled")?
            .map(|value| value == "true")
            .unwrap_or(false);
        let auto_approve_max_risk_score = self
            .setting("auto_approve_max_risk_score")?
            .and_then(|value| value.parse().ok())
            .unwrap_or(20);
        let index_strategy = "graph".to_string();
        let pr_summary_comment_enabled = self
            .setting("pr_summary_comment_enabled")?
            .map(|value| value == "true")
            .unwrap_or(true);
        let review_result_mode = self
            .setting("review_result_mode")?
            .filter(|value| matches!(value.as_str(), "silent" | "result_comment"))
            .unwrap_or_else(|| "result_comment".to_string());
        let review_archive_retention_days = self
            .setting("review_archive_retention_days")?
            .and_then(|value| value.parse().ok())
            .filter(|value| (0..=365).contains(value))
            .unwrap_or(0);
        Ok(Settings {
            theme,
            font_size,
            sidebar_collapsed,
            mission_workspace_configured,
            mission_index_done,
            poll_interval_seconds,
            auto_skip_keywords,
            review_auto_enabled,
            review_concurrency,
            review_depth_mode,
            review_deep_threshold,
            review_chunk_size,
            review_parallel_chunks,
            review_max_files,
            review_focus_files,
            review_lens_pass,
            review_suggestions_enabled,
            dismiss_stale_approvals,
            auto_index_stale_repos,
            auto_post_mode,
            review_assigned_only,
            review_target_filter_enabled,
            review_target_logins,
            review_target_teams,
            auto_approve_enabled,
            auto_approve_max_risk_score,
            index_strategy,
            pr_summary_comment_enabled,
            review_result_mode,
            review_archive_retention_days,
        })
    }

    const ALLOWED_SETTING_KEYS: &'static [&'static str] = &[
        "theme",
        "font_size",
        "sidebar_collapsed",
        "mission_workspace_configured",
        "mission_index_done",
        "poll_interval_seconds",
        "auto_skip_keywords",
        "review_auto_enabled",
        "review_concurrency",
        "review_depth_mode",
        "review_deep_threshold",
        "review_chunk_size",
        "review_parallel_chunks",
        "review_max_files",
        "review_focus_files",
        "review_lens_pass",
        "review_suggestions_enabled",
        "dismiss_stale_approvals",
        "auto_index_stale_repos",
        "auto_post_mode",
        "review_assigned_only",
        "review_target_filter_enabled",
        "review_target_logins",
        "review_target_teams",
        "auto_approve_enabled",
        "auto_approve_max_risk_score",
        "index_strategy",
        "pr_summary_comment_enabled",
        "review_result_mode",
        "review_archive_retention_days",
    ];

    pub fn save_setting(&self, key: &str, value: &str) -> Result<Settings, String> {
        if !Self::ALLOWED_SETTING_KEYS.contains(&key) {
            return Err(format!("unknown setting key: {key}"));
        }
        if key == "review_archive_retention_days" {
            let days = value
                .parse::<i64>()
                .map_err(|_| "Retention days must be an integer between 0 and 365.".to_string())?;
            if !(0..=365).contains(&days) {
                return Err("Retention days must be between 0 and 365.".to_string());
            }
        }
        if key == "auto_post_mode" && !matches!(value, "off" | "comment" | "request_changes") {
            return Err("auto_post_mode must be off, comment, or request_changes.".to_string());
        }
        if key == "auto_approve_enabled" && !matches!(value, "true" | "false") {
            return Err("auto_approve_enabled must be true or false.".to_string());
        }
        if key == "auto_approve_max_risk_score" {
            let score = value
                .parse::<i64>()
                .map_err(|_| "Auto-approve max risk score must be an integer >= 0.".to_string())?;
            if score < 0 {
                return Err("Auto-approve max risk score must be an integer >= 0.".to_string());
            }
        }
        if key == "review_result_mode" && !matches!(value, "silent" | "result_comment") {
            return Err("review_result_mode must be silent or result_comment.".to_string());
        }
        if key == "review_depth_mode" && !matches!(value, "economic" | "auto" | "deep") {
            return Err("Review depth must be economic, auto, or deep.".to_string());
        }
        if key == "index_strategy" && value != "graph" {
            return Err("Index strategy must be graph.".to_string());
        }
        if matches!(
            key,
            "review_deep_threshold" | "review_chunk_size" | "review_parallel_chunks" | "review_max_files"
        ) && value.parse::<i64>().ok().filter(|value| *value > 0).is_none()
        {
            return Err("Review numeric settings must be positive.".to_string());
        }
        if key == "review_focus_files" && value.parse::<i64>().ok().filter(|value| *value >= 0).is_none() {
            return Err("review_focus_files must be zero or a positive number of files.".to_string());
        }
        if matches!(key, "review_lens_pass" | "review_suggestions_enabled") && !matches!(value, "true" | "false") {
            return Err(format!("{key} must be true or false."));
        }
        self.conn
            .execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, (EXTRACT(EPOCH FROM now()) * 1000)::BIGINT)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, value],
            )
            .map_err(|error| error.to_string())?;
        self.settings()
    }

    pub fn save_settings_batch(&self, update: &SettingsUpdate) -> Result<SettingsSnapshot, String> {
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        if let Some(selection) = &update.repositories {
            let workspaces = self.list_workspaces()?;
            for login in &selection.logins {
                if !workspaces.iter().any(|workspace| workspace.login == *login) {
                    return Err(format!("Unknown workspace: {login}"));
                }
            }
            if let Some(login) = selection
                .monitor_all
                .iter()
                .find(|login| !selection.logins.contains(login))
            {
                return Err(format!(
                    "Select the workspace {login} before monitoring all its repositories."
                ));
            }
            let workspace_ids: HashSet<i64> = workspaces
                .iter()
                .filter(|workspace| selection.logins.contains(&workspace.login))
                .map(|workspace| workspace.id)
                .collect();
            let mut selected_ids: HashSet<i64> = selection.repo_ids.iter().copied().collect();
            for workspace in workspaces
                .iter()
                .filter(|workspace| selection.monitor_all.contains(&workspace.login))
            {
                selected_ids.extend(self.workspace_repo_ids(workspace.id)?);
            }
            let mut repos: HashMap<i64, Repo> = self
                .monitored_repos()?
                .into_iter()
                .map(|repo| (repo.id, repo))
                .collect();
            for id in &selected_ids {
                let repo = self.repo_by_id(*id)?;
                if !repo.workspace_id.is_some_and(|id| workspace_ids.contains(&id)) {
                    return Err(format!(
                        "Select the workspace for {} before enabling it.",
                        repo.full_name
                    ));
                }
                repos.insert(*id, repo);
            }
            self.set_selected_workspaces(&selection.logins)?;
            self.set_monitor_all_workspaces(&selection.monitor_all)?;
            for repo in repos.values() {
                let enabled = selected_ids.contains(&repo.id);
                if enabled == repo.subscription_enabled {
                    continue;
                }
                self.save_repo_subscription(
                    repo.id,
                    enabled,
                    &repo.review_mode,
                    repo.skip_draft_prs,
                    repo.skip_self_prs,
                )?;
                if enabled {
                    self.enqueue_repo_index(repo.id)?;
                }
            }
        }
        for (key, value) in &update.settings {
            let range = match key.as_str() {
                "poll_interval_seconds" => Some(1..=i64::MAX),
                "review_concurrency" => Some(1..=crate::review::policy::MAX_RUNNERS as i64),
                "review_deep_threshold" => Some(2..=i64::MAX),
                "review_chunk_size" => Some(2..=20),
                "review_parallel_chunks" => Some(1..=20),
                "review_max_files" => Some(1..=i64::MAX),
                "review_focus_files" => Some(0..=i64::MAX),
                "review_archive_retention_days" => Some(0..=365),
                _ => None,
            };
            if range.is_some_and(|range| !value.parse::<i64>().is_ok_and(|number| range.contains(&number))) {
                return Err(format!("Invalid value for {key}."));
            }
            self.save_setting(key, value)?;
        }
        let snapshot = SettingsSnapshot {
            settings: self.settings()?,
            workspaces: self.list_workspaces()?,
            selected_repos: self.monitored_repos()?,
        };
        tx.commit().map_err(|error| error.to_string())?;
        Ok(snapshot)
    }

    pub fn poll_status(&self) -> Result<PollStatus, String> {
        let settings = self.settings()?;
        let monitored_repo_count = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM subscriptions s JOIN repos r ON r.id = s.repo_id WHERE s.enabled = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        Ok(PollStatus {
            poll_interval_seconds: settings.poll_interval_seconds,
            running: self
                .setting("poll_running")?
                .map(|value| value == "true")
                .unwrap_or(false),
            last_poll_at: self.setting("last_poll_at")?.and_then(|value| value.parse().ok()),
            next_poll_at: self.setting("next_poll_at")?.and_then(|value| value.parse().ok()),
            last_error: self.setting("last_poll_error")?.filter(|value| !value.is_empty()),
            monitored_repo_count,
        })
    }

    pub fn record_poll_start(&self) -> Result<PollStatus, String> {
        self.save_setting_value("poll_running", "true")?;
        self.poll_status()
    }

    pub fn record_poll_result(&self, now: i64, error: Option<&str>) -> Result<PollStatus, String> {
        let settings = self.settings()?;
        let next = now + settings.poll_interval_seconds * 1000;
        self.save_setting_values(&[
            ("poll_running", "false".to_string()),
            ("last_poll_at", now.to_string()),
            ("next_poll_at", next.to_string()),
            ("last_poll_error", error.unwrap_or("").to_string()),
        ])?;
        self.poll_status()
    }

    /// The model and effort reviews use on `provider`; its default when unset or no longer offered.
    pub(crate) fn review_engine(&self, provider: &str) -> Result<(String, String), String> {
        use crate::providers::engines;
        let stored = self
            .setting(&engines::setting_key(provider))?
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok());
        Ok(stored
            .and_then(|value| {
                engines::resolve(
                    provider,
                    value["model"].as_str()?,
                    value["effort"].as_str().unwrap_or(""),
                )
                .ok()
            })
            .unwrap_or_else(|| engines::default_engine(provider)))
    }

    pub(crate) fn save_review_engine(&self, provider: &str, model: &str, effort: &str) -> Result<(), String> {
        use crate::providers::engines;
        let (model, effort) = engines::resolve(provider, model, effort)?;
        self.save_setting_value(
            &engines::setting_key(provider),
            &serde_json::json!({ "model": model, "effort": effort }).to_string(),
        )
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM settings WHERE key = $1")
            .map_err(|error| error.to_string())?;
        let mut rows = stmt.query(params![key]).map_err(|error| error.to_string())?;
        match rows.next().map_err(|error| error.to_string())? {
            Some(row) => row.get(0).map(Some).map_err(|error| error.to_string()),
            None => Ok(None),
        }
    }

    pub(super) fn ensure_setting_default(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, (EXTRACT(EPOCH FROM now()) * 1000)::BIGINT) ON CONFLICT DO NOTHING",
                params![key, value],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(crate) fn save_setting_value(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, (EXTRACT(EPOCH FROM now()) * 1000)::BIGINT)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, value],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(super) fn save_setting_values(&self, values: &[(&str, String)]) -> Result<(), String> {
        let tx = self.conn.unchecked_transaction().map_err(|error| error.to_string())?;
        for (key, value) in values {
            tx.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES ($1, $2, (EXTRACT(EPOCH FROM now()) * 1000)::BIGINT)
       ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, value],
            )
            .map_err(|error| error.to_string())?;
        }
        tx.commit().map_err(|error| error.to_string())
    }
}
