//! The registry session service: pull/push/fetch_config over the bearer-challenge auth flow.
//!
//! `impl Client` is split by FLOW across sibling files — [`pull`] (manifest/blob download + unpack),
//! [`push`] (blob upload + manifest PUT) and [`auth`] (the Bearer-challenge token dance) — while this
//! `mod.rs` holds the struct, the shared entry points (`new`, `fetch_config`), the `DENIED` literal and
//! the [`BearerChallenge`] parser. Each sibling opens with `use super::*` to reach these plus the flat
//! registry namespace, and Rust lets the `impl Client` blocks span the files transparently.

use super::*;
use serde_json::Value;

mod auth;
mod pull;
mod push;

/// The conformant `docker push` denial, surfaced whether the registry rejects the push at the token
/// endpoint or later at the blob-upload POST — both sites use this one literal so callers get a stable
/// error either way.
const DENIED: &str = "denied: requested access to the resource is denied (run `docker login`)";

/// A registry session for one image: caches the bearer token across the manifest + blob requests.
pub struct Client {
    image: ImageRef,
    creds: Credentials,
    token: Option<String>,
}
impl Client {
    pub fn new(image: ImageRef, creds: Credentials) -> Client {
        Client {
            image,
            creds,
            token: None,
        }
    }

    /// Fetch ONLY the image's run config blob (Cmd/Entrypoint/Env/WorkingDir/User + architecture/os),
    /// resolving the platform variant but WITHOUT downloading or unpacking any layers. Used to refresh a
    /// locally-cached image's config on a re-pull of an already-present tag, so a subsequent
    /// `docker run` picks up the correct Entrypoint/Cmd even when the layers are already on disk.
    pub fn fetch_config(&mut self, want_archs: &[&str]) -> Result<Value, String> {
        let manifest = self.resolve_manifest(want_archs)?;
        self.config_blob(&manifest)
    }
}

/// The `realm`/`service`/`scope` of a `WWW-Authenticate: Bearer …` header.
pub(super) struct BearerChallenge {
    pub(super) realm: String,
    pub(super) service: String,
    pub(super) scope: String,
}
impl BearerChallenge {
    pub(super) fn parse(headers: &str) -> Option<BearerChallenge> {
        let line = headers
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("www-authenticate:"))?;
        let params = line.splitn(2, "Bearer").nth(1)?;
        let get = |k: &str| {
            params
                .split(',')
                .find_map(|kv| kv.trim().strip_prefix(&format!("{k}=")))
                .map(|v| v.trim_matches('"').to_string())
        };
        Some(BearerChallenge {
            realm: get("realm")?,
            service: get("service").unwrap_or_default(),
            scope: get("scope").unwrap_or_default(),
        })
    }
}
