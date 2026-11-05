use super::*;

pub(crate) fn gitlab_status() -> ToolStatus {
    let mut probe = Command::new("glab");
    probe.args(["api", "user"]);
    if let Err(error) = crate::gitlab::apply_gitlab_host(&mut probe) {
        return ToolStatus {
            ok: false,
            detail: error,
        };
    }
    match probe.output() {
        Ok(output) if output.status.success() => {
            // `/health` is public: never forward the full `/user` JSON (email, etc.).
            let username = serde_json::from_slice::<serde_json::Value>(&output.stdout)
                .ok()
                .and_then(|value| {
                    value
                        .get("username")
                        .or_else(|| value.get("login"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                });
            ToolStatus {
                ok: true,
                detail: username.unwrap_or_else(|| "authenticated".into()),
            }
        }
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("glab api user failed")
                .to_string();
            ToolStatus { ok: false, detail }
        }
        Err(error) => ToolStatus {
            ok: false,
            detail: error.to_string(),
        },
    }
}

pub(crate) fn command_output_timeout(command: &str, args: &[&str], timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new(command)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let start = Instant::now();
    loop {
        if child.try_wait().map_err(|error| error.to_string())?.is_some() {
            return child.wait_with_output().map_err(|error| error.to_string());
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{command} timed out after {}s", timeout.as_secs()));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn command_status(command: &str, args: &[&str]) -> ToolStatus {
    match Command::new(command).args(args).output() {
        Ok(output) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            ToolStatus {
                ok: output.status.success(),
                detail: text
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or("ok")
                    .to_string(),
            }
        }
        Err(error) => ToolStatus {
            ok: false,
            detail: error.to_string(),
        },
    }
}

pub(crate) fn with_db<T: Serialize>(
    state: &SharedState,
    f: impl FnOnce(&Database) -> Result<T, String>,
) -> Result<serde_json::Value, String> {
    serde_json::to_value(with_db_raw(state, f)?).map_err(|error| error.to_string())
}

pub(crate) fn with_db_read<T: Serialize>(
    state: &SharedState,
    f: impl FnOnce(&Database) -> Result<T, String>,
) -> Result<serde_json::Value, String> {
    serde_json::to_value(with_db_read_raw(state, f)?).map_err(|error| error.to_string())
}

pub(crate) fn with_db_raw<T>(state: &SharedState, f: impl FnOnce(&Database) -> Result<T, String>) -> Result<T, String> {
    let db = state.config.db.open()?;
    f(&db)
}

pub(crate) fn with_db_read_raw<T>(
    state: &SharedState,
    f: impl FnOnce(&Database) -> Result<T, String>,
) -> Result<T, String> {
    let db = state.config.db.open_read()?;
    f(&db)
}

pub(crate) fn parse_id(value: &str) -> Result<i64, String> {
    value.parse::<i64>().map_err(|_| format!("invalid id: {value}"))
}

/// The first value of `name` in the request's query string, percent-decoded.
pub(crate) fn query_param(request: &Request, name: &str) -> Option<String> {
    request.query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' => match value
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            {
                Some(byte) => {
                    out.push(byte);
                    index += 2;
                }
                None => out.push(b'%'),
            },
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn json_ok(value: &serde_json::Value) -> Response {
    Response {
        status: 200,
        content_type: "application/json",
        body: serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec()),
    }
}

pub(crate) fn json_error(status: u16, message: &str) -> Response {
    Response {
        status,
        content_type: "application/json",
        body: serde_json::to_vec(&serde_json::json!({ "error": message })).unwrap_or_else(|_| b"{}".to_vec()),
    }
}

pub(crate) fn html_ok_owned(html: String) -> Response {
    Response {
        status: 200,
        content_type: "text/html; charset=utf-8",
        body: html.into_bytes(),
    }
}

pub(crate) fn web_app_entry_html() -> Option<String> {
    let url = std::env::var("OCR_WEB_APP_URL").ok()?;
    let target = format!("{}/onboarding", url.trim_end_matches('/'));
    Some(embed_brand_asset(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="refresh" content="0;url={target}"><title>OpenCodeReview</title><link rel="icon" type="image/svg+xml" href="/assets/favicon.svg"></head><body><script>window.location.replace({target:?});</script><a href="{target}">Open OpenCodeReview onboarding</a></body></html>"#
    )))
}

pub(crate) fn onboarding_html() -> String {
    embed_brand_asset(
        std::env::var("OCR_ONBOARDING_HTML_PATH")
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .or_else(|| std::fs::read_to_string("assets/onboarding.html").ok())
            .unwrap_or_else(|| ONBOARDING_HTML.to_string()),
    )
}

fn embed_brand_asset(html: String) -> String {
    use base64::Engine;
    let assets: [(&str, &[u8]); 2] = [
        ("/assets/mascot.svg", include_bytes!("../../../assets/mascot.svg")),
        ("/assets/favicon.svg", include_bytes!("../../../assets/favicon.svg")),
    ];
    assets.into_iter().fold(html, |html, (path, bytes)| {
        let svg = base64::engine::general_purpose::STANDARD.encode(bytes);
        html.replace(&format!("\"{path}\""), &format!("\"data:image/svg+xml;base64,{svg}\""))
    })
}

pub(crate) fn trusted_cors_origin(origin: Option<&str>) -> Option<String> {
    let trusted = std::env::var("OCR_WEB_APP_URL").ok()?;
    let trusted = trusted.trim_end_matches('/');
    origin.filter(|value| origins_match(value, trusted)).map(str::to_string)
}

pub(crate) fn origins_match(origin: &str, trusted: &str) -> bool {
    let parse = |value: &str| {
        reqwest::Url::parse(value).ok().filter(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
        })
    };
    let (Some(origin), Some(trusted)) = (parse(origin), parse(trusted)) else {
        return false;
    };
    let loopback = |url: &reqwest::Url| matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    origin.scheme() == trusted.scheme()
        && origin.port_or_known_default() == trusted.port_or_known_default()
        && (origin.host_str() == trusted.host_str() || loopback(&origin) && loopback(&trusted))
}

