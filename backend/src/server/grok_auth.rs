use super::*;
use std::fs;
use std::io::Read;
use std::sync::OnceLock;

/// Device codes expire after 15 minutes.
const FLOW_TIMEOUT: Duration = Duration::from_secs(900);
const DEVICE_URL: &str = "https://auth.x.ai/device";

#[derive(Default)]
struct Flow {
    cancel: Option<Arc<AtomicBool>>,
    events: Vec<serde_json::Value>,
    secret: Option<String>,
}

fn flow() -> std::sync::MutexGuard<'static, Flow> {
    static FLOW: OnceLock<Mutex<Flow>> = OnceLock::new();
    FLOW.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn authorized(request: &Request, flow: &Flow) -> Result<(), String> {
    require_trusted_origin(request)?;
    match &flow.secret {
        Some(expected) if request.flow_secret.as_deref() != Some(expected.as_str()) => {
            Err("forbidden: invalid flow secret".into())
        }
        _ => Ok(()),
    }
}

/// The one-time code `grok login --device-auth` prints.
pub(crate) fn device_code(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|word| {
            let Some((left, right)) = word.split_once('-') else {
                return false;
            };
            let part = |value: &str| {
                (4..=8).contains(&value.len())
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            };
            part(left) && part(right) && word.bytes().any(|byte| byte.is_ascii_digit())
        })
        .map(str::to_string)
}

/// The authorize link `grok login --device-auth` prints, when present.
pub(crate) fn device_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .find(|word| {
            word.starts_with("https://auth.x.ai/")
                || word.starts_with("https://accounts.x.ai/")
                || word.starts_with("https://x.ai/")
        })
        .map(str::to_string)
}

/// Saves a signed-in `auth.json` to `account` or to a new Grok account.
fn store_auth(db_handle: &Db, account: Option<i64>, raw: &str) -> Result<(), String> {
    let db = db_handle.open()?;
    let id = match account {
        Some(id) => {
            if db.ai_account(id)?.provider != "grok" {
                return Err("That account is not a Grok account".into());
            }
            id
        }
        None => {
            let count = db
                .list_ai_accounts()?
                .iter()
                .filter(|account| account.provider == "grok")
                .count();
            let label = if count == 0 {
                "Grok".to_string()
            } else {
                format!("Grok {}", count + 1)
            };
            db.create_ai_account("grok", &label)?.id
        }
    };
    let stored = crate::accounts::store_grok_auth(id, raw);
    if stored.is_err() && account.is_none() {
        let _ = db.delete_ai_account(id);
    }
    stored?;
    db.reset_ai_account_status(id)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GrokAuthStart {
    account_id: Option<i64>,
}

/// Starts `grok login --device-auth` in a fresh Grok home; the resulting sign-in is saved to
/// `accountId` when given, otherwise to a new account.
pub(crate) fn start_grok_auth(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    require_trusted_origin(request)?;
    let target: GrokAuthStart = if request.body.iter().all(u8::is_ascii_whitespace) {
        GrokAuthStart::default()
    } else {
        serde_json::from_slice(&request.body).map_err(|_| "Invalid sign-in request")?
    };
    let account = target.account_id;
    if let Some(id) = account {
        with_db_read_raw(state, |db| db.ai_account(id))?;
    }
    let db_handle = state.config.db.clone();
    let mut current = flow();
    if current.cancel.is_some() {
        return Ok(serde_json::json!({ "running": true, "flowSecret": current.secret }));
    }
    let home = std::env::temp_dir().join(format!("ocr-grok-auth-{}", crate::sandbox::secret()?));
    fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    let mut child = Command::new("grok")
        .args(["login", "--device-auth"])
        .env("GROK_HOME", &home)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("GROK_DISABLE_AUTOUPDATER", "1")
        .env_remove("XAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not start the Grok CLI: {error}"))?;
    let cancel = Arc::new(AtomicBool::new(false));
    let secret = generate_flow_secret();
    *current = Flow {
        cancel: Some(cancel.clone()),
        events: vec![],
        secret: Some(secret.clone()),
    };
    drop(current);
    let output = Arc::new(Mutex::new(String::new()));
    for stream in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let output = output.clone();
        thread::spawn(move || {
            let mut stream = stream;
            let mut buffer = [0u8; 4096];
            while let Ok(read) = stream.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                if let Ok(mut text) = output.lock() {
                    if text.len() < 65536 {
                        text.push_str(&String::from_utf8_lossy(&buffer[..read]));
                    }
                }
            }
        });
    }
    thread::spawn(move || {
        let pid = child.id();
        let started = Instant::now();
        let mut prompted = false;
        let finish = |event: serde_json::Value| {
            let _ = fs::remove_dir_all(&home);
            let mut current = flow();
            current.events.push(event);
            current.cancel = None;
        };
        loop {
            let text = strip_terminal(&output.lock().map(|text| text.clone()).unwrap_or_default());
            if !prompted {
                if let Some(code) = device_code(&text) {
                    prompted = true;
                    let url = device_url(&text).unwrap_or_else(|| DEVICE_URL.into());
                    flow()
                        .events
                        .push(serde_json::json!({ "kind": "prompt", "url": url, "code": code }));
                }
            }
            if cancel.load(Ordering::Relaxed) || started.elapsed() > FLOW_TIMEOUT {
                crate::review::kill_tree(pid, &mut child);
                let message = if cancel.load(Ordering::Relaxed) {
                    "Sign-in was cancelled."
                } else {
                    "The code expired. Start again."
                };
                return finish(serde_json::json!({ "kind": "done", "success": false, "message": message }));
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let saved = fs::read_to_string(home.join("auth.json"))
                        .ok()
                        .filter(|_| status.success());
                    let event = match saved.map(|raw| store_auth(&db_handle, account, &raw)) {
                        Some(Ok(())) => {
                            serde_json::json!({ "kind": "done", "success": true, "message": "Signed in to Grok." })
                        }
                        Some(Err(error)) => serde_json::json!({ "kind": "done", "success": false, "message": error }),
                        None => {
                            serde_json::json!({ "kind": "done", "success": false, "message": "Grok sign-in did not finish. Start again." })
                        }
                    };
                    return finish(event);
                }
                Ok(None) => thread::sleep(Duration::from_millis(500)),
                Err(error) => {
                    return finish(
                        serde_json::json!({ "kind": "done", "success": false, "message": error.to_string() }),
                    )
                }
            }
        }
    });
    Ok(serde_json::json!({ "running": true, "flowSecret": secret }))
}

