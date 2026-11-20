//! Settings the user can change, kept as a small JSON file next to the index.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The models a summary can be written with, cheapest first. These are the
/// names the Claude CLI accepts for `--model`.
pub const SUMMARY_MODELS: [&str; 3] = ["haiku", "sonnet", "opus"];

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
    /// Minutes a session must be quiet before it is summarised unasked, so
    /// one still in use is not summarised mid-thought.
    #[serde(default = "ten")]
    pub quiet_minutes: u32,
    /// Which model writes summaries; one of `SUMMARY_MODELS`.
    #[serde(default = "haiku")]
    pub summary_model: String,
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
    SUMMARY_MODELS[0].to_string()
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
            summary_model: haiku(),
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
        std::fs::read_to_string(Config::path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// The model to ask for, falling back to the cheapest when the saved
    /// name is not one this version knows.
    pub fn model(&self) -> &str {
        SUMMARY_MODELS
            .iter()
            .find(|known| **known == self.summary_model)
            .copied()
            .unwrap_or(SUMMARY_MODELS[0])
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(crate::data_dir())?;
        std::fs::write(Config::path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
