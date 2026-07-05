//! Registry credentials as sent by the CLI in the `X-Registry-Auth` header.

use super::*;
use serde_json::Value;

/// Credentials for a registry, as sent by the CLI in the `X-Registry-Auth` header.
#[derive(Clone, Default)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}
impl Credentials {
    /// Anonymous credentials (public registries).
    pub fn none() -> Credentials {
        Credentials::default()
    }

    /// Decode docker's base64-encoded `X-Registry-Auth` JSON (`{username,password,...}`).
    pub fn from_x_registry_auth(b64: &str) -> Option<Credentials> {
        let json = base64_decode(b64.trim())?;
        let v: Value = serde_json::from_slice(&json).ok()?;
        Some(Credentials {
            username: v["username"].as_str().unwrap_or_default().to_string(),
            password: v["password"].as_str().unwrap_or_default().to_string(),
        })
    }
    pub(super) fn is_empty(&self) -> bool {
        self.username.is_empty() && self.password.is_empty()
    }
}
