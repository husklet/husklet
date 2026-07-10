//! The PULL flow: resolve the platform manifest, then download and unpack each layer blob into a fresh
//! rootfs, streaming live progress. Shares `resolve_manifest`/`config_blob` with `fetch_config` (hence
//! `pub(super)`); `unpack_layer` and `select_platform` are exclusive to this flow.

use super::*;
use crate::Error;
use serde_json::{json, Value};
use std::path::Path;

impl Client {
    /// Pull the image and unpack its layers into `rootfs` (created fresh). Picks the `linux/arm64`
    /// variant of a multi-arch index, falling back to `linux/amd64`. `progress` is invoked with a
    /// [`PullEvent`] as each layer downloads/unpacks, so the caller can stream live progress to the
    /// client; pass `&mut |_| {}` to ignore it.
    pub fn pull(
        &mut self,
        rootfs: &Path,
        want_archs: &[&str],
        progress: &mut dyn FnMut(PullEvent),
    ) -> Result<Pulled, Error> {
        let manifest = self.resolve_manifest(want_archs)?;
        let config = self.config_blob(&manifest).unwrap_or_else(|_| json!({}));
        let layers = manifest["layers"]
            .as_array()
            .ok_or_else(|| Error::Manifest("manifest has no layers".to_string()))?;
        if layers.is_empty() {
            return Err(Error::Manifest("manifest has no layers".to_string()));
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
                return Err(Error::Manifest("layer missing digest".to_string()));
            }
            self.unpack_layer(digest, *size, id, rootfs, progress)?;
            progress(PullEvent::PullComplete { id: id.clone() });
        }
        Ok(Pulled {
            image: self.image.clone(),
            config,
        })
    }

    // ---- manifest / config / layer ----

    pub(super) fn resolve_manifest(&mut self, want_archs: &[&str]) -> Result<Value, Error> {
        let man = self.get_json(
            &format!("/manifests/{}", self.image.manifest_reference()),
            Some(MANIFEST_ACCEPT),
        )?;
        let Some(list) = man["manifests"].as_array() else {
            return Ok(man);
        }; // already a single manifest
        let digest = want_archs
            .iter()
            .find_map(|arch| select_platform(list, arch))
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "no {} variant in the manifest list",
                    want_archs.join("/")
                ))
            })?;
        self.get_json(&format!("/manifests/{digest}"), Some(MANIFEST_ACCEPT))
    }
    pub(super) fn config_blob(&mut self, manifest: &Value) -> Result<Value, Error> {
        let digest = manifest["config"]["digest"]
            .as_str()
            .ok_or_else(|| Error::Manifest("manifest has no config".to_string()))?;
        let bytes = self.get_blob_bytes(digest)?;
        serde_json::from_slice(&bytes).map_err(|e| Error::Manifest(e.to_string()))
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
    ) -> Result<(), Error> {
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
}

/// Pick the digest of the `linux/<arch>` entry of a manifest list/index.
fn select_platform(list: &[Value], arch: &str) -> Option<String> {
    list.iter()
        .find(|m| m["platform"]["architecture"] == arch && m["platform"]["os"] == "linux")
        .and_then(|m| m["digest"].as_str().map(str::to_string))
}
