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
    if let Ok(token) = std::env::var("GITLAB_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            command.env("GITLAB_TOKEN", token);
        }
    }
    Ok(())
}

pub(crate) fn git_token() -> Option<String> {
    if let Some(token) = GIT_TOKEN.get() {
        return Some(token.clone());
    }
    let token = resolve_git_token();
    if let Some(token) = &token {
        let _ = GIT_TOKEN.set(token.clone());
    }
    token
}

pub(crate) fn resolve_git_token() -> Option<String> {
    if let Ok(token) = std::env::var("GITLAB_TOKEN") {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    let host = require_gitlab_host().ok()?;
    let mut token_cmd = Command::new("glab");
    token_cmd
        .args(["auth", "token", "--hostname", &host])
        .stdin(Stdio::null());
    apply_glab_config_dir(&mut token_cmd);
    let _ = token_cmd.env("GITLAB_HOST", &host);
    let output = token_cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!token.is_empty()).then_some(token)
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
    let host = require_gitlab_host()?;
    let login = current_login();
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
    if let Some(login) = current_login() {
        let detail = if std::env::var_os("GITLAB_TOKEN").is_some() {
            "glab authenticated via GITLAB_TOKEN"
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
    let mut status_cmd = Command::new("glab");
    status_cmd.args(["auth", "status", "--hostname", &host]);
    apply_glab_config_dir(&mut status_cmd);
    let _ = status_cmd.env("GITLAB_HOST", &host);
    match status_cmd.output() {
        Ok(output) => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let logged_in = output.status.success()
                && (text.to_ascii_lowercase().contains("logged in")
                    || text.to_ascii_lowercase().contains("authenticated"));
            let detail = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .take(6)
                .collect::<Vec<_>>()
                .join("\n");
            GitLabAuthStatus {
                logged_in,
                login: parse_glab_login(&text),
                host,
                detail,
            }
        }
        Err(error) => GitLabAuthStatus {
            logged_in: false,
            login: None,
            host,
            detail: format!("Could not run glab: {error}. Is the GitLab CLI installed?"),
        },
    }
}

pub(crate) fn parse_glab_login(text: &str) -> Option<String> {
    for marker in ["as ", "account "] {
        for line in text.lines() {
            if let Some(idx) = line.to_ascii_lowercase().find(marker) {
                let rest = &line[idx + marker.len()..];
                let login: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '[' | ']'))
                    .collect();
                if !login.is_empty() {
                    return Some(login.trim_matches(|c| c == '[' || c == ']').to_string());
                }
            }
        }
    }
    None
}

pub(crate) fn find_device_code(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|token| {
        let tok = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        let parts: Vec<&str> = tok.split('-').collect();
        let looks_like_code = parts.len() == 2
            && parts
                .iter()
                .all(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
        looks_like_code.then(|| tok.to_string())
    })
}

pub(crate) fn find_url(line: &str) -> Option<String> {
    line.split_whitespace().find(|w| w.starts_with("https://")).map(|w| {
        w.trim_matches(|c: char| {
            !(c.is_ascii_alphanumeric() || matches!(c, '/' | ':' | '.' | '-' | '_' | '?' | '=' | '&'))
        })
        .to_string()
    })
}

/// Drive `glab auth login --device` (OAuth device flow) from the GUI.
/// Self-Hosted needs an explicit API host/protocol; without `--device`/`--web`
/// glab waits on an interactive login-type prompt that never arrives over pipes.
pub fn run_auth_flow(cancel: &AtomicBool, action: &str, emit: &mut dyn FnMut(serde_json::Value)) {
    let host = match require_gitlab_host() {
        Ok(host) => host,
        Err(message) => {
            emit(serde_json::json!({ "kind": "done", "success": false, "message": message }));
            return;
        }
    };
    let _ = action;
    let args = [
        "auth",
        "login",
        "--hostname",
        host.as_str(),
        "--api-host",
        host.as_str(),
        "--api-protocol",
        "https",
        "--git-protocol",
        "https",
        "--device",
        "--insecure-storage",
    ];
    let mut auth_cmd = Command::new("glab");
    auth_cmd
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_glab_config_dir(&mut auth_cmd);
    let _ = auth_cmd.env("GITLAB_HOST", &host);
    let mut child = match auth_cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            emit(
                serde_json::json!({ "kind": "done", "success": false, "message": format!("Could not start glab: {error}. Is the GitLab CLI installed?") }),
            );
            return;
        }
    };
    let pid = child.id();
    let mut stdin = child.stdin.take();
    let (tx, rx) = mpsc::channel::<String>();
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let started = Instant::now();
    let (mut code_sent, mut url_sent) = (false, false);
    loop {
        if cancel.load(Ordering::Relaxed) {
            crate::review::kill_tree(pid, &mut child);
            emit(serde_json::json!({ "kind": "done", "success": false, "message": "Cancelled." }));
            return;
        }
        if started.elapsed() > Duration::from_secs(300) {
            crate::review::kill_tree(pid, &mut child);
            emit(
                serde_json::json!({ "kind": "done", "success": false, "message": "Timed out. Run `glab auth login` in a terminal to finish." }),
            );
            return;
        }
        while let Ok(line) = rx.try_recv() {
            if !code_sent {
                if let Some(code) = find_device_code(&line) {
                    code_sent = true;
                    emit(serde_json::json!({ "kind": "prompt", "code": code }));
                    if let Some(mut handle) = stdin.take() {
                        let _ = handle.write_all(b"\n");
                        let _ = handle.flush();
                    }
                }
            }
            if !url_sent {
                if let Some(url) = find_url(&line) {
                    url_sent = true;
                    emit(serde_json::json!({ "kind": "prompt", "url": url }));
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                while let Ok(line) = rx.try_recv() {
                    if !url_sent {
                        if let Some(url) = find_url(&line) {
                            url_sent = true;
                            emit(serde_json::json!({ "kind": "prompt", "url": url }));
                        }
                    }
                }
                emit(serde_json::json!({
                  "kind": "done",
                  "success": status.success(),
                  "message": if status.success() { "Signed in to GitLab.".to_string() } else { "glab didn't finish. If no code appeared, run `glab auth login` in a terminal.".to_string() },
                }));
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(300)),
            Err(error) => {
                emit(serde_json::json!({ "kind": "done", "success": false, "message": error.to_string() }));
                return;
            }
        }
    }
}
