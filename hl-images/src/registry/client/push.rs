//! The PUSH flow: tar+gzip the rootfs into a single layer, upload the config + layer blobs, then PUT the
//! manifest. `upload_blob`/`put_manifest` are exclusive to this flow; `authenticate` (shared) lives in
//! `auth`.

use super::*;
use crate::Error;
use serde_json::json;
use std::path::Path;

impl Client {
    /// Push `rootfs` to the registry as a single-layer image under `self.image`. Returns the manifest
    /// digest. Requires credentials for a registry that demands auth.
    ///
    /// `config_obj` is the OCI image `config.config` object — the full runtime metadata Docker records
    /// (`Cmd`, `Entrypoint`, `Env`, `WorkingDir`, `User`, `ExposedPorts`, `Labels`, `StopSignal`,
    /// `Volumes`, `Healthcheck`). The caller assembles it from the image state so a pushed image starts
    /// and inspects exactly like the local one; passing `{"Cmd": [...]}` reproduces the old behavior.
    pub fn push(
        &mut self,
        rootfs: &Path,
        config_obj: &serde_json::Value,
        arch: &str,
        os: &str,
        work: &Path,
    ) -> Result<String, Error> {
        reset_dir(work)?;
        let layer = work.join("layer.tar.gz");
        let (layer_digest, layer_size) = tar_gzip(rootfs, &layer)?; // compressed digest = blob id
        let diff_id = crate::image::digest::sha256_gz_file(&layer)?; // uncompressed digest = rootfs diff_id

        let config = json!({
            "architecture": arch, "os": os, // os=darwin for mac containers; the manifest is os/arch-tagged
            "config": config_obj,
            "rootfs": { "type": "layers", "diff_ids": [diff_id] },
        });
        let config_path = work.join("config.json");
        let config_bytes = serde_json::to_vec(&config).map_err(|e| Error::Manifest(e.to_string()))?;
        std::fs::write(&config_path, &config_bytes).map_err(|e| Error::Archive(e.to_string()))?;
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

    // ---- push primitives ----

    fn upload_blob(&self, digest: &str, file: &Path) -> Result<(), Error> {
        let base = self.image.base_url();
        // already present?
        if http::head(&format!("{base}/blobs/{digest}"), self.token.as_deref())? == 200 {
            return Ok(());
        }
        // start an upload session -> Location, then monolithic PUT with ?digest=
        let start = http::post(&format!("{base}/blobs/uploads/"), self.token.as_deref())?;
        if start.status == 401 || start.status == 403 {
            return Err(Error::Registry(DENIED.to_string()));
        }
        if start.status != 202 {
            return Err(Error::Registry(format!(
                "blob upload not accepted ({})",
                start.status
            )));
        }
        let location = header(&start.headers, "location")
            .ok_or_else(|| Error::Registry("upload returned no Location".to_string()))?;
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
            Err(Error::Registry(format!("blob PUT -> {}", r.status)))
        }
    }
    fn put_manifest(&self, body: &[u8]) -> Result<String, Error> {
        let url = format!("{}/manifests/{}", self.image.base_url(), self.image.tag);
        let r = http::put_bytes(&url, body, MEDIA_MANIFEST, self.token.as_deref())?;
        if r.status == 201 {
            Ok(header(&r.headers, "docker-content-digest").unwrap_or_default())
        } else {
            Err(Error::Registry(format!(
                "manifest PUT -> {} {}",
                r.status,
                String::from_utf8_lossy(&r.body)
            )))
        }
    }
}
