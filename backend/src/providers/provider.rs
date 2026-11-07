//! The CLI provider interface: each AI CLI OpenCodeReview can run implements discovery the same
//! way, so a new CLI registers here without touching the engine picker or settings.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A model the provider's connection can review with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveredModel {
    pub id: String,
    pub label: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
}

/// Credential material discovery needs; `None` means no signed-in account for that provider.
#[derive(Clone, Debug)]
pub(crate) enum ProviderAuth {
    Claude {
        token: String,
    },
    Codex {
        auth_json: String,
    },
    Grok {
        auth_json: Option<String>,
        api_key: Option<String>,
    },
    None,
}

pub(crate) trait ProviderCli: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn discover_models(&self, auth: &ProviderAuth) -> Result<Vec<DiscoveredModel>, String>;
}

struct ClaudeProvider;
struct CodexProvider;
struct GrokProvider;

impl ProviderCli for ClaudeProvider {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn display_name(&self) -> &'static str {
        "Claude"
    }

    fn discover_models(&self, auth: &ProviderAuth) -> Result<Vec<DiscoveredModel>, String> {
        crate::providers::claude::discover_models(auth)
    }
}

impl ProviderCli for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn display_name(&self) -> &'static str {
        "ChatGPT"
    }

    fn discover_models(&self, auth: &ProviderAuth) -> Result<Vec<DiscoveredModel>, String> {
        crate::providers::codex::discover_models(auth)
    }
}

impl ProviderCli for GrokProvider {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn display_name(&self) -> &'static str {
        "Grok"
    }

    fn discover_models(&self, auth: &ProviderAuth) -> Result<Vec<DiscoveredModel>, String> {
        crate::providers::grok::discover_models(auth)
    }
}

static CLAUDE: ClaudeProvider = ClaudeProvider;
static CODEX: CodexProvider = CodexProvider;
static GROK: GrokProvider = GrokProvider;
static PROVIDERS: [&dyn ProviderCli; 3] = [&CLAUDE, &CODEX, &GROK];

/// Every CLI OpenCodeReview reviews with.
pub(crate) fn registered() -> &'static [&'static dyn ProviderCli] {
    &PROVIDERS
}

pub(crate) fn by_id(id: &str) -> Option<&'static dyn ProviderCli> {
    registered().iter().copied().find(|cli| cli.id() == id)
}

const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

struct CacheEntry {
    account: Option<i64>,
    models: Vec<DiscoveredModel>,
    fetched_at: Instant,
}

static CACHE: OnceLock<Mutex<Vec<(String, CacheEntry)>>> = OnceLock::new();

fn cache() -> &'static Mutex<Vec<(String, CacheEntry)>> {
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Drops every cached catalogue so the next read asks the CLIs again.
pub(crate) fn invalidate_cache() {
    if let Ok(mut entries) = cache().lock() {
        entries.clear();
    }
}

/// Installs a catalogue for tests without calling a CLI or the network.
#[cfg(test)]
pub(crate) fn set_test_catalog(provider: &str, models: Vec<DiscoveredModel>) {
    if let Ok(mut entries) = cache().lock() {
        entries.retain(|(id, _)| id != provider);
        entries.push((
            provider.to_string(),
            CacheEntry {
                account: None,
                models,
                fetched_at: Instant::now(),
            },
        ));
    }
}

/// Runner projection roots (env override, then the image default when that directory exists).
fn runner_auth_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    if let Ok(root) = std::env::var("OCR_RUNNER_AUTH_ROOT") {
        roots.push(std::path::PathBuf::from(root));
    }
    let default = std::path::PathBuf::from("/claude-auth/app-managed");
    if !roots.contains(&default) && default.join("accounts").is_dir() {
        roots.push(default);
    }
    roots
}

/// Account ids that may hold credentials: server store plus the runner projection.
pub(crate) fn discoverable_account_ids() -> Vec<i64> {
    let mut ids = std::collections::BTreeSet::new();
    for id in crate::accounts::stored_ids() {
        ids.insert(id);
    }
    for root in runner_auth_roots() {
        if let Ok(entries) = std::fs::read_dir(root.join("accounts")) {
            for entry in entries.flatten() {
                if let Some(id) = entry.file_name().to_str().and_then(|name| name.parse().ok()) {
                    ids.insert(id);
                }
            }
        }
    }
    ids.into_iter().collect()
}