pub(crate) fn require_auth(request: &Request, state: &SharedState) -> Result<(), String> {
    let token = state
        .config
        .api_token
        .as_deref()
        .ok_or_else(|| "unauthorized: OCR_API_TOKEN must be set to use this endpoint".to_string())?;
    let provided = request.authorization.as_deref().and_then(|h| h.strip_prefix("Bearer "));
    match provided {
        Some(value) if value == token => Ok(()),
        _ => Err("unauthorized: invalid or missing API token".to_string()),
    }
}

pub(crate) fn require_trusted_origin(request: &Request) -> Result<(), String> {
    let trusted = std::env::var("OCR_WEB_APP_URL").unwrap_or_else(|_| "http://localhost:1421".to_string());
    match request.origin.as_deref() {
        Some(origin) if origins_match(origin, &trusted) => Ok(()),
        _ => Err("forbidden: untrusted origin".to_string()),
    }
}

pub(crate) fn generate_flow_secret() -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    hasher.update(now.as_nanos().to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let mut entropy = [0u8; 32];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut entropy);
    }
    hasher.update(entropy);
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn write_response(
    stream: &mut TcpStream,
    response: Response,
    cors_origin: Option<&str>,
) -> Result<(), String> {
    let reason = match response.status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let cors = cors_origin.map(|origin| format!("access-control-allow-origin: {origin}\r\naccess-control-allow-methods: GET,POST,PUT,DELETE,OPTIONS\r\naccess-control-allow-headers: content-type,authorization,x-flow-secret\r\n")).unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\n{}connection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len(),
        cors,
    );
    stream.write_all(head.as_bytes()).map_err(|error| error.to_string())?;
    stream.write_all(&response.body).map_err(|error| error.to_string())
}

