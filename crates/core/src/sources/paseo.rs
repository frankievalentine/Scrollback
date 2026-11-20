//! Paseo, an app that runs other coding agents (Claude Code, Codex,
//! OpenCode, Copilot, pi). It keeps no conversations of its own: each
//! agent's conversation is in that agent's usual files, which are already
//! read. What Paseo adds is which of those sessions it started, and the
//! title it gave each, in `~/.paseo/agents/<folder>/<agent-id>.json` (or
//! under `$PASEO_HOME`): `{title, provider, cwd, persistence: {sessionId}}`.

use crate::sources::t3code::Binding;
use crate::sources::walk;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn root() -> PathBuf {
    crate::env_dir("PASEO_HOME").unwrap_or_else(|| crate::home().join(".paseo"))
}

pub fn present() -> bool {
    root().join("agents").is_dir()
}

/// Sessions started from Paseo, by the agent's own session id.
pub fn bindings() -> Vec<Binding> {
    let files = walk(&root().join("agents"), &|path: &Path| path.extension().is_some_and(|e| e == "json"));
    files
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
        .filter_map(|agent| binding(&agent))
        .collect()
}

fn binding(agent: &Value) -> Option<Binding> {
    let native_id = agent["persistence"]["sessionId"]
        .as_str()
        .or_else(|| agent["runtimeInfo"]["sessionId"].as_str())
        .filter(|id| !id.is_empty())?;
    Some(Binding {
        native_id: native_id.to_string(),
        title: agent["title"].as_str().unwrap_or("").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_agent_session_behind_a_record() {
        let agent: Value = serde_json::from_str(
            r#"{"id":"a1","provider":"claude","cwd":"/p/app","title":"Tidy the parser",
                "persistence":{"provider":"claude","sessionId":"0afe6d6b-244a"}}"#,
        )
        .unwrap();
        let found = binding(&agent).unwrap();
        assert_eq!(found.native_id, "0afe6d6b-244a");
        assert_eq!(found.title, "Tidy the parser");
    }
}
