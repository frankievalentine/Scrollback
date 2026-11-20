//! Who writes the summaries. Each provider takes the same instructions, a
//! JSON schema and the condensed session, and returns a JSON object that
//! matches the schema.
//!
//! - **Claude**: the Claude Code command line, with the login it already
//!   has. Haiku only.
//! - **Ollama**: a model running on this computer. Nothing leaves it.
//! - **OpenAI-compatible**: any service that speaks OpenAI's chat API:
//!   OpenAI itself, OpenCode Go, Vercel AI Gateway, Cloudflare AI Gateway,
//!   or another address. Needs an API key, kept in the system keychain.

use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

/// How long one summary may take before it is given up on. Local models on
/// a laptop can be slow, so this is generous.
const TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Provider {
    #[default]
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "ollama")]
    Ollama,
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Claude, Provider::Ollama, Provider::OpenAiCompatible];

    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Ollama => "Ollama",
            Provider::OpenAiCompatible => "OpenAI-compatible",
        }
    }
}

/// The services an OpenAI-compatible provider is usually pointed at, each
/// filling in its address and a model to start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Endpoint {
    #[default]
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "opencode-go")]
    OpenCodeGo,
    #[serde(rename = "vercel")]
    Vercel,
    #[serde(rename = "cloudflare")]
    Cloudflare,
    #[serde(rename = "custom")]
    Custom,
}

impl Endpoint {
    pub const ALL: [Endpoint; 5] =
        [Endpoint::OpenAi, Endpoint::OpenCodeGo, Endpoint::Vercel, Endpoint::Cloudflare, Endpoint::Custom];

    pub fn name(self) -> &'static str {
        match self {
            Endpoint::OpenAi => "OpenAI",
            Endpoint::OpenCodeGo => "OpenCode Go",
            Endpoint::Vercel => "Vercel AI Gateway",
            Endpoint::Cloudflare => "Cloudflare AI Gateway",
            Endpoint::Custom => "Other",
        }
    }

    /// The name as a choice among the others, where space is short.
    pub fn short_name(self) -> &'static str {
        match self {
            Endpoint::Vercel => "Vercel",
            Endpoint::Cloudflare => "Cloudflare",
            other => other.name(),
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Endpoint::OpenAi => "openai",
            Endpoint::OpenCodeGo => "opencode-go",
            Endpoint::Vercel => "vercel",
            Endpoint::Cloudflare => "cloudflare",
            Endpoint::Custom => "custom",
        }
    }

    /// The address chat requests go to, ending before `/chat/completions`.
    /// Cloudflare's names the account, so it is a pattern to fill in.
    pub fn default_url(self) -> &'static str {
        match self {
            Endpoint::OpenAi => "https://api.openai.com/v1",
            Endpoint::OpenCodeGo => "https://opencode.ai/inference/go/openai/v1",
            Endpoint::Vercel => "https://ai-gateway.vercel.sh/v1",
            Endpoint::Cloudflare => "https://api.cloudflare.com/client/v4/accounts/ACCOUNT_ID/ai/v1",
            Endpoint::Custom => "",
        }
    }

    /// A small, quick model to start from: GPT Luna, OpenAI's most
    /// efficient, where the service names it reliably. Others are picked
    /// from the service's own list.
    pub fn default_model(self) -> &'static str {
        match self {
            Endpoint::OpenAi => "gpt-6-luna",
            Endpoint::Cloudflare => "openai/gpt-6-luna",
            Endpoint::OpenCodeGo | Endpoint::Vercel | Endpoint::Custom => "",
        }
    }

    /// Whether the service lists its models at `/models`.
    pub fn lists_models(self) -> bool {
        !matches!(self, Endpoint::Cloudflare)
    }
}

/// The Claude models a summary can be written with: the Haiku models
/// Anthropic still serves, newest first, by the names the Claude command
/// line accepts for `--model`.
pub const CLAUDE_MODELS: [(&str, &str); 2] = [("claude-haiku-5-5", "Haiku 5.5"), ("claude-haiku-4-5", "Haiku 4.5")];

/// The default address of Ollama on this computer, or `OLLAMA_HOST`.
pub fn default_ollama_url() -> String {
    match std::env::var("OLLAMA_HOST").ok().filter(|h| !h.trim().is_empty()) {
        Some(host) if host.starts_with("http") => host,
        Some(host) => format!("http://{host}"),
        None => "http://127.0.0.1:11434".to_string(),
    }
}