const ONBOARDING_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>OpenCodeReview setup</title>
  <link rel="icon" type="image/svg+xml" href="/assets/favicon.svg" />
  <style>
    :root { color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, sans-serif; background: #080a0f; color: #f4f6fb; }
    * { box-sizing: border-box; }
    body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: radial-gradient(circle at 50% -10%, rgba(86,103,255,.22), transparent 38%), linear-gradient(180deg, #101629 0%, #080a0f 64%); }
    main { width: min(720px, calc(100vw - 40px)); }
    .card { border: 1px solid rgba(255,255,255,.11); background: linear-gradient(180deg, rgba(15,18,30,.96), rgba(11,13,22,.96)); border-radius: 30px; box-shadow: 0 34px 100px rgba(0,0,0,.42), inset 0 1px 0 rgba(255,255,255,.04); padding: 38px; }
    .brand { display: flex; justify-content: space-between; gap: 24px; align-items: center; margin-bottom: 42px; }
    .logo { display: flex; align-items: center; gap: 10px; font-size: 16px; font-weight: 900; letter-spacing: -.035em; }
    .logo img { display: block; flex: none; width: 64px; height: 48px; object-fit: contain; }
    .steps { display: flex; gap: 8px; color: #7f879a; font-size: 12px; font-weight: 750; }
    .steps span { padding: 7px 11px; border-radius: 999px; background: rgba(255,255,255,.055); border: 1px solid transparent; }
    .steps .active { color: #fff; background: rgba(111,124,255,.2); border-color: rgba(145,154,255,.28); }
    .eyebrow { color: #8f98ff; font-size: 13px; font-weight: 850; letter-spacing: .08em; text-transform: uppercase; margin-bottom: 12px; }
    h1 { margin: 0; font-size: 42px; line-height: 1.02; letter-spacing: -.065em; }
    p { color: #aeb6c9; line-height: 1.62; margin: 14px 0 0; font-size: 16px; }
    button { border: 1px solid rgba(255,255,255,.08); border-radius: 14px; padding: 12px 16px; color: #f4f6fb; background: #2a3146; cursor: pointer; font-weight: 850; box-shadow: 0 1px 0 rgba(255,255,255,.05) inset; }
    button.primary { border-color: transparent; background: linear-gradient(135deg, #6978ff, #7d67ff); color: white; box-shadow: 0 14px 30px rgba(105,120,255,.22); }
    button:hover:not(:disabled) { transform: translateY(-1px); }
    button:disabled { opacity: .46; cursor: not-allowed; }
    .actions { display: flex; justify-content: space-between; gap: 12px; margin-top: 32px; }
    .right { margin-left: auto; display: flex; gap: 12px; }
    .panel { margin-top: 24px; }
    .status { border: 1px solid rgba(255,255,255,.1); background: rgba(255,255,255,.045); border-radius: 20px; padding: 18px; }
    .status b { font-size: 15px; }
    .ok { color: #78e6a3; }
    .bad { color: #ff9090; }
    .pending { color: #ffd98a; }
    .auth-box { border: 1px solid rgba(145,154,255,.2); background: rgba(111,124,255,.08); border-radius: 20px; padding: 16px; display: grid; gap: 12px; }
    .auth-row { display: grid; grid-template-columns: 110px minmax(0, 1fr) auto; align-items: center; gap: 12px; }
    .auth-label { color: #8c95aa; font-size: 13px; font-weight: 800; }
    .copyable { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding: 10px 12px; border-radius: 13px; background: rgba(255,255,255,.07); color: #f4f6fb; }
    code.copyable { display: block; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 22px; letter-spacing: .12em; }
    .copy-btn { padding: 9px 12px; font-size: 12px; }
    .success-box { margin-top: 24px; display: grid; place-items: center; text-align: center; border: 1px solid rgba(120,230,163,.22); background: radial-gradient(circle at 50% 0%, rgba(120,230,163,.16), rgba(255,255,255,.04) 48%); border-radius: 24px; padding: 28px; }
    .check { width: 58px; height: 58px; display: grid; place-items: center; border-radius: 999px; background: linear-gradient(135deg, #58d98a, #78e6a3); color: #07100b; font-size: 30px; font-weight: 950; box-shadow: 0 18px 42px rgba(120,230,163,.22); margin-bottom: 14px; }
    .success-box h2 { margin: 0; font-size: 22px; letter-spacing: -.04em; }
    @media (max-width: 640px) { .auth-row { grid-template-columns: 1fr; } }
    .list { max-height: 390px; overflow: auto; margin-top: 24px; border-top: 1px solid rgba(255,255,255,.08); }
    .org-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); gap: 12px; border-top: 0; }
    .org-card { position: relative; display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 14px; align-items: center; width: 100%; min-height: 92px; padding: 16px; text-align: left; border-radius: 20px; border-color: rgba(255,255,255,.1); background: rgba(255,255,255,.045); box-shadow: inset 0 1px 0 rgba(255,255,255,.045); }
    .org-card.selected { border-color: rgba(154,124,255,.84); background: linear-gradient(180deg, rgba(111,124,255,.18), rgba(255,255,255,.055)); box-shadow: 0 0 0 1px rgba(154,124,255,.28), 0 18px 46px rgba(111,124,255,.18), inset 0 1px 0 rgba(255,255,255,.08); }
    .org-avatar { width: 46px; height: 46px; display: grid; place-items: center; border-radius: 16px; background: linear-gradient(135deg, rgba(111,124,255,.28), rgba(154,124,255,.2)); color: #fff; font-weight: 950; letter-spacing: -.04em; }
    .org-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 15px; font-weight: 900; color: #f4f6fb; }
    .org-type { margin-top: 4px; color: #8c95aa; font-size: 13px; font-weight: 750; }
    .org-check { position: absolute; top: 12px; right: 12px; width: 22px; height: 22px; display: grid; place-items: center; border-radius: 999px; border: 1px solid rgba(255,255,255,.14); color: transparent; background: rgba(255,255,255,.05); font-size: 14px; font-weight: 950; }
    .org-card.selected .org-check { border-color: transparent; background: linear-gradient(135deg, #6978ff, #9a7cff); color: white; box-shadow: 0 8px 22px rgba(111,124,255,.28); }
    .repo { display: flex; justify-content: space-between; gap: 14px; align-items: center; padding: 15px 2px; border-bottom: 1px solid rgba(255,255,255,.08); }
    .muted { color: #8c95aa; }
    .hidden { display: none; }
    a { color: #aeb5ff; text-decoration-thickness: 1px; text-underline-offset: 3px; }
    @media (max-width: 640px) { .card { padding: 26px; } .brand, .actions { align-items: stretch; flex-direction: column; } .right { margin-left: 0; } h1 { font-size: 34px; } }
  </style>
</head>
<body>
<main class="card">
  <div class="brand"><div class="logo"><img src="/assets/mascot.svg" width="64" height="48" alt="" aria-hidden="true" />OpenCodeReview</div><div class="steps"><span id="s1">GitHub</span><span id="s2">Organizations</span><span id="s3">Repositories</span></div></div>
  <section id="step-github">
    <div class="eyebrow">Step 1 of 3</div>
    <h1>Connect GitHub</h1>
    <p>Authorize this OpenCodeReview instance to discover your organizations and repositories.</p>
    <div class="panel status" id="authStatus">Checking GitHub...</div>
    <div id="authPrompt" class="panel"></div>
    <div class="actions"><button onclick="refreshAll()">Refresh</button><div class="right"><button id="connectBtn" class="primary" onclick="connectGitHub()">Connect GitHub</button><button id="githubNext" onclick="go(2)" disabled>Continue</button></div></div>
  </section>
  <section id="step-orgs" class="hidden">
    <div class="eyebrow">Step 2 of 3</div>
    <h1>Select organizations</h1>
    <p>Choose where OpenCodeReview should look for repositories. You can change this later.</p>
    <div id="orgs" class="list org-grid"></div>
    <div class="actions"><button onclick="go(1)">Back</button><div class="right"><button onclick="loadOrgs()">Refresh</button><button id="orgsNext" onclick="saveOrgs()" disabled>Continue</button></div></div>
  </section>
  <section id="step-repos" class="hidden">
    <div class="eyebrow">Step 3 of 3</div>
    <h1>Choose repositories</h1>
    <p>Enable repositories now. OpenCodeReview will index them in the background.</p>
    <div id="repos" class="list"></div>
    <div class="actions"><button onclick="go(2)">Back</button><button class="primary" onclick="loadReposForSelected()">Refresh repositories</button></div>
  </section>
</main>
<script>
const $ = (id) => document.getElementById(id);
let step = 1, orgs = [], repos = [], authPoll = null, connected = false;
async function api(path, options = {}) {
  const res = await fetch(path, { headers: { 'content-type': 'application/json' }, ...options });
  const data = await res.json();
  if (!res.ok) throw new Error(data.error || res.statusText);
  return data;
}
function go(next) {
  step = next;
  $('step-github').classList.toggle('hidden', step !== 1);
  $('step-orgs').classList.toggle('hidden', step !== 2);
  $('step-repos').classList.toggle('hidden', step !== 3);
  $('s1').classList.toggle('active', step === 1);
  $('s2').classList.toggle('active', step === 2);
  $('s3').classList.toggle('active', step === 3);
  if (step === 2) loadOrgs();
  if (step === 3) loadReposForSelected();
}
async function loadAuth() {
  const a = await api('/auth/gitlab/status');
  connected = !!a.loggedIn;
  $('authStatus').style.display = connected ? 'none' : '';
  $('authStatus').innerHTML = '<b class="bad">GitHub not connected</b><p>Start the secure GitHub device flow to continue.</p>';
  $('authPrompt').innerHTML = connected ? `<div class="success-box"><div class="check">✓</div><h2>GitHub connected</h2><p>${a.login || 'GitHub user'} is ready. Continue to choose organizations.</p></div>` : '';
  $('connectBtn').style.display = connected ? 'none' : '';
  $('githubNext').classList.toggle('primary', connected);
  $('githubNext').disabled = !connected;
}
async function connectGitHub() {
  await api('/auth/gitlab/start', { method: 'POST' });
  $('authPrompt').innerHTML = '<p>Waiting for GitHub device code...</p>';
  if (authPoll) clearInterval(authPoll);
  authPoll = setInterval(loadAuthEvents, 1000);
  await loadAuthEvents();
}
async function loadAuthEvents() {
  const data = await api('/auth/gitlab/events');
  const prompt = data.events.filter(e => e.kind === 'prompt').reduce((acc, e) => ({...acc, ...e}), {});
  const done = data.events.find(e => e.kind === 'done');
  let html = '';
  if (prompt.url || prompt.code) html += '<div class="auth-box">';
  if (prompt.url) html += `<div class="auth-row"><div class="auth-label">GitHub URL</div><a class="copyable" href="${prompt.url}" target="_blank">${prompt.url}</a><button class="copy-btn" onclick="copyText('${prompt.url}', this)">Copy</button></div>`;
  if (prompt.code) html += `<div class="auth-row"><div class="auth-label">Device code</div><code class="copyable">${prompt.code}</code><button class="copy-btn" onclick="copyText('${prompt.code}', this)">Copy</button></div>`;
  if (html) html += '</div>';
  if (done) html += `<p class="${done.success ? 'ok' : 'bad'}">${done.message}</p>`;
  $('authPrompt').innerHTML = html || '<div class="auth-box"><p class="pending">Waiting for GitHub...</p></div>';
  if (!data.running) { clearInterval(authPoll); authPoll = null; await loadAuth(); }
}
async function loadOrgs() {
  orgs = await api('/gitlab/workspaces');
  renderOrgs();
}
function renderOrgs() {
  $('orgs').innerHTML = orgs.map(o => `<button type="button" class="org-card ${o.selected ? 'selected' : ''}" onclick='toggleOrg(${JSON.stringify(o.login)})' aria-pressed="${o.selected ? 'true' : 'false'}"><div class="org-avatar">${orgInitials(o.login)}</div><div><div class="org-name">${escapeHtml(o.login)}</div><div class="org-type">${o.kind === 'user' ? 'Personal account' : 'Organization'}</div></div><div class="org-check">✓</div></button>`).join('');
  updateOrgContinue();
}
function toggleOrg(login) {
  orgs = orgs.map(o => o.login === login ? {...o, selected: !o.selected} : o);
  renderOrgs();
}
function selectedLogins() {
  return orgs.filter(o => o.selected).map(o => o.login);
}
function updateOrgContinue() {
  const selected = selectedLogins().length > 0;
  $('orgsNext').disabled = !selected;
  $('orgsNext').classList.toggle('primary', selected);
}
function orgInitials(login) {
  return login.split(/[-_\s]+/).filter(Boolean).slice(0, 2).map(part => part[0]).join('').toUpperCase() || login.slice(0, 2).toUpperCase();
}
function escapeHtml(value) {
  return String(value).replace(/[&<>"]/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[char]));
}
async function saveOrgs() {
  const selected = selectedLogins();
  if (!selected.length) return;
  await api('/workspaces/select', { method: 'POST', body: JSON.stringify({ logins: selected }) });
  orgs = orgs.map(o => ({...o, selected: selected.includes(o.login)}));
  go(3);
}
async function loadReposForSelected() {
  repos = [];
  const selected = orgs.filter(o => o.selected);
  $('repos').innerHTML = selected.length ? '<p>Loading repositories...</p>' : '<p>Select at least one organization first.</p>';
  for (const org of selected) {
    const list = await api(`/workspaces/${org.login}/repos/refresh`, { method: 'POST' }).catch(() => []);
    repos.push(...list);
  }
  renderRepos();
}
function renderRepos() {
  if (!repos.length) { $('repos').innerHTML = '<p>No repositories found.</p>'; return; }
  $('repos').innerHTML = repos.map(r => {
    const name = escapeHtml(r.fullName);
    const branch = escapeHtml(r.defaultBranch || 'default branch unknown');
    return `<div class="repo"><div><b>${name}</b><p>${r.subscriptionEnabled ? 'Enabled' : 'Off'} · ${branch}</p></div><div class="right"><button onclick="enableRepo(${r.id})">Enable</button><button class="primary" onclick="indexRepo(${r.id})">Index</button></div></div>`;
  }).join('');
}
async function enableRepo(id) { await api(`/repos/${id}/subscribe`, { method: 'POST', body: JSON.stringify({ enabled: true, reviewMode: 'manual' }) }); await loadReposForSelected(); }
async function indexRepo(id) { await api(`/repos/${id}/index`, { method: 'POST' }); alert('Indexing started.'); }
async function copyText(value, button) {
  await navigator.clipboard.writeText(value);
  const label = button.textContent;
  button.textContent = 'Copied';
  setTimeout(() => button.textContent = label, 1200);
}
async function refreshAll() { await loadAuth(); }
go(1); refreshAll().catch(e => { $('authStatus').innerHTML = `<b class="bad">${e.message}</b>`; });
</script>
</body>
</html>"#;

pub(crate) fn overall_percent(stage: &str, processed: i64, total: i64) -> i64 {
    let stage = if stage == "parsing_ast" {
        "parsing_imports"
    } else {
        stage
    };
    let stages: [(&str, i64); 7] = [
        ("fetching", 5),
        ("reading_tree", 5),
        ("classifying", 10),
        ("detecting_packages", 10),
        ("parsing_imports", 60),
        ("writing", 5),
        ("ready", 5),
    ];
    let mut base = 0i64;
    for (name, weight) in stages {
        if name == stage {
            let fraction = if total > 0 {
                (processed as f64 / total as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return (base as f64 + weight as f64 * fraction).round() as i64;
        }
        base += weight;
    }
    base.min(100)
}

#[cfg(test)]
mod branding_tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn setup_branding_embeds_shared_svg_without_asset_routes() {
        for template in [ONBOARDING_HTML, include_str!("../../../assets/onboarding.html")] {
            let html = embed_brand_asset(template.to_string());
            assert!(html.contains("rel=\"icon\" type=\"image/svg+xml\""));
            assert!(html.contains("width=\"64\" height=\"48\" alt=\"\" aria-hidden=\"true\""));
            assert!(!html.contains(".logo::before"));
            assert!(!html.contains("\"/assets/mascot.svg\""));
            let images: Vec<_> = html.split("data:image/svg+xml;base64,").skip(1).collect();
            assert_eq!(images.len(), 2);
            let expected: [&[u8]; 2] = [
                include_bytes!("../../../assets/favicon.svg"),
                include_bytes!("../../../assets/mascot.svg"),
            ];
            for (image, expected) in images.into_iter().zip(expected) {
                let encoded = image.split('"').next().unwrap();
                let decoded = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
                assert_eq!(decoded, expected);
            }
            assert!(!html.contains("\"/assets/favicon.svg\""));
        }
    }
}
