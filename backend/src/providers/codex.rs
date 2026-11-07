//! Codex CLI execution for review stages that run on a Codex (ChatGPT) account.
use serde_json::Value;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::providers::claude::Failure;
use crate::providers::provider::{DiscoveredModel, ProviderAuth};

/// Reasoning efforts Codex accepts when a catalogue entry omits levels.
const DEFAULT_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];

/// `auth.json` written by `codex login`; anything larger is not a Codex credential.
pub(crate) const MAX_AUTH_BYTES: usize = 64 * 1024;

/// Features that reach services beyond the model, run other agents, or load local extensions.
const DISABLED_FEATURES: &[&str] = &[
    "apps",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "computer_use",
    "image_generation",
    "multi_agent",
    "plugins",
    "remote_plugin",
    "plugin_sharing",
    "skill_mcp_dependency_install",
    "skill_search",
    "tool_suggest",
    "goals",
    "hooks",
    "in_app_browser",
    "daemon_auto_start",
    "workspace_dependencies",
    "worktrees",
    "shell_snapshot",
];

/// Arguments for one non-interactive execution whose prompt arrives on stdin, whose JSONL events
/// go to stdout and whose final message is written to `last_message`.
///
/// Codex's own sandbox needs user namespaces, which the hardened runner container denies, so the
/// container is the sandbox: read-only root, no capabilities, an unprivileged UID and egress
/// limited to the model provider.
pub(crate) fn exec_args(workdir: &Path, last_message: &Path, model: &str, effort: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["exec".into()];
    for feature in DISABLED_FEATURES {
        args.extend(["--disable".into(), OsString::from(feature)]);
    }
    args.extend(
        [
            "--json",
            "--color",
            "never",
            "--ephemeral",
            "--ignore-user-config",
            "--ignore-rules",
            "--skip-git-repo-check",
            "--dangerously-bypass-approvals-and-sandbox",
            "-c",
            "web_search=\"disabled\"",
        ]
        .map(OsString::from),
    );
    // `default` leaves the model to Codex.
    if model != "default" {
        args.extend(["-m".into(), OsString::from(model)]);
    }
    args.extend([
        "-c".into(),
        OsString::from(format!("model_reasoning_effort=\"{effort}\"")),
    ]);
    args.extend([
        OsString::from("-C"),
        workdir.as_os_str().to_owned(),
        OsString::from("-o"),
        last_message.as_os_str().to_owned(),
        OsString::from("-"),
    ]);
    args
}

/// Checks that `raw` is a Codex `auth.json` holding a ChatGPT sign-in.
pub(crate) fn validate_auth(raw: &str) -> Result<(), String> {
    if raw.len() > MAX_AUTH_BYTES {
        return Err("Codex authentication is too large".into());
    }
    let value: Value = serde_json::from_str(raw).map_err(|_| "Codex authentication is not valid JSON")?;
    if value["tokens"]["refresh_token"].as_str().is_none_or(str::is_empty) {
        return Err("Codex authentication has no ChatGPT sign-in; sign in again".into());
    }
    Ok(())
}

/// The ChatGPT email in the sign-in's ID token, to name the account.
pub(crate) fn account_email(raw: &str) -> Option<String> {
    use base64::Engine;
    let value: Value = serde_json::from_str(raw).ok()?;
    let payload = value["tokens"]["id_token"].as_str()?.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .ok()?,
    )
    .ok()?;
    claims["email"]
        .as_str()
        .filter(|email| email.contains('@') && email.len() <= 80)
        .map(str::to_string)
}

/// Credential values in an `auth.json` that must never appear in review output.
pub(crate) fn secrets(raw: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    ["access_token", "refresh_token", "id_token"]
        .iter()
        .filter_map(|key| value["tokens"][key].as_str())
        .chain(value["OPENAI_API_KEY"].as_str())
        .filter(|secret| secret.len() >= 16)
        .map(str::to_string)
        .collect()
}