/// Whether summaries can be written with the chosen provider, and if not,
/// why, in words for the settings.
pub fn ready(config: &Config) -> Result<(), String> {
    if std::env::var_os("SCROLLBACK_NO_CLAUDE").is_some() {
        return Err("Summaries are switched off for this run.".to_string());
    }
    match config.summary_provider {
        Provider::Claude => match crate::summarize::claude_binary() {
            Some(_) => Ok(()),
            None => Err("The claude command was not found. Install Claude Code, or choose another provider.".to_string()),
        },
        Provider::Ollama => match config.ollama_model.trim() {
            "" => Err("Choose an Ollama model.".to_string()),
            _ => Ok(()),
        },
        Provider::OpenAiCompatible => {
            let url = config.endpoint_url();
            if url.trim().is_empty() {
                return Err("Fill in the address.".to_string());
            }
            if url.contains("ACCOUNT_ID") {
                return Err("Put your Cloudflare account id in the address.".to_string());
            }
            if config.endpoint_model().trim().is_empty() {
                return Err("Fill in a model.".to_string());
            }
            if crate::secrets::api_key(config.endpoint).is_none() {
                return Err("Add an API key.".to_string());
            }
            Ok(())
        }
    }
}

/// Ask the chosen provider. Returns the JSON object and, when the provider
/// says, what the request cost.
pub fn ask(instructions: &str, schema: &Value, input: &str) -> Result<(Value, f64)> {
    let config = Config::load();
    match config.summary_provider {
        Provider::Claude => crate::summarize::ask_claude(&config, instructions, schema, input),
        Provider::Ollama => ollama(&config, instructions, schema, input),
        Provider::OpenAiCompatible => openai(&config, instructions, schema, input),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent("Scrollback")
        .build()
        .into()
}

/// Ollama's chat API, with the schema as the reply's required format.
fn ollama(config: &Config, instructions: &str, schema: &Value, input: &str) -> Result<(Value, f64)> {
    let url = format!("{}/api/chat", config.ollama_url().trim_end_matches('/'));
    let body = json!({
        "model": config.ollama_model,
        "messages": [
            {"role": "system", "content": instructions},
            {"role": "user", "content": input},
        ],
        "format": schema,
        "stream": false,
        // The condensed session runs to about 7,000 tokens; Ollama's default
        // context is smaller and would cut it short.
        "options": {"num_ctx": 16384, "temperature": 0.2},
    });
    let mut response = agent()
        .post(&url)
        .send_json(&body)
        .with_context(|| format!("reaching Ollama at {}", config.ollama_url()))?;
    let status = response.status().as_u16();
    let reply: Value = response.body_mut().read_json().context("reading Ollama's reply")?;
    if status >= 400 {
        bail!("Ollama said: {}", error_text(&reply));
    }
    let content = reply["message"]["content"].as_str().unwrap_or("");
    Ok((parse_object(content)?, 0.0))
}

/// OpenAI's chat completions, asking for the schema as structured output.
/// A service that refuses structured output is asked again for plain JSON,
/// with the schema in the instructions.
fn openai(config: &Config, instructions: &str, schema: &Value, input: &str) -> Result<(Value, f64)> {
    let key = crate::secrets::api_key(config.endpoint).ok_or_else(|| anyhow!("no API key is saved"))?;
    let url = format!("{}/chat/completions", config.endpoint_url().trim_end_matches('/'));
    let messages = |system: String| {
        json!([
            {"role": "system", "content": system},
            {"role": "user", "content": input},
        ])
    };
    let strict = json!({
        "model": config.endpoint_model(),
        "messages": messages(instructions.to_string()),
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": "summary", "strict": true, "schema": schema},
        },
    });
    let request = |body: &Value| -> Result<(u16, Value)> {
        let mut builder = agent().post(&url).header("Authorization", &format!("Bearer {key}"));
        // Cloudflare routes through a named AI Gateway when one is given.
        if config.endpoint == Endpoint::Cloudflare && !config.cloudflare_gateway.trim().is_empty() {
            builder = builder.header("cf-aig-gateway-id", config.cloudflare_gateway.trim());
        }
        let mut response = builder.send_json(body).with_context(|| format!("reaching {}", config.endpoint.name()))?;
        let status = response.status().as_u16();
        let reply: Value = response.body_mut().read_json().context("reading the reply")?;
        Ok((status, reply))
    };
    let (mut status, mut reply) = request(&strict)?;
    if status == 400 && error_text(&reply).to_lowercase().contains("response_format") {
        let loose = json!({
            "model": config.endpoint_model(),
            "messages": messages(format!(
                "{instructions}\n\nReply with only a JSON object that matches this JSON schema:\n{schema}"
            )),
        });
        (status, reply) = request(&loose)?;
    }
    if status >= 400 {
        bail!("{} said: {}", config.endpoint.name(), error_text(&reply));
    }
    let content = reply["choices"][0]["message"]["content"].as_str().unwrap_or("");
    let cost = reply["usage"]["cost"].as_f64().unwrap_or(0.0);
    Ok((parse_object(content)?, cost))
}

