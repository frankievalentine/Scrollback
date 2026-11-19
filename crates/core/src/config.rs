//! Settings the user can change, kept as a small JSON file next to the index.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Write summaries without being asked, once a session has gone quiet.
    #[serde(default = "yes")]
    pub auto_summarize: bool,
    /// Whether the first-run screen has been seen and answered. Until it
    /// has, nothing is summarised unasked.
    #[serde(default)]
    pub onboarded: bool,
}

fn yes() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config { auto_summarize: true, onboarded: false }
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

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(crate::data_dir())?;
        std::fs::write(Config::path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
