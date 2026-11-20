//! Settings the user can change, kept as a small JSON file next to the index.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::provider::{Endpoint, Provider, CLAUDE_MODELS};

/// How long a session can be asked to stay quiet before it is summarised.
pub const QUIET_MINUTES: [u32; 4] = [5, 10, 30, 60];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// Write summaries without being asked, once a session has gone quiet.
    #[serde(default = "yes")]
    pub auto_summarize: bool,
    /// Whether the first-run screen has been seen and answered. Until it
    /// has, nothing is summarised unasked.
    #[serde(default)]
    pub onboarded: bool,
    /// Minutes a session must be quiet before it counts as finished and its
    /// summary is written unasked.
    #[serde(default = "ten")]
    pub quiet_minutes: u32,
    /// Also summarise a session while it is still in use, and bring that
    /// summary up to date every so often, so the session can be found by
    /// what it is about before it ends.
    #[serde(default = "yes")]
    pub live_summaries: bool,
    /// Who writes summaries.
    #[serde(default)]
    pub summary_provider: Provider,
    /// The Claude model, one of `CLAUDE_MODELS`.
    #[serde(default = "haiku")]
    pub summary_model: String,
    /// Where Ollama is, when not its usual address.
    #[serde(default)]
    pub ollama_url: String,
    /// The Ollama model, by the name Ollama lists it under.
    #[serde(default)]
    pub ollama_model: String,
    /// Which OpenAI-compatible service, and for each the address and model
    /// chosen, when not the service's defaults. The API key is kept in the
    /// system keychain, not here.
    #[serde(default)]
    pub endpoint: Endpoint,
    #[serde(default)]
    pub endpoint_urls: BTreeMap<String, String>,
    #[serde(default)]
    pub endpoint_models: BTreeMap<String, String>,
    /// Cloudflare: the AI Gateway to route through, if any.
    #[serde(default)]
    pub cloudflare_gateway: String,
    /// `system`, `light` or `dark`.
    #[serde(default = "system")]
    pub theme: String,
    /// Sources left out of the app, by `Origin` slug.
    #[serde(default)]
    pub hidden_sources: Vec<String>,
    /// Folders to read a harness's sessions from instead of the usual one,
    /// by `Harness` slug.
    #[serde(default)]
    pub folders: BTreeMap<String, String>,
    /// Show the icon in the menu bar, and keep running there when the
    /// window is closed.
    #[serde(default = "yes")]
    pub menu_bar_icon: bool,
}

fn yes() -> bool {
    true
}

fn ten() -> u32 {
    10
}

fn haiku() -> String {
    CLAUDE_MODELS[0].0.to_string()
}

fn system() -> String {
    "system".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            auto_summarize: true,
            onboarded: false,
            quiet_minutes: ten(),
            live_summaries: true,
            summary_provider: Provider::Claude,
            summary_model: haiku(),
            ollama_url: String::new(),
            ollama_model: String::new(),
            endpoint: Endpoint::OpenAi,
            endpoint_urls: BTreeMap::new(),
            endpoint_models: BTreeMap::new(),
            cloudflare_gateway: String::new(),
            theme: system(),
            hidden_sources: Vec::new(),
            folders: BTreeMap::new(),
            menu_bar_icon: true,
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        crate::data_dir().join("config.json")
    }

    /// The saved settings, or the defaults when none are saved or the file
    /// cannot be read.
    pub fn load() -> Config {
        let Ok(text) = std::fs::read_to_string(Config::path()) else {
            return Config::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|_| Config::lenient(&text))
    }

    /// Settings that do not all read, as when a later version wrote a value
    /// this one does not know: each setting that does read is kept, rather
    /// than losing them all to one.
    fn lenient(text: &str) -> Config {
        let Ok(serde_json::Value::Object(saved)) = serde_json::from_str::<serde_json::Value>(text) else {
            return Config::default();
        };
        let mut config = serde_json::to_value(Config::default()).expect("the defaults serialise");
        for (key, value) in saved {
            let mut trial = config.clone();
            trial[&key] = value;
            if serde_json::from_value::<Config>(trial.clone()).is_ok() {
                config = trial;
            }
        }
        serde_json::from_value(config).unwrap_or_default()
    }

    /// The Claude model to ask for. A saved choice this version no longer
    /// offers, such as Sonnet or Opus from an earlier one, becomes Haiku.
    pub fn model(&self) -> &str {
        CLAUDE_MODELS
            .iter()
            .map(|(id, _)| *id)
            .find(|known| *known == self.summary_model)
            .unwrap_or(CLAUDE_MODELS[0].0)
    }

    pub fn ollama_url(&self) -> String {
        match self.ollama_url.trim() {
            "" => crate::provider::default_ollama_url(),
            url => url.to_string(),
        }
    }

    /// The chosen service's address: the one saved, or its default.
    pub fn endpoint_url(&self) -> String {
        self.endpoint_urls
            .get(self.endpoint.slug())
            .filter(|url| !url.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| self.endpoint.default_url().to_string())
    }

    pub fn endpoint_model(&self) -> String {
        self.endpoint_models
            .get(self.endpoint.slug())
            .filter(|model| !model.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| self.endpoint.default_model().to_string())
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(crate::data_dir())?;
        std::fs::write(Config::path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_value_costs_only_its_own_setting() {
        let config = Config::lenient(r#"{"onboarded": true, "summary_provider": "something-new", "quiet_minutes": 30}"#);
        assert!(config.onboarded);
        assert_eq!(config.quiet_minutes, 30);
        assert_eq!(config.summary_provider, Provider::Claude);
    }
}
