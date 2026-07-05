//! The AUTH flow: authenticated GETs with the `WWW-Authenticate: Bearer` challenge dance, plus the token
//! minting. `get_json`/`get_blob_bytes`/`authenticate` are shared with the pull/push flows (hence
//! `pub(super)`); `authed_get` and the `token_from_challenge*` pair are internal to this file.

use super::*;
use serde_json::Value;

impl Client {
    // ---- authenticated GET with the bearer-challenge dance ----

    pub(super) fn get_json(&mut self, path: &str, accept: Option<&str>) -> Result<Value, String> {
        let bytes = self.authed_get(path, accept)?;
        serde_json::from_slice(&bytes).map_err(|e| format!("bad JSON from {path}: {e}"))
    }
    pub(super) fn get_blob_bytes(&mut self, digest: &str) -> Result<Vec<u8>, String> {
        self.authed_get(&format!("/blobs/{digest}"), None)
    }
    fn authed_get(&mut self, path: &str, accept: Option<&str>) -> Result<Vec<u8>, String> {
        let url = format!("{}{path}", self.image.base_url());
        let first = http::get(&url, accept, self.token.as_deref())?;
        if first.status == 200 {
            return Ok(first.body);
        }
        if first.status == 401 {
            let token = self.token_from_challenge(&first.headers)?;
            self.token = Some(token.clone());
            let retry = http::get(&url, accept, Some(&token))?;
            if retry.status == 200 {
                return Ok(retry.body);
            }
            return Err(format!("GET {url} -> {} after auth", retry.status));
        }
        Err(format!("GET {url} -> {}", first.status))
    }
    /// Ensure we hold a token for `scope`; returns it (None for anonymous registries that 401-less).
    pub(super) fn authenticate(&mut self, scope_action: &str) -> Result<Option<String>, String> {
        if self.token.is_some() {
            return Ok(self.token.clone());
        }
        // probe to discover the auth realm
        let probe = http::get(
            &format!("{}/manifests/{}", self.image.base_url(), self.image.tag),
            Some(MANIFEST_ACCEPT),
            None,
        )?;
        if probe.status == 200 {
            return Ok(None);
        } // open registry
        let scope = format!("repository:{}:{scope_action}", self.image.repository);
        let token = self.token_from_challenge_scoped(&probe.headers, Some(&scope))?;
        self.token = Some(token.clone());
        Ok(Some(token))
    }
    fn token_from_challenge(&self, headers: &str) -> Result<String, String> {
        self.token_from_challenge_scoped(headers, None)
    }
    fn token_from_challenge_scoped(
        &self,
        headers: &str,
        scope: Option<&str>,
    ) -> Result<String, String> {
        let ch = BearerChallenge::parse(headers).ok_or("registry gave no Bearer challenge")?;
        let scope = scope.unwrap_or(&ch.scope);
        let url = format!("{}?service={}&scope={}", ch.realm, ch.service, scope);
        let creds = if self.creds.is_empty() {
            None
        } else {
            Some(&self.creds)
        };
        let resp = http::get_with_basic(&url, creds)?;
        if resp.status != 200 {
            // A registry that refuses to mint a *push*-scoped token for an unauthenticated client is
            // denying the push -- exactly `docker push` without `docker login`. Surface the conformant
            // "denied" message whether the denial lands here at the token endpoint (401/403) or later at
            // the blob-upload POST (see upload_blob), so callers get one stable error either way.
            let action = scope.rsplit(':').next().unwrap_or("");
            if action.contains("push") && (resp.status == 401 || resp.status == 403) {
                return Err(DENIED.into());
            }
            return Err(format!("token endpoint -> {}", resp.status));
        }
        let v: Value = serde_json::from_slice(&resp.body).map_err(|e| e.to_string())?;
        v["token"]
            .as_str()
            .or_else(|| v["access_token"].as_str())
            .map(str::to_string)
            .ok_or_else(|| "token response had no token".to_string())
    }
}
