//! Grok Build CLI execution for review stages that run on a Grok account.
use serde_json::Value;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::providers::claude::Failure;
use crate::providers::provider::{DiscoveredModel, ProviderAuth};

/// Reasoning efforts Grok accepts when a catalogue entry omits levels.
const DEFAULT_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

/// Offline fallback when the account cannot reach `grok models`.
const FALLBACK_MODELS: &[(&str, &str)] = &[
    ("grok-4.7", "Grok 4.7"),
    ("grok-4.7-build-fast", "Grok 4.7 Build Fast"),
    ("grok-4.6", "Grok 4.6"),
    ("grok-4.5", "Grok 4.5"),
];

/// `auth.json` written by `grok login`; anything larger is not a Grok credential.
pub(crate) const MAX_AUTH_BYTES: usize = 64 * 1024;
pub(crate) const MAX_API_KEY_BYTES: usize = 4096;

/// Built-in tools every isolated stage may call.
pub(crate) const RUNNER_TOOLS: &str = "Read,Grep,Glob,Bash,Write";

/// Tools that reach the network, spawn agents, or load MCP.
const DISALLOWED_TOOLS: &str = "Agent,Task,WebFetch,WebSearch,mcp__*";

/// Arguments for one non-interactive execution. The prompt is the `-p` value; stdout is
/// streaming-json. The container is the sandbox (egress-limited, unprivileged UID).
pub(crate) fn print_args(workdir: &Path, prompt: &str, model: &str, effort: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--no-auto-update",
        "--no-alt-screen",
        "--always-approve",
        "--no-subagents",
        "--no-memory",
        "--disable-web-search",
        "--output-format",
        "streaming-json",
        "--cwd",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    args.push(workdir.as_os_str().to_owned());
    args.extend([
        OsString::from("--tools"),
        OsString::from(RUNNER_TOOLS),
        OsString::from("--disallowed-tools"),
        OsString::from(DISALLOWED_TOOLS),
        OsString::from("-m"),
        OsString::from(model),
        OsString::from("--effort"),
        OsString::from(effort),
        OsString::from("-p"),
        OsString::from(prompt),
    ]);
    args
}

/// True when `value` is one Grok auth entry (or a legacy flat shape) with a usable credential.
fn entry_has_credential(value: &Value) -> bool {
    [
        value["key"].as_str(),
        value["access_token"].as_str(),
        value["refresh_token"].as_str(),
        value["tokens"]["access_token"].as_str(),
        value["tokens"]["refresh_token"].as_str(),
        value["session"]["access_token"].as_str(),
        value["session"]["refresh_token"].as_str(),
    ]
    .iter()
    .any(|token| token.is_some_and(|value| !value.is_empty()))
}

/// Checks that `raw` is a Grok `auth.json` from `grok login`.
///
/// Real files are a map of OIDC scopes to entries with `key` (access JWT) and optional
/// `refresh_token`, e.g. `{"https://auth.x.ai::<client>":{"auth_mode":"oidc","key":"…"}}`.
pub(crate) fn validate_auth(raw: &str) -> Result<(), String> {
    if raw.len() > MAX_AUTH_BYTES {
        return Err("Grok authentication is too large".into());
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| "Grok authentication is not valid JSON")?;
    let has_sign_in = entry_has_credential(&value)
        || value
            .as_object()
            .is_some_and(|map| map.values().any(entry_has_credential));
    if !has_sign_in {
        return Err("Grok authentication has no sign-in; sign in again".into());
    }
    Ok(())
}

/// Email or handle from a signed-in `auth.json`, to name the account.
pub(crate) fn account_email(raw: &str) -> Option<String> {
    use base64::Engine;
    let value: Value = serde_json::from_str(raw).ok()?;
    let entries: Vec<&Value> = match value.as_object() {
        Some(map) if map.values().any(entry_has_credential) => map.values().collect(),
        _ => vec![&value],
    };
    for entry in entries {
        if let Some(email) = entry["email"]
            .as_str()
            .or_else(|| entry["user_email"].as_str())
            .filter(|email| email.contains('@') && email.len() <= 80)
        {
            return Some(email.to_string());
        }
        for jwt in [
            entry["key"].as_str(),
            entry["access_token"].as_str(),
            entry["id_token"].as_str(),
        ]
        .into_iter()
        .flatten()
        {
            let Some(payload) = jwt.split('.').nth(1) else {
                continue;
            };
            let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('='))
            else {
                continue;
            };
            let Ok(claims) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if let Some(email) = claims["email"]
                .as_str()
                .or_else(|| claims["preferred_username"].as_str())
                .filter(|email| email.contains('@') && email.len() <= 80)
            {
                return Some(email.to_string());
            }
        }
    }
    None
}