/// The models an OpenAI-compatible service offers, by id.
pub fn endpoint_models(config: &Config) -> Result<Vec<String>> {
    let key = crate::secrets::api_key(config.endpoint).ok_or_else(|| anyhow!("add an API key first"))?;
    let url = format!("{}/models", config.endpoint_url().trim_end_matches('/'));
    let mut response = agent()
        .get(&url)
        .header("Authorization", &format!("Bearer {key}"))
        .call()
        .with_context(|| format!("reaching {}", config.endpoint.name()))?;
    let status = response.status().as_u16();
    let reply: Value = response.body_mut().read_json()?;
    if status >= 400 {
        bail!("{}", error_text(&reply));
    }
    let mut ids: Vec<String> = reply["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["id"].as_str().map(str::to_string))
        .collect();
    ids.sort();
    Ok(ids)
}

/// The models Ollama has, by name.
pub fn ollama_models(url: &str) -> Result<Vec<String>> {
    let mut response = agent()
        .get(&format!("{}/api/tags", url.trim_end_matches('/')))
        .call()
        .with_context(|| format!("reaching Ollama at {url}"))?;
    let reply: Value = response.body_mut().read_json()?;
    let mut names: Vec<String> = reply["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| model["name"].as_str().map(str::to_string))
        .collect();
    names.sort();
    Ok(names)
}

/// The reply's text as one JSON object. Models asked for plain JSON
/// sometimes wrap it in a code fence or a sentence, which is cut away.
fn parse_object(text: &str) -> Result<Value> {
    let text = text.trim();
    if let Ok(value @ Value::Object(_)) = serde_json::from_str::<Value>(text) {
        return Ok(value);
    }
    let start = text.find('{');
    let end = text.rfind('}');
    if let (Some(start), Some(end)) = (start, end) {
        if let Ok(value @ Value::Object(_)) = serde_json::from_str::<Value>(&text[start..=end]) {
            return Ok(value);
        }
    }
    bail!("the reply was not a JSON object: {}", crate::squash(text, 200))
}

/// The message in an error reply, whatever shape the service uses.
fn error_text(reply: &Value) -> String {
    let message = reply["error"]["message"]
        .as_str()
        .or_else(|| reply["error"].as_str())
        .or_else(|| reply["message"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| reply.to_string());
    crate::squash(&message, 300)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_object_in_a_wrapped_reply() {
        assert_eq!(parse_object(r#"{"a":1}"#).unwrap(), json!({"a": 1}));
        assert_eq!(parse_object("```json\n{\"a\": [1, 2]}\n```").unwrap(), json!({"a": [1, 2]}));
        assert_eq!(parse_object("Here it is: {\"a\": \"}\"} Done.").unwrap(), json!({"a": "}"}));
        assert!(parse_object("no json here").is_err());
    }

    #[test]
    fn names_in_the_settings_file_match_the_slugs() {
        for endpoint in Endpoint::ALL {
            assert_eq!(serde_json::to_value(endpoint).unwrap(), json!(endpoint.slug()));
        }
        assert_eq!(serde_json::to_value(Provider::OpenAiCompatible).unwrap(), json!("openai-compatible"));
    }

    #[test]
    fn reads_error_messages_in_either_shape() {
        assert_eq!(error_text(&json!({"error": {"message": "bad model"}})), "bad model");
        assert_eq!(error_text(&json!({"error": "model not found"})), "model not found");
    }
}