/// Why an execution failed, read only from Codex's own error events and stderr, never from
/// command output, so reviewed code that mentions "usage limit" can't trip it.
pub(crate) fn classify(stdout: &str, stderr: &str) -> Failure {
    let mut text = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter_map(|event| match event["type"].as_str() {
            Some("error") => event["message"].as_str().map(str::to_string),
            Some("turn.failed") => event["error"]["message"].as_str().map(str::to_string),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text.push_str(stderr);
    let lower = text.to_ascii_lowercase();
    if ["usage limit", "usage_limit_reached", "hit your limit"]
        .iter()
        .any(|word| lower.contains(word))
    {
        return Failure::UsageLimit {
            resets_at: retry_after(&lower).map(|millis| crate::db::now_millis() + millis),
        };
    }
    if [
        "401 unauthorized",
        "refresh token",
        "token_expired",
        "not logged in",
        "please log in",
        "sign in again",
        "login again",
    ]
    .iter()
    .any(|word| lower.contains(word))
    {
        return Failure::Auth;
    }
    if ["429", "rate limit", "overloaded", "503", "server_is_overloaded"]
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

/// Codex says when a window resets as "try again in 2 hours 13 minutes" (or days/seconds).
fn retry_after(lower: &str) -> Option<i64> {
    let tail = &lower[lower.find("try again in ")? + "try again in ".len()..];
    let words = tail
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty())
        .take(8)
        .collect::<Vec<_>>();
    let mut total = 0i64;
    for pair in words.windows(2) {
        let Ok(amount) = pair[0].parse::<i64>() else { continue };
        let unit = pair[1].trim_end_matches(|c: char| !c.is_ascii_alphabetic());
        total += amount
            * match unit.trim_end_matches('s') {
                "day" => 86_400_000,
                "hour" | "hr" => 3_600_000,
                "minute" | "min" => 60_000,
                "second" | "sec" => 1_000,
                _ => 0,
            };
    }
    (total > 0).then_some(total)
}

/// Short, safe diagnostic for a failed process; raw CLI output stays private.
pub(crate) fn failure(stdout: &str, stderr: &str) -> String {
    match classify(stdout, stderr) {
        Failure::Other if stdout.trim().is_empty() && stderr.trim().is_empty() => {
            "Codex process exited without diagnostics; verify runner health and authentication".into()
        }
        Failure::Other => "Codex execution failed; verify authentication".into(),
        Failure::Overloaded => "Codex is temporarily overloaded; retry later".into(),
        Failure::Model => "Codex model is unavailable for this account".into(),
        failure => failure.encode(),
    }
}

/// Models available to the signed-in Codex account via `codex debug models`.
pub(crate) fn discover_models(auth: &ProviderAuth) -> Result<Vec<DiscoveredModel>, String> {
    let mut models = match auth {
        ProviderAuth::Codex { auth_json } => debug_models(Some(auth_json), false)
            .or_else(|_| debug_models(Some(auth_json), true))
            .or_else(|_| debug_models(None, true))?,
        ProviderAuth::None | ProviderAuth::Claude { .. } | ProviderAuth::Grok { .. } => debug_models(None, true)?,
    };
    if !models.iter().any(|model| model.id == "default") {
        models.insert(
            0,
            DiscoveredModel {
                id: "default".into(),
                label: "Codex default".into(),
                efforts: DEFAULT_EFFORTS.iter().map(|effort| (*effort).into()).collect(),
                default_effort: "medium".into(),
            },
        );
    }
    Ok(models)
}

fn debug_models(auth_json: Option<&str>, bundled: bool) -> Result<Vec<DiscoveredModel>, String> {
    let home = std::env::temp_dir().join(format!("ocr-codex-models-{}", crate::sandbox::secret()?));
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
    let mut command = Command::new("codex");
    command
        .args(["debug", "models"])
        .env("CODEX_HOME", &home)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if bundled {
        command.arg("--bundled");
    }
    if let Ok(proxy) = std::env::var("OCR_RUNNER_PROXY").or_else(|_| std::env::var("HTTPS_PROXY")) {
        if !proxy.is_empty() {
            command.env("HTTPS_PROXY", &proxy).env("HTTP_PROXY", &proxy);
        }
    }
    let output = command
        .output()
        .map_err(|_| "Codex model catalogue is unavailable right now")?;
    let _ = std::fs::remove_dir_all(&home);
    if !output.status.success() {
        return Err("Codex model catalogue is unavailable right now".into());
    }
    let body: Value =
        serde_json::from_slice(&output.stdout).map_err(|_| "Codex returned an unreadable model catalogue")?;
    parse_debug_models(&body)
}

/// Parses `codex debug models` JSON into picker rows (`visibility == "list"` only).
pub(crate) fn parse_debug_models(body: &Value) -> Result<Vec<DiscoveredModel>, String> {
    let Some(models) = body.get("models").and_then(Value::as_array) else {
        return Err("Codex model catalogue is missing models".into());
    };
    Ok(models.iter().filter_map(parse_model).collect())
}

fn parse_model(value: &Value) -> Option<DiscoveredModel> {
    let visibility = value.get("visibility").and_then(Value::as_str).unwrap_or("list");
    if visibility != "list" {
        return None;
    }
    let id = value
        .get("slug")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)?
        .to_string();
    if id.is_empty() {
        return None;
    }
    let label = value
        .get("display_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(&id)
        .to_string();
    let efforts = value
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| {
                    level
                        .get("effort")
                        .or_else(|| level.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect::<Vec<_>>()
        })
        .filter(|levels| !levels.is_empty())
        .unwrap_or_else(|| DEFAULT_EFFORTS.iter().map(|effort| (*effort).into()).collect());
    let default_effort = value
        .get("default_reasoning_level")
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
    fn parse_debug_models_keeps_listed_slugs_and_efforts() {
        let body = serde_json::json!({
            "models": [
                {
                    "slug": "gpt-5.5",
                    "display_name": "GPT-5.5",
                    "visibility": "list",
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [
                        { "effort": "low" },
                        { "effort": "medium" },
                        { "effort": "high" },
                        { "effort": "xhigh" }
                    ]
                },
                {
                    "slug": "hidden",
                    "display_name": "Hidden",
                    "visibility": "hide"
                }
            ]
        });
        let models = parse_debug_models(&body).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.5");
        assert_eq!(models[0].label, "GPT-5.5");
        assert_eq!(models[0].efforts, vec!["low", "medium", "high", "xhigh"]);
        assert_eq!(models[0].default_effort, "medium");
    }

    #[test]
    fn exec_args_disable_extensions_and_read_the_prompt_from_stdin() {
        let args = exec_args(Path::new("/work"), Path::new("/scratch/last.txt"), "default", "medium");
        let args = args.iter().map(|arg| arg.to_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(args[0], "exec");
        assert_eq!(args.last(), Some(&"-"));
        for feature in ["apps", "plugins", "multi_agent", "browser_use", "hooks"] {
            assert!(args.windows(2).any(|pair| pair == ["--disable", feature]), "{feature}");
        }
        assert!(args.windows(2).any(|pair| pair == ["-c", "web_search=\"disabled\""]));
        assert!(args.contains(&"--ignore-user-config") && args.contains(&"--ephemeral"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-c", "model_reasoning_effort=\"medium\""]));
        assert!(!args.contains(&"-m"), "the default model is Codex's own");
        let args = exec_args(Path::new("/work"), Path::new("/scratch/last.txt"), "gpt-5.5", "high");
        let args = args.iter().map(|arg| arg.to_str().unwrap()).collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-m", "gpt-5.5"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-c", "model_reasoning_effort=\"high\""]));
        assert_eq!(args.last(), Some(&"-"));
    }

    #[test]
    fn auth_needs_a_chatgpt_sign_in_and_exposes_its_secrets_for_redaction() {
        let auth = r#"{"OPENAI_API_KEY":null,"tokens":{"id_token":"eyJ-fixture-id-token-value","access_token":"eyJ-fixture-access-token","refresh_token":"rt_fixture_refresh_token","account_id":"a"},"last_refresh":"2026-10-24T00:00:00Z"}"#;
        assert!(validate_auth(auth).is_ok());
        assert!(validate_auth(r#"{"tokens":{}}"#).is_err());
        assert!(validate_auth("not json").is_err());
        assert_eq!(secrets(auth).len(), 3);
        assert_eq!(account_email(auth), None);
        let claims = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"email":"dev@example.com"}"#,
        );
        let signed =
            format!(r#"{{"tokens":{{"id_token":"h.{claims}.s","refresh_token":"rt_fixture_refresh_token"}}}}"#);
        assert_eq!(account_email(&signed).as_deref(), Some("dev@example.com"));
    }

    #[test]
    fn failures_come_from_codex_error_events_not_command_output() {
        let command = r#"{"type":"item.completed","item":{"type":"command_execution","aggregated_output":"usage limit 401 Unauthorized"}}"#;
        assert_eq!(classify(command, ""), Failure::Other);
        let limited = r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit. Upgrade to Pro, or try again in 2 hours 13 minutes."}}"#;
        let Failure::UsageLimit { resets_at: Some(at) } = classify(limited, "") else {
            panic!("expected a usage limit with a reset")
        };
        let expected = crate::db::now_millis() + 2 * 3_600_000 + 13 * 60_000;
        assert!((at - expected).abs() < 5_000);
        assert_eq!(
            classify(
                r#"{"type":"error","message":"unexpected status 401 Unauthorized: token expired"}"#,
                ""
            ),
            Failure::Auth
        );
        assert_eq!(classify("", "stream error: 429 Too Many Requests"), Failure::Overloaded);
        assert_eq!(
            classify(
                r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit."}}"#,
                ""
            ),
            Failure::UsageLimit { resets_at: None }
        );
    }
}