/// Access JWT (`key`) from a signed-in `auth.json`, for plan usage and discovery.
pub(crate) fn access_token(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let entries: Vec<&Value> = match value.as_object() {
        Some(map) if map.values().any(entry_has_credential) => map.values().collect(),
        _ => vec![&value],
    };
    for entry in entries {
        for key in ["key", "access_token"] {
            if let Some(token) = entry[key]
                .as_str()
                .or_else(|| entry["tokens"][key].as_str())
                .or_else(|| entry["session"][key].as_str())
                .filter(|token| !token.is_empty())
            {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// Checks that `raw` looks like an xAI API key (`xai-…`).
pub(crate) fn validate_api_key(raw: &str) -> Result<(), String> {
    let key = raw.trim();
    if key.is_empty()
        || key.len() > MAX_API_KEY_BYTES
        || !key.starts_with("xai-")
        || !key.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err("That is not an xAI API key. Paste a key that starts with xai-".into());
    }
    Ok(())
}

pub(crate) fn read_api_key(path: &Path) -> Result<String, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|_| "Grok authentication is unavailable; sign in from Settings → Providers")?;
    validate_api_key(&raw)?;
    Ok(raw.trim().into())
}

/// Collects credential strings from one auth entry (scoped or flat).
fn entry_secrets(value: &Value, out: &mut Vec<String>) {
    for key in ["key", "access_token", "refresh_token", "id_token"] {
        for path in [
            value[key].as_str(),
            value["tokens"][key].as_str(),
            value["session"][key].as_str(),
        ] {
            if let Some(secret) = path.filter(|secret| secret.len() >= 16) {
                out.push(secret.to_string());
            }
        }
    }
}

/// Credential values in an `auth.json` or API key that must never appear in review output.
pub(crate) fn secrets(auth_json: Option<&str>, api_key: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(raw) = auth_json {
        if let Ok(value) = serde_json::from_str::<Value>(raw) {
            entry_secrets(&value, &mut out);
            // Scoped OIDC map: each value is an entry with `key` / `auth_mode`. Skip nested
            // bags like `tokens` that entry_secrets already walks from the root.
            if let Some(map) = value.as_object() {
                for entry in map.values() {
                    if entry.get("key").is_some() || entry.get("auth_mode").is_some() {
                        entry_secrets(entry, &mut out);
                    }
                }
            }
        }
    }
    if let Some(key) = api_key.filter(|key| key.len() >= 16) {
        out.push(key.to_string());
    }
    out
}

/// Why an execution failed, read only from Grok's own events and stderr.
pub(crate) fn classify(stdout: &str, stderr: &str) -> Failure {
    let mut text = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter_map(|event| {
            event["error"]["message"]
                .as_str()
                .or_else(|| event["message"].as_str())
                .or_else(|| event["result"].as_str())
                .map(str::to_string)
        })
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text.push_str(stderr);
    let lower = text.to_ascii_lowercase();
    if [
        "usage limit",
        "usage_limit",
        "hit your limit",
        "rate limit exceeded",
        "quota",
    ]
    .iter()
    .any(|word| lower.contains(word))
    {
        return Failure::UsageLimit { resets_at: None };
    }
    if [
        "401",
        "unauthorized",
        "invalid api key",
        "not logged in",
        "please log in",
        "sign in again",
        "authentication",
        "token_expired",
        "refresh token",
    ]
    .iter()
    .any(|word| lower.contains(word))
    {
        return Failure::Auth;
    }
    if ["429", "overloaded", "503", "server_is_overloaded"]
        .iter()
        .any(|word| lower.contains(word))
    {
        return Failure::Overloaded;
    }
    if lower.contains("model")
        && (lower.contains("not found") || lower.contains("not supported") || lower.contains("does not exist"))
    {
        return Failure::Model;
    }
    Failure::Other
}

/// Short, safe diagnostic for a failed process; raw CLI output stays private.
pub(crate) fn failure(stdout: &str, stderr: &str) -> String {
    match classify(stdout, stderr) {
        Failure::Other if stdout.trim().is_empty() && stderr.trim().is_empty() => {
            "Grok process exited without diagnostics; verify runner health and authentication".into()
        }
        Failure::Other => "Grok execution failed; verify authentication".into(),
        Failure::Overloaded => "Grok is temporarily overloaded; retry later".into(),
        Failure::Model => "Grok model is unavailable for this account".into(),
        failure => failure.encode(),
    }
}

/// Final assistant text of a streaming-json transcript.
pub(crate) fn final_result(transcript: &str) -> Result<String, String> {
    let events = transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect::<Vec<_>>();
    if let Some(text) = events.iter().rev().find_map(|event| {
        if event["type"] == "result" || event["type"] == "message" || event["kind"] == "result" {
            event["result"]
                .as_str()
                .or_else(|| event["text"].as_str())
                .or_else(|| event["content"].as_str())
                .or_else(|| event["message"].as_str())
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        } else {
            None
        }
    }) {
        return Ok(text);
    }
    // Accumulate streamed assistant chunks when the CLI never emits a single result event.
    let mut chunks = String::new();
    for event in &events {
        if let Some(text) = event["delta"]
            .as_str()
            .or_else(|| event["content"]["text"].as_str())
            .or_else(|| event["update"]["content"]["text"].as_str())
        {
            chunks.push_str(text);
        }
    }
    if !chunks.trim().is_empty() {
        return Ok(chunks);
    }
    Err(match classify(transcript, "") {
        Failure::Other => "Grok final response is missing: no result event".into(),
        failure => failure.encode(),
    })
}

/// Models available to the signed-in Grok account via `grok models`.
pub(crate) fn discover_models(auth: &ProviderAuth) -> Result<Vec<DiscoveredModel>, String> {
    let (auth_json, api_key) = match auth {
        ProviderAuth::Grok { auth_json, api_key } => (auth_json.as_deref(), api_key.as_deref()),
        ProviderAuth::None | ProviderAuth::Claude { .. } | ProviderAuth::Codex { .. } => (None, None),
    };
    match list_models(auth_json, api_key) {
        Ok(models) if !models.is_empty() => Ok(models),
        Ok(_) | Err(_) => Ok(fallback_models()),
    }
}

fn fallback_models() -> Vec<DiscoveredModel> {
    FALLBACK_MODELS
        .iter()
        .map(|(id, label)| DiscoveredModel {
            id: (*id).into(),
            label: (*label).into(),
            efforts: DEFAULT_EFFORTS.iter().map(|effort| (*effort).into()).collect(),
            default_effort: "medium".into(),
        })
        .collect()
}

fn list_models(auth_json: Option<&str>, api_key: Option<&str>) -> Result<Vec<DiscoveredModel>, String> {
    let home = std::env::temp_dir().join(format!("ocr-grok-models-{}", crate::sandbox::secret()?));
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
    }
    if let Some(raw) = auth_json {
        std::fs::write(home.join("auth.json"), raw).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(home.join("auth.json"), std::fs::Permissions::from_mode(0o600));
        }
    }
    // `grok models` prints a plain-text catalogue; there is no `--json` flag.
    let mut command = Command::new("grok");
    command
        .args(["models"])
        .env("GROK_HOME", &home)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("GROK_DISABLE_AUTOUPDATER", "1")
        .env_remove("XAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(key) = api_key {
        command.env("XAI_API_KEY", key);
    }
    if let Ok(proxy) = std::env::var("OCR_RUNNER_PROXY").or_else(|_| std::env::var("HTTPS_PROXY")) {
        if !proxy.is_empty() {
            command.env("HTTPS_PROXY", &proxy).env("HTTP_PROXY", &proxy);
        }
    }
    let output = command
        .output()
        .map_err(|_| "Grok model catalogue is unavailable right now")?;
    let _ = std::fs::remove_dir_all(&home);
    if !output.status.success() {
        return Err("Grok model catalogue is unavailable right now".into());
    }
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.stderr.is_empty() {
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    parse_models_output(&text)
}

