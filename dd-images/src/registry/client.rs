//! The registry session service: pull/push/fetch_config over the bearer-challenge auth flow.

use super::http;
use super::*;
use serde_json::{json, Value};
use std::path::Path;

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

    /// Pull the image and unpack its layers into `rootfs` (created fresh). Picks the `linux/arm64`
    /// variant of a multi-arch index, falling back to `linux/amd64`. `progress` is invoked with a
    /// [`PullEvent`] as each layer downloads/unpacks, so the caller can stream live progress to the
    /// client; pass `&mut |_| {}` to ignore it.
    pub fn pull(
        &mut self,
        rootfs: &Path,
        want_archs: &[&str],
        progress: &mut dyn FnMut(PullEvent),
    ) -> Result<Pulled, String> {
        let manifest = self.resolve_manifest(want_archs)?;
        let config = self.config_blob(&manifest).unwrap_or_else(|_| json!({}));
        let layers = manifest["layers"]
            .as_array()
            .ok_or("manifest has no layers")?;
        if layers.is_empty() {
            return Err("manifest has no layers".into());
        }
        // Announce every layer up front (docker shows one "Pulling fs layer" line per blob), then pull
        // them in order so each layer's download/extract progress streams live and in id order.
        let metas: Vec<(String, u64, String)> = layers
            .iter()
            .map(|layer| {
                let digest = layer["digest"].as_str().unwrap_or_default().to_string();
                let size = layer["size"].as_u64().unwrap_or(0);
                let id = layer_short(&digest);
                (digest, size, id)
            })
            .collect();
        for (_, _, id) in &metas {
            progress(PullEvent::Layer { id: id.clone() });
        }
        reset_dir(rootfs)?;
        for (digest, size, id) in &metas {
            if digest.is_empty() {
                return Err("layer missing digest".into());
            }
            self.unpack_layer(digest, *size, id, rootfs, progress)?;
            progress(PullEvent::PullComplete { id: id.clone() });
        }
        Ok(Pulled {
            image: self.image.clone(),
            config,
        })
    }

    /// Fetch ONLY the image's run config blob (Cmd/Entrypoint/Env/WorkingDir/User + architecture/os),
    /// resolving the platform variant but WITHOUT downloading or unpacking any layers. Used to refresh a
    /// locally-cached image's config on a re-pull of an already-present tag, so a subsequent
    /// `docker run` picks up the correct Entrypoint/Cmd even when the layers are already on disk.
    pub fn fetch_config(&mut self, want_archs: &[&str]) -> Result<Value, String> {
        let manifest = self.resolve_manifest(want_archs)?;
        self.config_blob(&manifest)
    }

    /// Push `rootfs` to the registry as a single-layer image under `self.image`. Returns the manifest
    /// digest. Requires credentials for a registry that demands auth.
    pub fn push(
        &mut self,
        rootfs: &Path,
        cmd: &[String],
        arch: &str,
        os: &str,
        work: &Path,
    ) -> Result<String, String> {
        reset_dir(work)?;
        let layer = work.join("layer.tar.gz");
        let (layer_digest, layer_size) = tar_gzip(rootfs, &layer)?; // compressed digest = blob id
        let diff_id = crate::image::digest::sha256_gz_file(&layer)?; // uncompressed digest = rootfs diff_id

        let config = json!({
            "architecture": arch, "os": os, // os=darwin for mac containers; the manifest is os/arch-tagged
            "config": { "Cmd": cmd },
            "rootfs": { "type": "layers", "diff_ids": [diff_id] },
        });
        let config_path = work.join("config.json");
        let config_bytes = serde_json::to_vec(&config).map_err(|e| e.to_string())?;
        std::fs::write(&config_path, &config_bytes).map_err(|e| e.to_string())?;
        let config_digest = crate::image::digest::sha256_file(&config_path)?;

        self.authenticate("push,pull")?;
        self.upload_blob(&config_digest, &config_path)?;
        self.upload_blob(&layer_digest, &layer)?;

        let manifest = json!({
            "schemaVersion": 2, "mediaType": MEDIA_MANIFEST,
            "config": { "mediaType": MEDIA_CONFIG, "size": config_bytes.len(), "digest": config_digest },
            "layers": [{ "mediaType": MEDIA_LAYER, "size": layer_size, "digest": layer_digest }],
        });
        self.put_manifest(&serde_json::to_vec(&manifest).unwrap())
    }

    // ---- manifest / config / layer ----

    fn resolve_manifest(&mut self, want_archs: &[&str]) -> Result<Value, String> {
        let man = self.get_json(
            &format!("/manifests/{}", self.image.tag),
            Some(MANIFEST_ACCEPT),
        )?;
        let Some(list) = man["manifests"].as_array() else {
            return Ok(man);
        }; // already a single manifest
        let digest = want_archs
            .iter()
            .find_map(|arch| select_platform(list, arch))
            .ok_or_else(|| format!("no {} variant in the manifest list", want_archs.join("/")))?;
        self.get_json(&format!("/manifests/{digest}"), Some(MANIFEST_ACCEPT))
    }
    fn config_blob(&mut self, manifest: &Value) -> Result<Value, String> {
        let digest = manifest["config"]["digest"]
            .as_str()
            .ok_or("manifest has no config")?;
        let bytes = self.get_blob_bytes(digest)?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    }
    /// Download one layer blob to a temp file (emitting live `Downloading` byte progress), then unpack it
    /// into `rootfs` (emitting `Extracting`). Landing the compressed blob on disk first — rather than the
    /// old `curl | tar` pipe — is what lets us poll the byte count and report real progress; the temp
    /// file is removed afterwards regardless of outcome.
    fn unpack_layer(
        &mut self,
        digest: &str,
        size: u64,
        id: &str,
        rootfs: &Path,
        progress: &mut dyn FnMut(PullEvent),
    ) -> Result<(), String> {
        let token = self.authenticate("pull")?;
        let url = format!("{}/blobs/{digest}", self.image.base_url());
        let tmp = std::env::temp_dir().join(format!("dd-layer-{}-{id}.tar.gz", std::process::id()));
        let out = (|| {
            http::download_to_file(&url, token.as_deref(), &tmp, &mut |cur| {
                progress(PullEvent::Downloading {
                    id: id.to_string(),
                    current: cur,
                    total: size,
                });
            })?;
            progress(PullEvent::DownloadComplete { id: id.to_string() });
            progress(PullEvent::Extracting {
                id: id.to_string(),
                current: size,
                total: size,
            });
            // OPAQUE (`.wh..wh..opq`): a layer marks a directory opaque to hide ALL lower-layer entries in
            // it (e.g. `RUN rm -rf /var/lib/apt/lists/* && …`, a wholesale node_modules replace). Because
            // we FLATTEN every layer into one rootfs, honor it by clearing that dir's already-extracted
            // lower content BEFORE laying this layer down — otherwise stale lower files leak into the
            // squashed image (silent corruption of the pulled image). Must run before extract so the
            // layer's own entries repopulate the cleared dir.
            clear_opaque_dirs(rootfs, &opaque_dirs_in_tar(&tmp));
            http::extract_targz(&tmp, rootfs)
        })();
        let _ = std::fs::remove_file(&tmp);
        out?;
        apply_whiteouts(rootfs)
    }

    // ---- push primitives ----

    fn upload_blob(&self, digest: &str, file: &Path) -> Result<(), String> {
        let base = self.image.base_url();
        // already present?
        if http::head(&format!("{base}/blobs/{digest}"), self.token.as_deref())? == 200 {
            return Ok(());
        }
        // start an upload session -> Location, then monolithic PUT with ?digest=
        let start = http::post(&format!("{base}/blobs/uploads/"), self.token.as_deref())?;
        if start.status == 401 || start.status == 403 {
            return Err(DENIED.into());
        }
        if start.status != 202 {
            return Err(format!("blob upload not accepted ({})", start.status));
        }
        let location = header(&start.headers, "location").ok_or("upload returned no Location")?;
        let sep = if location.contains('?') { '&' } else { '?' };
        let put = format!("{}{sep}digest={digest}", absolute(&location, &base));
        let r = http::put_file(
            &put,
            file,
            "application/octet-stream",
            self.token.as_deref(),
        )?;
        if r.status == 201 || r.status == 202 {
            Ok(())
        } else {
            Err(format!("blob PUT -> {}", r.status))
        }
    }
    fn put_manifest(&self, body: &[u8]) -> Result<String, String> {
        let url = format!("{}/manifests/{}", self.image.base_url(), self.image.tag);
        let r = http::put_bytes(&url, body, MEDIA_MANIFEST, self.token.as_deref())?;
        if r.status == 201 {
            Ok(header(&r.headers, "docker-content-digest").unwrap_or_default())
        } else {
            Err(format!(
                "manifest PUT -> {} {}",
                r.status,
                String::from_utf8_lossy(&r.body)
            ))
        }
    }

    // ---- authenticated GET with the bearer-challenge dance ----

    fn get_json(&mut self, path: &str, accept: Option<&str>) -> Result<Value, String> {
        let bytes = self.authed_get(path, accept)?;
        serde_json::from_slice(&bytes).map_err(|e| format!("bad JSON from {path}: {e}"))
    }
    fn get_blob_bytes(&mut self, digest: &str) -> Result<Vec<u8>, String> {
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
    fn authenticate(&mut self, scope_action: &str) -> Result<Option<String>, String> {
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

/// Pick the digest of the `linux/<arch>` entry of a manifest list/index.
fn select_platform(list: &[Value], arch: &str) -> Option<String> {
    list.iter()
        .find(|m| m["platform"]["architecture"] == arch && m["platform"]["os"] == "linux")
        .and_then(|m| m["digest"].as_str().map(str::to_string))
}