fn claude_token_for(id: i64) -> Option<String> {
    runner_auth_roots()
        .into_iter()
        .map(|root| root.join(format!("accounts/{id}/claude/oauth-token")))
        .chain(std::iter::once(crate::accounts::claude_token_path(id)))
        .find_map(|path| crate::providers::claude::read_token(&path).ok())
}

fn codex_auth_for(id: i64) -> Option<String> {
    runner_auth_roots()
        .into_iter()
        .map(|root| root.join(format!("accounts/{id}/codex/auth.json")))
        .chain(std::iter::once(crate::accounts::codex_auth_path(id)))
        .find_map(|path| {
            std::fs::read_to_string(path)
                .ok()
                .filter(|raw| crate::providers::codex::validate_auth(raw).is_ok())
        })
}

fn grok_auth_for(id: i64) -> (Option<String>, Option<String>) {
    let auth_json = runner_auth_roots()
        .into_iter()
        .map(|root| root.join(format!("accounts/{id}/grok/auth.json")))
        .chain(std::iter::once(crate::accounts::grok_auth_path(id)))
        .find_map(|path| {
            std::fs::read_to_string(path)
                .ok()
                .filter(|raw| crate::providers::grok::validate_auth(raw).is_ok())
        });
    let api_key = runner_auth_roots()
        .into_iter()
        .map(|root| root.join(format!("accounts/{id}/grok/api-key")))
        .chain(std::iter::once(crate::accounts::grok_api_key_path(id)))
        .find_map(|path| crate::providers::grok::read_api_key(&path).ok());
    (auth_json, api_key)
}

/// First signed-in account for `provider`, and the auth material discovery needs.
pub(crate) fn first_auth(provider: &str) -> (Option<i64>, ProviderAuth) {
    for id in discoverable_account_ids() {
        match provider {
            "claude" => {
                if let Some(token) = claude_token_for(id) {
                    return (Some(id), ProviderAuth::Claude { token });
                }
            }
            "codex" => {
                if let Some(auth_json) = codex_auth_for(id) {
                    return (Some(id), ProviderAuth::Codex { auth_json });
                }
            }
            "grok" => {
                let (auth_json, api_key) = grok_auth_for(id);
                if auth_json.is_some() || api_key.is_some() {
                    return (Some(id), ProviderAuth::Grok { auth_json, api_key });
                }
            }
            _ => {}
        }
    }
    (None, ProviderAuth::None)
}

/// Live catalogue for `provider`, cached briefly. Falls back to the last good cache when discovery
/// fails so a transient outage does not empty the picker mid-review.
pub(crate) fn catalog(provider: &str) -> Vec<DiscoveredModel> {
    let (account, auth) = first_auth(provider);
    if let Ok(entries) = cache().lock() {
        if let Some(entry) = entries
            .iter()
            .find(|(id, entry)| id == provider && entry.account == account && entry.fetched_at.elapsed() < CACHE_TTL)
        {
            return entry.1.models.clone();
        }
        if let Some(entry) = entries.iter().find(|(id, _)| id == provider) {
            // Stale but same provider: refresh below; keep as fallback if refresh fails.
            let stale = entry.1.models.clone();
            drop(entries);
            return refresh(provider, account, &auth).unwrap_or(stale);
        }
    }
    refresh(provider, account, &auth).unwrap_or_default()
}

fn refresh(provider: &str, account: Option<i64>, auth: &ProviderAuth) -> Result<Vec<DiscoveredModel>, String> {
    let cli = by_id(provider).ok_or_else(|| format!("{provider} is not a review provider"))?;
    let models = cli.discover_models(auth)?;
    if let Ok(mut entries) = cache().lock() {
        entries.retain(|(id, _)| id != provider);
        entries.push((
            provider.to_string(),
            CacheEntry {
                account,
                models: models.clone(),
                fetched_at: Instant::now(),
            },
        ));
    }
    Ok(models)
}

/// Every model id any registered provider currently offers (for the runner catalogue).
pub(crate) fn all_model_ids() -> Vec<String> {
    registered()
        .iter()
        .flat_map(|cli| catalog(cli.id()).into_iter().map(|model| model.id))
        .collect()
}