/// Parses `grok models` output into picker rows (plain text today; JSON if a future build adds it).
pub(crate) fn parse_models_output(raw: &str) -> Result<Vec<DiscoveredModel>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Grok model catalogue is empty".into());
    }
    if let Ok(body) = serde_json::from_str::<Value>(trimmed) {
        return parse_models_json(&body);
    }
    // Some builds may print one JSON object per line.
    let json_lines = trimmed
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter_map(|value| parse_model(&value))
        .collect::<Vec<_>>();
    if !json_lines.is_empty() {
        return Ok(json_lines);
    }
    parse_models_text(trimmed)
}

/// Parses the plain-text catalogue printed by `grok models`:
///
/// ```text
/// Default model: grok-4.7
/// Available models:
///   * grok-4.7 (default)
///   - grok-4.7-build-fast
///   - grok-4.6
/// ```
fn parse_models_text(raw: &str) -> Result<Vec<DiscoveredModel>, String> {
    let mut default_id: Option<String> = None;
    let mut ids = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Default model:") {
            let id = rest.split_whitespace().next().unwrap_or("").to_string();
            if !id.is_empty() {
                default_id = Some(id);
            }
            continue;
        }
        let Some(id) = model_line_id(trimmed) else {
            continue;
        };
        if !ids.iter().any(|known| known == &id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Err("Grok returned an unreadable model catalogue".into());
    }
    if let Some(default) = default_id {
        if let Some(pos) = ids.iter().position(|id| *id == default) {
            let id = ids.remove(pos);
            ids.insert(0, id);
        } else {
            ids.insert(0, default);
        }
    }
    Ok(ids
        .into_iter()
        .map(|id| DiscoveredModel {
            label: humanize_model_id(&id),
            efforts: DEFAULT_EFFORTS.iter().map(|effort| (*effort).into()).collect(),
            default_effort: "medium".into(),
            id,
        })
        .collect())
}