pub(crate) fn grok_auth_events(request: &Request) -> Result<serde_json::Value, String> {
    let current = flow();
    authorized(request, &current)?;
    Ok(serde_json::json!({ "running": current.cancel.is_some(), "events": current.events }))
}

pub(crate) fn cancel_grok_auth(request: &Request) -> Result<serde_json::Value, String> {
    let current = flow();
    authorized(request, &current)?;
    if let Some(cancel) = &current.cancel {
        cancel.store(true, Ordering::Relaxed);
    }
    Ok(serde_json::json!({ "running": false }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GrokApiKeyPaste {
    api_key: String,
    account_id: Option<i64>,
}

/// Saves a pasted xAI API key to `accountId` or to a new Grok account.
pub(crate) fn save_grok_api_key(request: &Request, state: &SharedState) -> Result<serde_json::Value, String> {
    require_trusted_origin(request)?;
    let paste: GrokApiKeyPaste = serde_json::from_slice(&request.body).map_err(|_| "Invalid API key")?;
    crate::providers::grok::validate_api_key(&paste.api_key)?;
    if let Some(id) = paste.account_id {
        with_db_read_raw(state, |db| db.ai_account(id))?;
    }
    let db = state.config.db.open()?;
    let id = match paste.account_id {
        Some(id) => {
            if db.ai_account(id)?.provider != "grok" {
                return Err("That account is not a Grok account".into());
            }
            id
        }
        None => {
            let count = db
                .list_ai_accounts()?
                .iter()
                .filter(|account| account.provider == "grok")
                .count();
            let label = if count == 0 {
                "Grok".to_string()
            } else {
                format!("Grok {}", count + 1)
            };
            db.create_ai_account("grok", &label)?.id
        }
    };
    let stored = crate::accounts::store_grok_api_key(id, &paste.api_key);
    if stored.is_err() && paste.account_id.is_none() {
        let _ = db.delete_ai_account(id);
    }
    stored?;
    db.reset_ai_account_status(id)?;
    Ok(serde_json::json!({ "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_code_is_read_from_the_cli_instructions() {
        let text = "Visit https://auth.x.ai/device and enter code\n   7KQ2-9XWPL\n";
        assert_eq!(device_code(text).as_deref(), Some("7KQ2-9XWPL"));
        assert_eq!(device_url(text).as_deref(), Some("https://auth.x.ai/device"));
        assert_eq!(device_code("Welcome to Grok Build"), None);
    }
}
