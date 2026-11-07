//! The model and reasoning effort each provider reviews with. Settings keep one choice per
//! provider, so a review that moves to another provider's account uses that provider's choice.
//!
//! Catalogues come from each CLI's [`crate::providers::provider::ProviderCli`] discovery, not a
//! compile-time list.

use crate::providers::provider::{self, DiscoveredModel};

pub(crate) type EngineModel = DiscoveredModel;

/// Display names for every registered review provider.
pub(crate) fn providers() -> Vec<(&'static str, &'static str)> {
    provider::registered()
        .iter()
        .map(|cli| (cli.id(), cli.display_name()))
        .collect()
}

pub(crate) fn models(provider: &str) -> Vec<EngineModel> {
    provider::catalog(provider)
}

/// Every model id OpenCodeReview can run, for the runner catalogue.
pub(crate) fn all_model_ids() -> Vec<String> {
    provider::all_model_ids()
}

/// The provider's default model and effort (first discovered entry).
pub(crate) fn default_engine(provider: &str) -> (String, String) {
    models(provider)
        .into_iter()
        .next()
        .map(|model| (model.id, model.default_effort))
        .unwrap_or_default()
}

/// Checks `model` and `effort` for `provider` and returns them, an empty effort becoming the
/// model's default.
pub(crate) fn resolve(provider: &str, model: &str, effort: &str) -> Result<(String, String), String> {
    let catalogue = models(provider);
    let entry = catalogue
        .iter()
        .find(|entry| entry.id == model)
        .ok_or_else(|| format!("{model} is not a model OpenCodeReview runs on {provider}"))?;
    let effort = if effort.is_empty() {
        entry.default_effort.as_str()
    } else {
        effort
    };
    if entry.efforts.is_empty() && effort.is_empty() || entry.efforts.iter().any(|known| known == effort) {
        Ok((entry.id.clone(), effort.to_string()))
    } else {
        Err(format!("{} doesn't take a {effort} effort", entry.label))
    }
}

/// "Claude · Opus 5.5 · medium", as reviews show the engine they ran on.
pub(crate) fn label(provider: &str, model: &str, effort: &str) -> String {
    let name = provider::by_id(provider)
        .map(|cli| cli.display_name())
        .unwrap_or(provider);
    let model = models(provider)
        .into_iter()
        .find(|entry| entry.id == model)
        .map(|entry| entry.label)
        .unwrap_or_else(|| model.to_string());
    [name, model.as_str(), effort]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The settings key holding `provider`'s choice.
pub(crate) fn setting_key(provider: &str) -> String {
    format!("review_engine_{provider}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::provider::{self, DiscoveredModel};

    fn seed() {
        provider::invalidate_cache();
        provider::set_test_catalog(
            "claude",
            vec![DiscoveredModel {
                id: "claude-opus-5-5".into(),
                label: "Opus 5.5".into(),
                efforts: vec!["low".into(), "medium".into(), "high".into(), "max".into()],
                default_effort: "medium".into(),
            }],
        );
        provider::set_test_catalog(
            "codex",
            vec![
                DiscoveredModel {
                    id: "default".into(),
                    label: "Codex default".into(),
                    efforts: vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()],
                    default_effort: "medium".into(),
                },
                DiscoveredModel {
                    id: "gpt-5.5".into(),
                    label: "GPT-5.5".into(),
                    efforts: vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()],
                    default_effort: "medium".into(),
                },
            ],
        );
    }

    #[test]
    fn every_provider_defaults_to_a_valid_engine() {
        seed();
        for (provider, _) in providers() {
            let (model, effort) = default_engine(provider);
            assert_eq!(resolve(provider, &model, &effort).unwrap(), (model, effort));
        }
    }

    #[test]
    fn catalogue_lists_discovered_engines() {
        seed();
        let ids = all_model_ids();
        assert!(ids.iter().any(|id| id == "claude-opus-5-5"));
        assert!(ids.iter().any(|id| id == "default"));
        assert!(ids.iter().any(|id| id == "gpt-5.5"));
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn resolve_rejects_unknown_models_and_efforts() {
        seed();
        assert_eq!(
            resolve("claude", "claude-opus-5-5", "").unwrap(),
            ("claude-opus-5-5".to_string(), "medium".to_string())
        );
        assert!(resolve("claude", "gpt-5.5", "medium").is_err());
        assert!(resolve("codex", "gpt-5.5", "ultra").is_err());
    }

    #[test]
    fn labels_name_the_provider_model_and_effort() {
        seed();
        assert_eq!(label("claude", "claude-opus-5-5", "high"), "Claude · Opus 5.5 · high");
    }
}
