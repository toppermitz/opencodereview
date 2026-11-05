use super::*;

/// URL-encoded project path for GitLab REST (`group/sub/project` → `group%2Fsub%2Fproject`).
pub(crate) fn project_api_id(full_name: &str) -> String {
    full_name
        .split('/')
        .map(|segment| {
            let mut out = String::new();
            for b in segment.bytes() {
                match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
                    _ => out.push_str(&format!("%{b:02X}")),
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("%2F")
}

/// A writable glab config directory (cached for the process), or None to use glab's default.
pub(crate) fn glab_config_dir() -> Option<PathBuf> {
    GLAB_CONFIG_DIR.get_or_init(prepare_glab_config_dir).clone()
}

pub(crate) fn prepare_glab_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("GLAB_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    let source = std::env::var_os("XDG_CONFIG_HOME")
        .map(|base| PathBuf::from(base).join("glab-cli"))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config").join("glab-cli")))?;
    if !source.join("config.yml").is_file() {
        return None;
    }
    if dir_is_writable(&source) {
        return None;
    }
    let target = glab_config_cache_dir();
    if create_private_dir(&target).is_err() {
        return None;
    }
    let mut copied = false;
    for name in ["config.yml", "aliases.yml"] {
        let src = source.join(name);
        if src.is_file() && std::fs::copy(&src, target.join(name)).is_ok() {
            copied = true;
        }
    }
    copied.then_some(target)
}

pub(crate) fn glab_config_cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME").or_else(|| std::env::var_os("XDG_CACHE_HOME")) {
        return PathBuf::from(dir).join("opencodereview").join("glab-config");
    }
    std::env::temp_dir().join(format!("ocr-glab-config-{}", std::process::id()))
}

pub(crate) fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)?;
    }
    Ok(())
}

pub(crate) fn dir_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".ocr-write-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

pub(crate) fn apply_glab_config_dir(command: &mut Command) {
    if let Some(dir) = glab_config_dir() {
        command.env("GLAB_CONFIG_DIR", dir);
    }
}

/// Resolved GitLab host for every `glab`/git operation (cached for the process).
/// Precedence: `OCR_GITLAB_HOST`, then `GITLAB_HOST`. There is no default — Self-Hosted
/// must be configured. An empty string means the host is missing or invalid.
pub fn gitlab_host() -> String {
    GL_HOST.get_or_init(detect_gitlab_host).clone()
}

pub(crate) fn detect_gitlab_host() -> String {
    for key in ["OCR_GITLAB_HOST", "GITLAB_HOST"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if is_valid_gitlab_host(value) {
                return value.to_string();
            }
        }
    }
    String::new()
}

pub(crate) fn require_gitlab_host() -> Result<String, String> {
    let host = gitlab_host();
    if host.is_empty() {
        return Err("OCR_GITLAB_HOST is required (bare host or host:port for your GitLab Self-Hosted instance)".into());
    }
    Ok(host)
}

/// A bare `host` or `host:port` suitable for interpolation into a git config key
/// and clone URL. Rejects schemes, paths, credentials, and any whitespace.
pub(crate) fn is_valid_gitlab_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let (name, port) = match host.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (host, None),
    };
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    if name.is_empty() {
        return false;
    }
    name.split('.').all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

pub(crate) fn apply_gitlab_host(command: &mut Command) -> Result<(), String> {
    let host = require_gitlab_host()?;
    apply_glab_config_dir(command);
    command.env("GITLAB_HOST", &host);
    // Dev fallback first; otherwise the OAuth device-flow token for Self-Hosted.
    if let Ok(token) = std::env::var("GITLAB_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            command.env("GITLAB_TOKEN", token);
            return Ok(());
        }
    }
    if let Some(token) = crate::gitlab::oauth::resolve_oauth_access_token() {
        // glab 1.128 still sends OAUTH_TOKEN as Private-Token unless told it is OAuth2.
        command.env("OAUTH_TOKEN", &token);
        command.env("GLAB_IS_OAUTH2", "true");
    }
    Ok(())
}

pub(crate) fn clear_token_cache() {
    // OAuth resolution always reads the store (and refreshes when needed); nothing to drop.
}

pub(crate) fn git_token() -> Option<String> {
    // Always resolve so an expired access token is refreshed before git clone/fetch.
    resolve_git_token()
}

pub(crate) fn resolve_git_token() -> Option<String> {
    crate::gitlab::oauth::resolve_oauth_access_token()
}

/// Configure a `git` command to authenticate HTTPS requests with a GitLab token
/// via `http.https://<host>/.extraheader` using Basic `oauth2:<token>`.
pub(crate) fn apply_git_https_auth(command: &mut Command) {
    let Ok(host) = require_gitlab_host() else {
        return;
    };
    if let Some(token) = git_token() {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", format!("http.https://{host}/.extraheader"))
            .env(
                "GIT_CONFIG_VALUE_0",
                format!("AUTHORIZATION: basic {}", basic_auth_token(&token)),
            );
    }
}

