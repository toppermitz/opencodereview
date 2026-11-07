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
const FALLBACK_MODELS: &[(&str, &str)] = &[("grok-4", "Grok 4"), ("grok-code", "Grok Code")];

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

/// Checks that `raw` is a Grok `auth.json` from `grok login`.
pub(crate) fn validate_auth(raw: &str) -> Result<(), String> {
    if raw.len() > MAX_AUTH_BYTES {
        return Err("Grok authentication is too large".into());
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| "Grok authentication is not valid JSON")?;
    let has_token = [
        value["access_token"].as_str(),
        value["refresh_token"].as_str(),
        value["tokens"]["access_token"].as_str(),
        value["tokens"]["refresh_token"].as_str(),
        value["session"]["access_token"].as_str(),
        value["session"]["refresh_token"].as_str(),
    ]
    .iter()
    .any(|token| token.is_some_and(|value| !value.is_empty()));
    if !has_token {
        return Err("Grok authentication has no sign-in; sign in again".into());
    }
    Ok(())
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

/// Credential values in an `auth.json` or API key that must never appear in review output.
pub(crate) fn secrets(auth_json: Option<&str>, api_key: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(raw) = auth_json {
        if let Ok(value) = serde_json::from_str::<Value>(raw) {
            for key in ["access_token", "refresh_token", "id_token"] {
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
    let mut command = Command::new("grok");
    command
        .args(["models", "--json"])
        .env("GROK_HOME", &home)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("GROK_DISABLE_AUTOUPDATER", "1")
        .env_remove("XAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
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
        .or_else(|_| {
            // Older builds accept `grok models` without `--json`.
            let mut retry = Command::new("grok");
            retry
                .args(["models"])
                .env("GROK_HOME", &home)
                .env("HOME", &home)
                .env("NO_COLOR", "1")
                .env("GROK_DISABLE_AUTOUPDATER", "1")
                .env_remove("XAI_API_KEY")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            if let Some(key) = api_key {
                retry.env("XAI_API_KEY", key);
            }
            retry.output()
        })
        .map_err(|_| "Grok model catalogue is unavailable right now")?;
    let _ = std::fs::remove_dir_all(&home);
    if !output.status.success() {
        return Err("Grok model catalogue is unavailable right now".into());
    }
    parse_models_output(&String::from_utf8_lossy(&output.stdout))
}

/// Parses `grok models` JSON (array or `{models:[…]}`) into picker rows.
pub(crate) fn parse_models_output(raw: &str) -> Result<Vec<DiscoveredModel>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Grok model catalogue is empty".into());
    }
    if let Ok(body) = serde_json::from_str::<Value>(trimmed) {
        return parse_models_json(&body);
    }
    // Some builds print one JSON object per line.
    let models = trimmed
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter_map(|value| parse_model(&value))
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err("Grok returned an unreadable model catalogue".into());
    }
    Ok(models)
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
        let auth = r#"{"tokens":{"access_token":"tok_access_token_value","refresh_token":"tok_refresh_token_value"}}"#;
        assert!(validate_auth(auth).is_ok());
        assert!(validate_auth(r#"{"tokens":{}}"#).is_err());
        assert!(validate_auth("not json").is_err());
        assert!(validate_api_key("xai-fixture-key-value").is_ok());
        assert!(validate_api_key("sk-other").is_err());
        assert_eq!(secrets(Some(auth), Some("xai-fixture-key-value")).len(), 3);
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
