//! GitLab OAuth 2.0 device authorization grant (Self-Hosted).
//! Tokens are stored under `$OCR_DATA_DIR/gitlab/oauth.json` and passed to `glab` as `OAUTH_TOKEN`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::auth::{create_private_dir, require_gitlab_host};

pub(crate) const DEVICE_SCOPES: &str = "api read_api read_user write_repository openid profile";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const NEAR_EXPIRY_SECS: u64 = 120;

/// Serializes load / refresh / save / clear of `oauth.json` across concurrent GitLab callers.
static OAUTH_STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredOAuth {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Host that issued the tokens; empty in pre-binding stores (treated as a mismatch).
    #[serde(default)]
    pub host: String,
    /// OAuth Application ID that issued the tokens.
    #[serde(default)]
    pub client_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DeviceAuthorization {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_device_expires")]
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_device_expires() -> u64 {
    300
}
fn default_interval() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
pub(crate) struct TokenSuccess {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TokenError {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

pub(crate) fn gitlab_client_id() -> Result<String, String> {
    let id = std::env::var("OCR_GITLAB_CLIENT_ID")
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    if id.is_empty() {
        return Err(
            "OCR_GITLAB_CLIENT_ID is required. Create a non-confidential OAuth application with Device authorization on your GitLab instance and set its Application ID."
                .into(),
        );
    }
    Ok(id)
}

pub(crate) fn oauth_store_path() -> PathBuf {
    let root = std::env::var_os("OCR_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"));
    root.join("gitlab").join("oauth.json")
}

pub(crate) fn https_base(host: &str) -> String {
    format!("https://{host}")
}

pub(crate) fn verification_url(auth: &DeviceAuthorization) -> String {
    auth.verification_uri_complete
        .as_deref()
        .filter(|u| !u.is_empty())
        .unwrap_or(auth.verification_uri.as_str())
        .to_string()
}

pub(crate) fn stored_from_token(token: &TokenSuccess, host: &str, client_id: &str) -> StoredOAuth {
    let expires_at = token.expires_in.map(|secs| {
        let base = token.created_at.unwrap_or_else(now_epoch);
        base.saturating_add(secs)
    });
    StoredOAuth {
        access_token: token.access_token.clone(),
        refresh_token: token.refresh_token.clone(),
        expires_at,
        scope: token.scope.clone(),
        host: host.to_string(),
        client_id: client_id.to_string(),
    }
}

fn store_matches_install(stored: &StoredOAuth, host: &str, client_id: &str) -> bool {
    !stored.host.is_empty() && !stored.client_id.is_empty() && stored.host == host && stored.client_id == client_id
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn is_expired(stored: &StoredOAuth) -> bool {
    match stored.expires_at {
        Some(at) => now_epoch() + NEAR_EXPIRY_SECS >= at,
        None => false,
    }
}

fn is_hard_expired(stored: &StoredOAuth) -> bool {
    match stored.expires_at {
        Some(at) => now_epoch() >= at,
        None => false,
    }
}

pub(crate) fn load_stored(path: &Path) -> Option<StoredOAuth> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(crate) fn save_stored(path: &Path, stored: &StoredOAuth) -> Result<(), String> {
    let _guard = OAUTH_STORE_LOCK
        .lock()
        .map_err(|_| "GitLab OAuth store lock poisoned".to_string())?;
    save_stored_unlocked(path, stored)
}

fn save_stored_unlocked(path: &Path, stored: &StoredOAuth) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        create_private_dir(parent).map_err(|e| format!("Could not create GitLab credential directory: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(stored).map_err(|e| e.to_string())?;
    write_private_file(path, &json).map_err(|e| format!("Could not store GitLab OAuth token: {e}"))
}

pub(crate) fn clear_stored(path: &Path) {
    let _guard = OAUTH_STORE_LOCK.lock();
    clear_stored_unlocked(path);
}

fn clear_stored_unlocked(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Could not build HTTP client: {e}"))
}

pub(crate) fn authorize_device(host: &str, client_id: &str) -> Result<DeviceAuthorization, String> {
    let client = http_client()?;
    let url = format!("{}/oauth/authorize_device", https_base(host));
    let response = client
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "client_id={}&scope={}",
            urlencoding(client_id),
            urlencoding(DEVICE_SCOPES)
        ))
        .send()
        .map_err(|e| format!("Could not reach GitLab device authorization: {e}"))?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(oauth_http_error("authorize device", status.as_u16(), &body));
    }
    serde_json::from_str(&body).map_err(|e| format!("Invalid authorize_device response: {e}"))
}

pub(crate) fn poll_device_token(
    host: &str,
    client_id: &str,
    device_code: &str,
) -> Result<Result<TokenSuccess, TokenError>, String> {
    let client = http_client()?;
    let url = format!("{}/oauth/token", https_base(host));
    let response = client
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type={}&device_code={}&client_id={}",
            urlencoding(DEVICE_GRANT),
            urlencoding(device_code),
            urlencoding(client_id)
        ))
        .send()
        .map_err(|e| format!("Could not poll GitLab token endpoint: {e}"))?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if let Ok(token) = serde_json::from_str::<TokenSuccess>(&body) {
        if !token.access_token.is_empty() {
            return Ok(Ok(token));
        }
    }
    if let Ok(err) = serde_json::from_str::<TokenError>(&body) {
        return Ok(Err(err));
    }
    Err(oauth_http_error("token poll", status.as_u16(), &body))
}