fn model_line_id(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("* ")
        .or_else(|| line.strip_prefix("- "))
        .or_else(|| line.strip_prefix("• "))?;
    let id = rest.split_whitespace().next()?.trim();
    if id.is_empty()
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '.' || ch == '_')
    {
        return None;
    }
    Some(id.to_string())
}

fn humanize_model_id(id: &str) -> String {
    id.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            if part.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
                part.to_string()
            } else {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_models_json(body: &Value) -> Result<Vec<DiscoveredModel>, String> {
    let models = body
        .get("models")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())
        .ok_or("Grok model catalogue is missing models")?;
    Ok(models.iter().filter_map(parse_model).collect())
}

fn parse_model(value: &Value) -> Option<DiscoveredModel> {
    let id = value
        .get("id")
        .or_else(|| value.get("slug"))
        .or_else(|| value.get("model"))
        .and_then(Value::as_str)?
        .to_string();
    if id.is_empty() {
        return None;
    }
    let label = value
        .get("name")
        .or_else(|| value.get("display_name"))
        .or_else(|| value.get("label"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(&id)
        .to_string();
    let efforts = value
        .get("supported_reasoning_levels")
        .or_else(|| value.get("efforts"))
        .or_else(|| value.get("reasoning_efforts"))
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| {
                    level
                        .as_str()
                        .or_else(|| level.get("effort").and_then(Value::as_str))
                        .or_else(|| level.get("id").and_then(Value::as_str))
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .filter(|levels| !levels.is_empty())
        .unwrap_or_else(|| DEFAULT_EFFORTS.iter().map(|effort| (*effort).into()).collect());
    let default_effort = value
        .get("default_reasoning_level")
        .or_else(|| value.get("default_effort"))
        .and_then(Value::as_str)
        .filter(|effort| efforts.iter().any(|known| known == *effort))
        .unwrap_or_else(|| {
            if efforts.iter().any(|effort| effort == "medium") {
                "medium"
            } else {
                efforts.first().map(String::as_str).unwrap_or("medium")
            }
        })
        .to_string();
    Some(DiscoveredModel {
        id,
        label,
        efforts,
        default_effort,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_args_pin_headless_sandbox_flags() {
        let args = print_args(Path::new("/work"), "review this", "grok-4", "high");
        let args = args.iter().map(|arg| arg.to_str().unwrap()).collect::<Vec<_>>();
        assert!(args.contains(&"--no-auto-update"));
        assert!(args.contains(&"--always-approve"));
        assert!(args.contains(&"--no-subagents"));
        assert!(args.contains(&"--disable-web-search"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "streaming-json"]));
        assert!(args.windows(2).any(|pair| pair == ["--cwd", "/work"]));
        assert!(args.windows(2).any(|pair| pair == ["-m", "grok-4"]));
        assert!(args.windows(2).any(|pair| pair == ["--effort", "high"]));
        assert!(args.windows(2).any(|pair| pair == ["-p", "review this"]));
        assert!(args.windows(2).any(|pair| pair == ["--tools", RUNNER_TOOLS]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--disallowed-tools", DISALLOWED_TOOLS]));
    }

    #[test]
    fn auth_and_api_key_validation() {
        let scoped = r#"{"https://auth.x.ai::client":{"auth_mode":"oidc","key":"tok_access_token_value","refresh_token":"tok_refresh_token_value","user_id":"u1"}}"#;
        assert!(validate_auth(scoped).is_ok());
        assert!(validate_auth(r#"{"https://auth.x.ai::client":{"auth_mode":"oidc"}}"#).is_err());
        let legacy =
            r#"{"tokens":{"access_token":"tok_access_token_value","refresh_token":"tok_refresh_token_value"}}"#;
        assert!(validate_auth(legacy).is_ok());
        assert!(validate_auth(r#"{"tokens":{}}"#).is_err());
        assert!(validate_auth("not json").is_err());
        assert!(validate_api_key("xai-fixture-key-value").is_ok());
        assert!(validate_api_key("sk-other").is_err());
        assert_eq!(secrets(Some(scoped), Some("xai-fixture-key-value")).len(), 3);
        assert_eq!(secrets(Some(legacy), None).len(), 2);
        assert_eq!(account_email(scoped), None);
        assert_eq!(access_token(scoped).as_deref(), Some("tok_access_token_value"));
        let claims = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"email":"dev@example.com"}"#,
        );
        let with_email = format!(
            r#"{{"https://auth.x.ai::client":{{"auth_mode":"oidc","key":"h.{claims}.s","refresh_token":"tok_refresh_token_value"}}}}"#
        );
        assert_eq!(account_email(&with_email).as_deref(), Some("dev@example.com"));
    }

    #[test]
    fn parse_models_keeps_ids_and_efforts() {
        let body = r#"{"models":[{"id":"grok-4","name":"Grok 4","default_effort":"medium","efforts":["low","medium","high"]}]}"#;
        let models = parse_models_output(body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "grok-4");
        assert_eq!(models[0].label, "Grok 4");
        assert_eq!(models[0].efforts, vec!["low", "medium", "high"]);
        assert_eq!(models[0].default_effort, "medium");
    }

    #[test]
    fn parse_models_text_lists_available_and_puts_default_first() {
        let body = "You are logged in with grok.com.\n\nDefault model: grok-4.7\n\nAvailable models:\n  * grok-4.7 (default)\n  - grok-4.7-build-fast\n  - grok-4.6\n  - grok-4.5\n";
        let models = parse_models_output(body).unwrap();
        assert_eq!(
            models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(),
            vec!["grok-4.7", "grok-4.7-build-fast", "grok-4.6", "grok-4.5"]
        );
        assert_eq!(models[0].label, "Grok 4.7");
        assert_eq!(models[1].label, "Grok 4.7 Build Fast");
        assert_eq!(models[0].efforts, vec!["low", "medium", "high", "xhigh"]);
        assert_eq!(models[0].default_effort, "medium");
    }

    #[test]
    fn final_result_reads_result_events_and_failures_come_from_cli_text() {
        let ok = r#"{"type":"assistant","delta":"partial"}
{"type":"result","result":"{\"findings\":[]}"}
"#;
        assert_eq!(final_result(ok).unwrap(), "{\"findings\":[]}");
        assert_eq!(
            classify(r#"{"type":"error","message":"401 Unauthorized: invalid api key"}"#, ""),
            Failure::Auth
        );
        assert_eq!(
            classify("", "You've hit your usage limit for this plan"),
            Failure::UsageLimit { resets_at: None }
        );
    }
}