/// base64 of `oauth2:<token>` for an HTTP Basic `Authorization` header.
pub(crate) fn basic_auth_token(token: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = format!("oauth2:{token}").into_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = bytes.get(i + 1).copied().unwrap_or(0);
        let b2 = bytes.get(i + 2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b11) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if i + 1 < bytes.len() {
            TABLE[(((b1 & 0b1111) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if i + 2 < bytes.len() {
            TABLE[(b2 & 0b111111) as usize] as char
        } else {
            '='
        });
        i += 3;
    }
    out
}

/// Authenticated `glab` user username (cached).
pub fn current_login() -> Option<String> {
    if let Some(login) = GL_LOGIN.lock().ok().and_then(|cached| cached.clone()) {
        return Some(login);
    }
    let login = run_raw("glab", &["api", "user"])
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("username")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
        })
        .filter(|value| !value.is_empty());
    if let (Some(value), Ok(mut cached)) = (&login, GL_LOGIN.lock()) {
        *cached = Some(value.clone());
    }
    login
}

pub(crate) fn forget_identity() {
    if let Ok(mut cached) = GL_LOGIN.lock() {
        *cached = None;
    }
    if let Ok(mut cached) = GL_TEAM_SLUGS.lock() {
        *cached = None;
    }
}

pub(crate) fn sign_out_result(success: bool, stderr: &str) -> Result<(), String> {
    if success || stderr.to_ascii_lowercase().contains("not logged in") {
        return Ok(());
    }
    let detail = stderr
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("glab auth logout failed");
    Err(format!("Could not sign out of GitLab: {detail}"))
}

pub fn sign_out() -> Result<(), String> {
    if std::env::var_os("GITLAB_TOKEN").is_some() {
        return Err("GitLab is connected through GITLAB_TOKEN in the environment. Remove it from the OpenCodeReview configuration to sign out.".into());
    }
    let path = crate::gitlab::oauth::oauth_store_path();
    crate::gitlab::oauth::clear_stored(&path);
    clear_token_cache();
    // Capture any legacy glab session before clearing identity caches, then always
    // log it out too — upgrades may still have credentials in glab's config.
    let host = require_gitlab_host()?;
    let login = current_login();
    forget_identity();
    let mut command = Command::new("glab");
    command.args(["auth", "logout", "--hostname", &host]);
    if let Some(login) = &login {
        command.args(["--user", login]);
    }
    apply_glab_config_dir(&mut command);
    let _ = command.env("GITLAB_HOST", &host);
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not run glab: {error}. Is the GitLab CLI installed?"))?;
    forget_identity();
    clear_token_cache();
    sign_out_result(output.status.success(), &String::from_utf8_lossy(&output.stderr))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLabAuthStatus {
    pub logged_in: bool,
    pub login: Option<String>,
    pub host: String,
    pub detail: String,
}

pub fn auth_status() -> GitLabAuthStatus {
    let host = gitlab_host();
    if host.is_empty() {
        return GitLabAuthStatus {
            logged_in: false,
            login: None,
            host,
            detail: "OCR_GITLAB_HOST is required (bare host or host:port for your GitLab Self-Hosted instance)".into(),
        };
    }
    let via_env = std::env::var_os("GITLAB_TOKEN").is_some();
    let via_oauth = crate::gitlab::oauth::oauth_store_path().is_file();
    if let Some(login) = current_login() {
        let detail = if via_env {
            "glab authenticated via GITLAB_TOKEN"
        } else if via_oauth {
            "authenticated via OAuth device flow"
        } else {
            "glab authenticated"
        };
        return GitLabAuthStatus {
            logged_in: true,
            login: Some(login),
            host,
            detail: detail.to_string(),
        };
    }
    if via_oauth || via_env {
        return GitLabAuthStatus {
            logged_in: false,
            login: None,
            host,
            detail: "GitLab credentials are present but the API user could not be read. Connect again.".into(),
        };
    }
    let detail = if std::env::var_os("OCR_GITLAB_CLIENT_ID").map(|v| !v.is_empty()).unwrap_or(false) {
        "Not connected. Use Connect GitLab to authorize with a device code.".into()
    } else {
        "Not connected. Set OCR_GITLAB_CLIENT_ID to your GitLab OAuth Application ID, then Connect GitLab.".into()
    };
    GitLabAuthStatus {
        logged_in: false,
        login: None,
        host,
        detail,
    }
}

/// Drive GitLab's OAuth device authorization grant from the GUI, then store the
/// access token for `glab` (`OAUTH_TOKEN`). See `oauth::run_device_flow`.
pub fn run_auth_flow(cancel: &AtomicBool, action: &str, emit: &mut dyn FnMut(serde_json::Value)) {
    let _ = action;
    crate::gitlab::oauth::run_device_flow(cancel, emit);
}
