//! API keys for summary providers, kept in the system's own keychain:
//! Keychain on macOS, Credential Manager on Windows, the Secret Service on
//! Linux. They are never written to Scrollback's settings file.
//!
//! A key in the usual environment variable for a service (`OPENAI_API_KEY`,
//! `AI_GATEWAY_API_KEY`) is used when none is saved, so people who already
//! set one need not paste it again.

use crate::provider::Endpoint;

const SERVICE: &str = "Scrollback";

fn entry(name: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(SERVICE, name).ok()
}

fn read(name: &str) -> Option<String> {
    entry(name)?.get_password().ok().filter(|key| !key.trim().is_empty())
}

fn write(name: &str, value: Option<&str>) -> Result<(), String> {
    let entry = entry(name).ok_or("the system keychain could not be opened")?;
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(value) => entry.set_password(value).map_err(|err| err.to_string()),
        None => match entry.delete_credential() {
            Err(keyring::Error::NoEntry) | Ok(()) => Ok(()),
            Err(err) => Err(err.to_string()),
        },
    }
}

fn key_name(endpoint: Endpoint) -> String {
    format!("api-key-{}", endpoint.slug())
}

/// The environment variable a service's own tools read its key from.
fn env_name(endpoint: Endpoint) -> Option<&'static str> {
    match endpoint {
        Endpoint::OpenAi => Some("OPENAI_API_KEY"),
        Endpoint::Vercel => Some("AI_GATEWAY_API_KEY"),
        Endpoint::OpenCodeGo | Endpoint::Cloudflare | Endpoint::Custom => None,
    }
}

/// The key for a service: the saved one, or its usual environment variable.
pub fn api_key(endpoint: Endpoint) -> Option<String> {
    read(&key_name(endpoint)).or_else(|| {
        env_name(endpoint)
            .and_then(|name| std::env::var(name).ok())
            .filter(|key| !key.trim().is_empty())
    })
}

/// Whether a key is saved in the keychain, as opposed to coming from the
/// environment or missing.
pub fn api_key_saved(endpoint: Endpoint) -> bool {
    read(&key_name(endpoint)).is_some()
}

/// Save a key, or remove it with `None`.
pub fn set_api_key(endpoint: Endpoint, key: Option<&str>) -> Result<(), String> {
    write(&key_name(endpoint), key)
}