pub(crate) fn refresh_access_token(host: &str, client_id: &str, refresh_token: &str) -> Result<TokenSuccess, String> {
    let client = http_client()?;
    let url = format!("{}/oauth/token", https_base(host));
    let response = client
        .post(&url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=refresh_token&client_id={}&refresh_token={}",
            urlencoding(client_id),
            urlencoding(refresh_token)
        ))
        .send()
        .map_err(|e| format!("Could not refresh GitLab OAuth token: {e}"))?;
    let status = response.status();
    let body = response.text().map_err(|e| e.to_string())?;
    if let Ok(token) = serde_json::from_str::<TokenSuccess>(&body) {
        if !token.access_token.is_empty() {
            return Ok(token);
        }
    }
    if let Ok(err) = serde_json::from_str::<TokenError>(&body) {
        return Err(err.error_description.unwrap_or(err.error).trim().to_string());
    }
    Err(oauth_http_error("token refresh", status.as_u16(), &body))
}

fn oauth_http_error(what: &str, status: u16, body: &str) -> String {
    // Structured OAuth errors are safe to surface; never echo a raw body that may hold tokens.
    if let Ok(err) = serde_json::from_str::<TokenError>(body) {
        return err.error_description.filter(|d| !d.is_empty()).unwrap_or(err.error);
    }
    format!("GitLab {what} failed (HTTP {status})")
}

fn is_definitive_auth_failure(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("invalid_grant")
        || message.contains("invalid_token")
        || message.contains("revoked")
        || message.contains("expired") && message.contains("refresh")
}

