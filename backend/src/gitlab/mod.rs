use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

static GL_LOGIN: Mutex<Option<String>> = Mutex::new(None);
static GL_TEAM_SLUGS: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static GL_HOST: OnceLock<String> = OnceLock::new();
static GLAB_CONFIG_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

mod activation;
pub(crate) use activation::{activation_pr, conversation_comments_page, unedited_author_command, ConversationComment};

mod auth;
mod comments;
mod issues;
mod oauth;
mod pr;
mod repo;
mod review_requests;
mod status_comments;
mod transport;

pub(crate) use issues::write_related_issues_context;

pub(crate) use auth::{apply_git_https_auth, apply_gitlab_host, is_valid_gitlab_host, project_api_id};
pub use auth::{auth_status, current_login, gitlab_host, run_auth_flow, sign_out};
pub use comments::{
    dedupe_review_comments, find_review_id_by_marker, post_review, prepare_review, resolve_stale_ocr_discussions,
    upsert_issue_comment_by_marker, DedupeResult, KeepPoint, PreparedReview,
};
pub(crate) use comments::{keep_points_from_annotated_comments, with_gitlab_verdict};
#[cfg(not(test))]
pub(crate) use pr::pr_file_paths;
pub use pr::{diff_anchors, pr_description};
pub use repo::{
    list_open_prs, list_open_prs_for_login, list_repos, list_workspaces, live_pr_state, live_pr_state_cancellable,
    LivePrState,
};
#[cfg(test)]
pub(crate) use status_comments::StatusComment;
pub(crate) use status_comments::{GitLabStatusComments, StatusCommentApi};
pub(crate) use transport::{glab_capture_cancellable, glab_json, iso8601_to_epoch_ms, run_raw};

#[cfg(test)]
pub(crate) use repo::{parse_live_pr_state, LIVE_PR_JSON_FIELDS};

use crate::db::{RepoInput, WorkspaceInput};

#[derive(Debug, Deserialize)]
struct GlUser {
    id: i64,
    username: String,
}

/// Merge request wire shape kept compatible with former GitHub PR consumers.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhPullRequest {
    pub number: i64,
    pub title: String,
    pub author: GhPrAuthor,
    pub is_draft: bool,
    pub head_ref_oid: String,
    pub base_ref_oid: Option<String>,
    pub base_ref_name: Option<String>,
    pub head_ref_name: Option<String>,
    pub url: Option<String>,
    pub updated_at: Option<String>,
    pub changed_files: Option<i64>,
    pub assignees: Option<Vec<GhPrPrincipal>>,
    pub review_requests: Option<Vec<GhPrPrincipal>>,
}

pub type MergeRequest = GhPullRequest;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GhPrPrincipal {
    pub login: Option<String>,
    pub name: Option<String>,
    pub slug: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GhPrAuthor {
    pub login: String,
    pub avatar_url: Option<String>,
}

pub fn avatar_url(login: &str) -> String {
    format!("https://{}/{login}", gitlab_host())
}

#[cfg(test)]
mod tests;

pub(crate) use review_requests::{restore_review_requests, snapshot_review_requests, ReviewRequestSnapshot};