fn urlencoding(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Run the device grant until the user approves, cancels, or the device code expires.
pub fn run_device_flow(cancel: &AtomicBool, emit: &mut dyn FnMut(serde_json::Value)) {
    let host = match require_gitlab_host() {
        Ok(host) => host,
        Err(message) => {
            emit(serde_json::json!({ "kind": "done", "success": false, "message": message }));
            return;
        }
    };
    let client_id = match gitlab_client_id() {
        Ok(id) => id,
        Err(message) => {
            emit(serde_json::json!({ "kind": "done", "success": false, "message": message }));
            return;
        }
    };
    let auth = match authorize_device(&host, &client_id) {
        Ok(auth) => auth,
        Err(message) => {
            emit(serde_json::json!({ "kind": "done", "success": false, "message": message }));
            return;
        }
    };
    emit(serde_json::json!({
        "kind": "prompt",
        "code": auth.user_code,
        "url": verification_url(&auth),
    }));

    let deadline = Instant::now() + Duration::from_secs(auth.expires_in.max(1));
    let mut interval = Duration::from_secs(auth.interval.max(1));
    loop {
        if cancel.load(Ordering::Relaxed) {
            emit(serde_json::json!({ "kind": "done", "success": false, "message": "Cancelled." }));
            return;
        }
        if Instant::now() >= deadline {
            emit(serde_json::json!({
                "kind": "done",
                "success": false,
                "message": "The device code expired. Start Connect GitLab again."
            }));
            return;
        }
        match poll_device_token(&host, &client_id, &auth.device_code) {
            Ok(Ok(token)) => {
                if cancel.load(Ordering::Relaxed) {
                    emit(serde_json::json!({ "kind": "done", "success": false, "message": "Cancelled." }));
                    return;
                }
                let stored = stored_from_token(&token, &host, &client_id);
                if let Err(message) = save_stored(&oauth_store_path(), &stored) {
                    emit(serde_json::json!({ "kind": "done", "success": false, "message": message }));
                    return;
                }
                super::auth::forget_identity();
                super::auth::clear_token_cache();
                emit(serde_json::json!({ "kind": "done", "success": true, "message": "Signed in to GitLab." }));
                return;
            }
            Ok(Err(err)) => match err.error.as_str() {
                "authorization_pending" => {}
                "slow_down" => {
                    interval = interval.saturating_add(Duration::from_secs(5));
                }
                "expired_token" => {
                    emit(serde_json::json!({
                        "kind": "done",
                        "success": false,
                        "message": err.error_description.unwrap_or_else(|| "The device code expired. Start Connect GitLab again.".into())
                    }));
                    return;
                }
                "access_denied" => {
                    emit(serde_json::json!({
                        "kind": "done",
                        "success": false,
                        "message": err.error_description.unwrap_or_else(|| "GitLab authorization was denied.".into())
                    }));
                    return;
                }
                other => {
                    let message = err
                        .error_description
                        .filter(|d| !d.is_empty())
                        .unwrap_or_else(|| other.to_string());
                    emit(serde_json::json!({ "kind": "done", "success": false, "message": message }));
                    return;
                }
            },
            Err(message) => {
                emit(serde_json::json!({ "kind": "done", "success": false, "message": message }));
                return;
            }
        }
        let sleep_for = interval.min(deadline.saturating_duration_since(Instant::now()));
        if sleep_for.is_zero() {
            continue;
        }
        let end = Instant::now() + sleep_for;
        while Instant::now() < end {
            if cancel.load(Ordering::Relaxed) {
                emit(serde_json::json!({ "kind": "done", "success": false, "message": "Cancelled." }));
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

/// Access token for `glab` / git: env `GITLAB_TOKEN`, else stored OAuth (refreshing when possible).
pub(crate) fn resolve_oauth_access_token() -> Option<String> {
    if let Ok(token) = std::env::var("GITLAB_TOKEN") {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    let path = oauth_store_path();
    let host = require_gitlab_host().ok()?;
    let client_id = gitlab_client_id().ok()?;
    let _guard = OAUTH_STORE_LOCK.lock().ok()?;
    // Reload under the lock so concurrent callers share one refresh.
    let mut stored = load_stored(&path)?;
    if !store_matches_install(&stored, &host, &client_id) {
        clear_stored_unlocked(&path);
        return None;
    }
    if is_expired(&stored) {
        match &stored.refresh_token {
            Some(refresh) => match refresh_access_token(&host, &client_id, refresh) {
                Ok(token) => {
                    stored = stored_from_token(&token, &host, &client_id);
                    if save_stored_unlocked(&path, &stored).is_err() {
                        clear_stored_unlocked(&path);
                        return None;
                    }
                }
                Err(error) if is_definitive_auth_failure(&error) => {
                    clear_stored_unlocked(&path);
                    return None;
                }
                Err(_) if is_hard_expired(&stored) => return None,
                Err(_) => {} // Keep the store; the access token may still work until hard expiry.
            },
            None if is_hard_expired(&stored) => {
                clear_stored_unlocked(&path);
                return None;
            }
            None => {} // Near expiry without refresh: keep using until hard expiry.
        }
    }
    Some(stored.access_token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_verification_uri_complete() {
        let auth = DeviceAuthorization {
            device_code: "d".into(),
            user_code: "0A44L90H".into(),
            verification_uri: "https://gitlab.example.com/oauth/device".into(),
            verification_uri_complete: Some("https://gitlab.example.com/oauth/device?user_code=0A44L90H".into()),
            expires_in: 300,
            interval: 5,
        };
        assert!(verification_url(&auth).contains("user_code=0A44L90H"));
    }

    #[test]
    fn parses_device_authorization_json() {
        let raw = r#"{
            "device_code":"GmRhmhcxhwAzkoEqiMEg_DnyEysNkuNhszIySk9eS",
            "user_code":"0A44L90H",
            "verification_uri":"https://gitlab.example.com/oauth/device",
            "verification_uri_complete":"https://gitlab.example.com/oauth/device?user_code=0A44L90H",
            "expires_in":300,
            "interval":5
        }"#;
        let auth: DeviceAuthorization = serde_json::from_str(raw).unwrap();
        assert_eq!(auth.user_code, "0A44L90H");
        assert_eq!(auth.interval, 5);
    }

    #[test]
    fn parses_token_success_without_refresh() {
        let raw = r#"{
            "access_token":"TOKEN",
            "token_type":"Bearer",
            "expires_in":7200,
            "scope":"api",
            "created_at":1593096829
        }"#;
        let token: TokenSuccess = serde_json::from_str(raw).unwrap();
        let stored = stored_from_token(&token, "gitlab.example.com", "app-id");
        assert_eq!(stored.access_token, "TOKEN");
        assert!(stored.refresh_token.is_none());
        assert_eq!(stored.expires_at, Some(1593096829 + 7200));
        assert_eq!(stored.host, "gitlab.example.com");
        assert_eq!(stored.client_id, "app-id");
    }

    #[test]
    fn oauth_http_error_keeps_structured_errors_not_raw_bodies() {
        let structured = r#"{"error":"access_denied","error_description":"The resource owner denied"}"#;
        assert_eq!(
            oauth_http_error("token poll", 400, structured),
            "The resource owner denied"
        );
        assert_eq!(
            oauth_http_error("token poll", 500, r#"{"access_token":"SECRET","token_type":"Bearer"}"#),
            "GitLab token poll failed (HTTP 500)"
        );
    }

    #[test]
    fn rejects_stores_from_another_host_or_client() {
        let stored = StoredOAuth {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: None,
            scope: None,
            host: "old.example.com".into(),
            client_id: "old-app".into(),
        };
        assert!(!store_matches_install(&stored, "new.example.com", "old-app"));
        assert!(!store_matches_install(&stored, "old.example.com", "new-app"));
        assert!(store_matches_install(&stored, "old.example.com", "old-app"));
        let legacy = StoredOAuth {
            host: String::new(),
            client_id: String::new(),
            ..stored.clone()
        };
        assert!(!store_matches_install(&legacy, "old.example.com", "old-app"));
    }

    #[test]
    fn classifies_pending_and_denied_errors() {
        for (error, _) in [
            ("authorization_pending", true),
            ("slow_down", true),
            ("expired_token", false),
            ("access_denied", false),
        ] {
            let err: TokenError =
                serde_json::from_str(&format!(r#"{{"error":"{error}","error_description":"…"}}"#)).unwrap();
            assert_eq!(err.error, error);
        }
    }

    #[test]
    fn store_round_trip_keeps_tokens() {
        let dir = std::env::temp_dir().join(format!("ocr-oauth-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("oauth.json");
        let stored = StoredOAuth {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at: Some(9_999_999_999),
            scope: Some("api".into()),
            host: "gitlab.example.com".into(),
            client_id: "app-id".into(),
        };
        save_stored(&path, &stored).unwrap();
        let loaded = load_stored(&path).unwrap();
        assert_eq!(loaded.access_token, "access");
        assert_eq!(loaded.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(loaded.host, "gitlab.example.com");
        assert_eq!(loaded.client_id, "app-id");
        clear_stored(&path);
        assert!(load_stored(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expired_without_leeway_when_past_expires_at() {
        let stored = StoredOAuth {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: Some(1),
            scope: None,
            host: "gitlab.example.com".into(),
            client_id: "app".into(),
        };
        assert!(is_expired(&stored));
        assert!(is_hard_expired(&stored));
        let fresh = StoredOAuth {
            access_token: "t".into(),
            refresh_token: None,
            expires_at: Some(now_epoch() + 10_000),
            scope: None,
            host: "gitlab.example.com".into(),
            client_id: "app".into(),
        };
        assert!(!is_expired(&fresh));
        assert!(!is_hard_expired(&fresh));
    }

    #[test]
    fn definitive_auth_failures_are_recognized() {
        assert!(is_definitive_auth_failure("invalid_grant"));
        assert!(is_definitive_auth_failure("The refresh token was revoked"));
        assert!(!is_definitive_auth_failure("connection timed out"));
        assert!(!is_definitive_auth_failure("502 Bad Gateway"));
    }
}
