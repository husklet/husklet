//! The PULL flow: resolve the platform manifest, then download and unpack each layer blob into a fresh
//! rootfs, streaming live progress. Shares `resolve_manifest`/`config_blob` with `fetch_config` (hence
//! `pub(super)`); `unpack_layer` and `select_platform` are exclusive to this flow.

use super::*;
use crate::Error;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A process-unique suffix so concurrent pulls / concurrent layer downloads in ONE process never share a
/// temp path (layer blob temp file, staging/backup rootfs dir).
fn uniq() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

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
        // The config blob is mandatory and must be valid JSON matching its descriptor (an invalid config
        // is NOT silently treated as `{}` — that hid the image's Entrypoint/Cmd/arch). Verified for digest
        // + size inside `config_blob`.
        let config = self.config_blob(&manifest)?;
        let layers = manifest["layers"].as_array().ok_or_else(|| {
            Error::Manifest("manifest has no layers array".to_string())
        })?;
        // A zero-layer manifest is VALID (a `scratch` image): it pulls to an empty rootfs. Only a missing
        // `layers` array (above) is an error.

        // The config's `rootfs.diff_ids` are the UNCOMPRESSED digest of each layer, in order; we verify
        // each downloaded layer's decompressed digest against them (defense against a swapped layer).
        let diff_ids: Vec<String> = config["rootfs"]["diff_ids"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        // Announce every layer up front (docker shows one "Pulling fs layer" line per blob), then pull
        // them in order so each layer's download/extract progress streams live and in id order. Reject an
        // unsupported layer media type BEFORE any download rather than blindly gzip-extracting it.
        struct LayerMeta {
            digest: String,
            size: u64,
            id: String,
        }
        let mut metas: Vec<LayerMeta> = Vec::with_capacity(layers.len());
        for layer in layers {
            let media = layer["mediaType"].as_str().unwrap_or_default();
            if !is_supported_layer_media(media) {
                return Err(Error::Manifest(format!(
                    "unsupported layer media type: {media}"
                )));
            }
            let digest = layer["digest"].as_str().unwrap_or_default().to_string();
            if digest.is_empty() {
                return Err(Error::Manifest("layer missing digest".to_string()));
            }
            let size = layer["size"].as_u64().unwrap_or(0);
            let id = layer_short(&digest);
            metas.push(LayerMeta { digest, size, id });
        }
        for m in &metas {
            progress(PullEvent::Layer { id: m.id.clone() });
        }

        // STAGING: extract every layer into a scratch rootfs and only swap it into the final path on FULL
        // success. A failure on a later layer then leaves no partial final rootfs — and if a previous
        // image already existed at `rootfs`, it stays intact.
        let staging = sibling(rootfs, "staging");
        reset_dir(&staging)?;
        let staged = (|| {
            for (i, m) in metas.iter().enumerate() {
                self.unpack_layer(
                    &m.digest,
                    m.size,
                    &m.id,
                    &staging,
                    diff_ids.get(i).map(String::as_str),
                    progress,
                )?;
                progress(PullEvent::PullComplete { id: m.id.clone() });
            }
            Ok::<(), Error>(())
        })();
        if let Err(e) = staged {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
        swap_into_place(&staging, rootfs)?;

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
            return validate_image_manifest(man); // already a single manifest
        };
        let digest = want_archs
            .iter()
            .find_map(|arch| select_platform(list, arch))
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "no {} variant in the manifest list",
                    want_archs.join("/")
                ))
            })?;
        let child = self.get_json(&format!("/manifests/{digest}"), Some(MANIFEST_ACCEPT))?;
        validate_image_manifest(child)
    }
    pub(super) fn config_blob(&mut self, manifest: &Value) -> Result<Value, Error> {
        let desc = &manifest["config"];
        let digest = desc["digest"]
            .as_str()
            .ok_or_else(|| Error::Manifest("manifest has no config".to_string()))?;
        let bytes = self.get_blob_bytes(digest)?;
        // Verify the config blob's SIZE and DIGEST against the descriptor before trusting it, then require
        // it to be valid JSON (an invalid/short config must reject, not degrade to `{}`).
        if let Some(sz) = desc["size"].as_u64() {
            if sz != bytes.len() as u64 {
                return Err(Error::Manifest(format!(
                    "config size mismatch: descriptor {sz} != {} bytes",
                    bytes.len()
                )));
            }
        }
        verify_digest("config", digest, &crate::image::digest::sha256_hex(&bytes))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::Manifest(format!("invalid config blob: {e}")))
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
        diff_id: Option<&str>,
        progress: &mut dyn FnMut(PullEvent),
    ) -> Result<(), Error> {
        let token = self.authenticate("pull")?;
        let url = format!("{}/blobs/{digest}", self.image.base_url());
        // A process-unique temp name (`<pid>-<seq>-<id>`) so concurrent pulls / concurrent layer
        // downloads never share and clobber the same blob file.
        let tmp = std::env::temp_dir().join(format!(
            "dd-layer-{}-{}-{id}.tar.gz",
            std::process::id(),
            uniq()
        ));
        let out = (|| {
            http::download_to_file(&url, token.as_deref(), &tmp, &mut |cur| {
                progress(PullEvent::Downloading {
                    id: id.to_string(),
                    current: cur,
                    total: size,
                });
            })?;
            progress(PullEvent::DownloadComplete { id: id.to_string() });
            // Verify the downloaded blob's SIZE and (compressed) DIGEST, and its UNCOMPRESSED digest
            // against the config's `diff_id`, BEFORE extraction — reject a tampered/mismatched layer
            // rather than unpack it into the rootfs.
            if size != 0 {
                let got = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
                if got != size {
                    return Err(Error::Manifest(format!(
                        "layer {id} size mismatch: descriptor {size} != {got} bytes"
                    )));
                }
            }
            verify_digest(
                &format!("layer {id}"),
                digest,
                &crate::image::digest::sha256_file(&tmp)?,
            )?;
            if let Some(want) = diff_id {
                verify_digest(
                    &format!("layer {id} diff_id"),
                    want,
                    &crate::image::digest::sha256_gz_file(&tmp)?,
                )?;
            }
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

/// Pick the digest of the `linux/<arch>` entry of a manifest list/index. The requested `arch` may carry
/// an OCI variant as `arch/variant` (e.g. `arm/v7`, `arm64/v8`); when a variant is requested the entry's
/// `platform.variant` must match, so two entries differing ONLY by variant resolve to the requested one.
/// A bare arch (no `/variant`) matches on arch+os alone (variant ignored).
fn select_platform(list: &[Value], arch: &str) -> Option<String> {
    let (want_arch, want_variant) = match arch.split_once('/') {
        Some((a, v)) => (a, Some(v)),
        None => (arch, None),
    };
    list.iter()
        .find(|m| {
            let p = &m["platform"];
            p["os"] == "linux"
                && p["architecture"] == want_arch
                && want_variant.is_none_or(|v| p["variant"] == v)
        })
        .and_then(|m| m["digest"].as_str().map(str::to_string))
}

/// Validate a resolved single image manifest before we extract from it: `schemaVersion` must be 2 (reject
/// legacy schema-1 `fsLayers` manifests) and any present `mediaType` must be a manifest type we support
/// (not an index — that was resolved already, and not an unknown type).
fn validate_image_manifest(man: Value) -> Result<Value, Error> {
    match man["schemaVersion"].as_u64() {
        Some(2) => {}
        other => {
            return Err(Error::Manifest(format!(
                "unsupported manifest schemaVersion: {}",
                other.map(|n| n.to_string()).unwrap_or_else(|| "absent".to_string())
            )))
        }
    }
    if let Some(media) = man["mediaType"].as_str() {
        if !is_supported_manifest_media(media) {
            return Err(Error::Manifest(format!(
                "unsupported manifest media type: {media}"
            )));
        }
    }
    Ok(man)
}

/// Compare an advertised descriptor digest with a computed one, tolerating the `sha256:` prefix on either
/// side. Both are lowercase hex; a mismatch is a hard error.
fn verify_digest(what: &str, want: &str, got: &str) -> Result<(), Error> {
    let norm = |s: &str| s.trim().trim_start_matches("sha256:").to_ascii_lowercase();
    if norm(want) != norm(got) {
        return Err(Error::Manifest(format!(
            "{what} digest mismatch: expected {want}, got {got}"
        )));
    }
    Ok(())
}

/// A sibling scratch path next to `final_` (same parent, hence same filesystem so a later `rename` swap is
/// atomic), process-unique so concurrent pulls don't collide: `<parent>/.rootfs.<tag>.<pid>.<seq>`.
fn sibling(final_: &Path, tag: &str) -> PathBuf {
    let parent = final_.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = final_
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "rootfs".to_string());
    parent.join(format!(".{name}.{tag}.{}.{}", std::process::id(), uniq()))
}

/// Atomically swap a fully-staged rootfs into `final_`, leaving any previous rootfs intact on failure. The
/// old tree (if any) is moved aside first so a failed swap can be rolled back; it is removed only once the
/// staged tree is in place.
fn swap_into_place(staging: &Path, final_: &Path) -> Result<(), Error> {
    if let Some(parent) = final_.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Archive(e.to_string()))?;
    }
    if final_.exists() {
        let backup = sibling(final_, "backup");
        std::fs::rename(final_, &backup).map_err(|e| Error::Archive(e.to_string()))?;
        if let Err(e) = std::fs::rename(staging, final_) {
            let _ = std::fs::rename(&backup, final_); // restore the previous rootfs
            let _ = std::fs::remove_dir_all(staging);
            return Err(Error::Archive(e.to_string()));
        }
        let _ = std::fs::remove_dir_all(&backup);
    } else {
        std::fs::rename(staging, final_).map_err(|e| Error::Archive(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    type Routes = HashMap<String, (u16, String, Vec<u8>)>;

    // A tiny in-process HTTP registry: routes exact request paths to canned (status, content-type, body)
    // responses. Enough to exercise the pull flow (manifest + config + layer blobs) fully offline.
    struct Mock {
        port: u16,
    }
    impl Mock {
        fn start(routes: Routes) -> Mock {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let routes = Arc::new(Mutex::new(routes));
            std::thread::spawn(move || {
                for conn in listener.incoming() {
                    let Ok(mut sock) = conn else { continue };
                    let mut buf = [0u8; 8192];
                    let n = sock.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("")
                        .to_string();
                    let hit = routes.lock().unwrap().get(&path).cloned();
                    let (status, ct, body) =
                        hit.unwrap_or((404, "text/plain".to_string(), b"not found".to_vec()));
                    let head = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(head.as_bytes());
                    let _ = sock.write_all(&body);
                }
            });
            Mock { port }
        }
        fn image(&self) -> ImageRef {
            ImageRef {
                registry: format!("127.0.0.1:{}", self.port),
                repository: "test/img".to_string(),
                tag: "latest".to_string(),
                digest: None,
            }
        }
        fn client(&self) -> Client {
            Client::new(self.image(), Credentials::none())
        }
    }

    // `Pulled` isn't `Debug`, so `.unwrap_err()` won't compile; unwrap the error explicitly.
    fn expect_err(r: Result<Pulled, Error>) -> Error {
        match r {
            Err(e) => e,
            Ok(_) => panic!("expected an error, got Ok"),
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let n = uniq();
        let p = std::env::temp_dir().join(format!("dd-pull-test-{}-{}-{}", tag, std::process::id(), n));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // Build a gzipped-tar layer containing `files`, returning (bytes, compressed-digest, diff_id).
    fn make_layer(files: &[(&str, &[u8])]) -> (Vec<u8>, String, String) {
        let dir = scratch("layer-src");
        for (name, data) in files {
            let p = dir.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, data).unwrap();
        }
        let tar = scratch("layer-tar").join("l.tar.gz");
        let st = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&tar)
            .arg("-C")
            .arg(&dir)
            .arg(".")
            .status()
            .unwrap();
        assert!(st.success());
        let bytes = std::fs::read(&tar).unwrap();
        let digest = format!("sha256:{}", crate::image::digest::sha256_hex(&bytes));
        let diff_id = crate::image::digest::sha256_gz_file(&tar).unwrap();
        let _ = std::fs::remove_dir_all(dir);
        (bytes, digest, diff_id)
    }

    fn config_bytes(diff_ids: &[String]) -> Vec<u8> {
        json!({
            "architecture": "amd64",
            "os": "linux",
            "rootfs": { "type": "layers", "diff_ids": diff_ids },
            "config": { "Cmd": ["/bin/sh"] }
        })
        .to_string()
        .into_bytes()
    }

    fn blob_path(digest: &str) -> String {
        format!("/v2/test/img/blobs/{digest}")
    }
    const MANIFEST_PATH: &str = "/v2/test/img/manifests/latest";

    // A well-formed single-layer image whose config + layer digests/sizes all match: pulls Ok, extracts.
    fn good_routes() -> (Routes, String, Vec<u8>) {
        let (layer, ldigest, diff_id) = make_layer(&[("hello.txt", b"hi\n")]);
        let cfg = config_bytes(&[diff_id]);
        let cdigest = format!("sha256:{}", crate::image::digest::sha256_hex(&cfg));
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": MEDIA_MANIFEST,
            "config": { "mediaType": MEDIA_CONFIG, "size": cfg.len(), "digest": cdigest },
            "layers": [{ "mediaType": MEDIA_LAYER, "size": layer.len(), "digest": ldigest }],
        })
        .to_string()
        .into_bytes();
        let mut r: Routes = HashMap::new();
        r.insert(MANIFEST_PATH.to_string(), (200, MEDIA_MANIFEST.to_string(), manifest.clone()));
        r.insert(blob_path(&cdigest), (200, MEDIA_CONFIG.to_string(), cfg));
        r.insert(blob_path(&ldigest), (200, MEDIA_LAYER.to_string(), layer));
        (r, ldigest, manifest)
    }

    #[test]
    fn pull_good_image_extracts_layer() {
        let (routes, _, _) = good_routes();
        let m = Mock::start(routes);
        let rootfs = scratch("good").join("rootfs");
        m.client().pull(&rootfs, &["amd64"], &mut |_| {}).expect("good pull");
        assert_eq!(std::fs::read(rootfs.join("hello.txt")).unwrap(), b"hi\n");
        let _ = std::fs::remove_dir_all(rootfs.parent().unwrap());
    }

    // Finding 12: an invalid (non-JSON) config blob must reject, not degrade to `{}`.
    #[test]
    fn pull_rejects_invalid_config() {
        let (mut routes, _, _) = good_routes();
        // find the config blob path (the one with MEDIA_CONFIG) and corrupt it, keeping a matching digest
        // would require rehash; instead point config to garbage AND fix its digest so we test the JSON
        // parse rejection specifically.
        let garbage = b"not json".to_vec();
        let cdigest = format!("sha256:{}", crate::image::digest::sha256_hex(&garbage));
        // rebuild manifest to reference the garbage config with a correct digest+size.
        let (layer, ldigest, diff_id) = make_layer(&[("x", b"y")]);
        let _ = diff_id;
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MEDIA_MANIFEST,
            "config": { "mediaType": MEDIA_CONFIG, "size": garbage.len(), "digest": cdigest },
            "layers": [{ "mediaType": MEDIA_LAYER, "size": layer.len(), "digest": ldigest }],
        }).to_string().into_bytes();
        routes.clear();
        routes.insert(MANIFEST_PATH.to_string(), (200, MEDIA_MANIFEST.to_string(), manifest));
        routes.insert(blob_path(&cdigest), (200, MEDIA_CONFIG.to_string(), garbage));
        routes.insert(blob_path(&ldigest), (200, MEDIA_LAYER.to_string(), layer));
        let m = Mock::start(routes);
        let rootfs = scratch("badcfg").join("rootfs");
        let err = expect_err(m.client().pull(&rootfs, &["amd64"], &mut |_| {}));
        assert!(format!("{err}").contains("config"), "got {err}");
        assert!(!rootfs.exists(), "no rootfs on config rejection");
    }

    // Finding 13: a schemaVersion-1 manifest must be rejected before any layer extraction.
    #[test]
    fn pull_rejects_schema_version_1() {
        let manifest = json!({
            "schemaVersion": 1,
            "fsLayers": [{ "blobSum": "sha256:deadbeef" }],
        }).to_string().into_bytes();
        let mut r: Routes = HashMap::new();
        r.insert(MANIFEST_PATH.to_string(), (200, "application/json".to_string(), manifest));
        let m = Mock::start(r);
        let rootfs = scratch("schema1").join("rootfs");
        let err = expect_err(m.client().pull(&rootfs, &["amd64"], &mut |_| {}));
        assert!(format!("{err}").contains("schemaVersion"), "got {err}");
        assert!(!rootfs.exists());
    }

    // Finding 14: an unsupported layer media type (zstd) must be rejected, not gzip-extracted.
    #[test]
    fn pull_rejects_zstd_layer_media() {
        let (layer, ldigest, diff_id) = make_layer(&[("f", b"z")]);
        let cfg = config_bytes(&[diff_id]);
        let cdigest = format!("sha256:{}", crate::image::digest::sha256_hex(&cfg));
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MEDIA_MANIFEST,
            "config": { "mediaType": MEDIA_CONFIG, "size": cfg.len(), "digest": cdigest },
            "layers": [{ "mediaType": "application/vnd.oci.image.layer.v1.tar+zstd",
                         "size": layer.len(), "digest": ldigest }],
        }).to_string().into_bytes();
        let mut r: Routes = HashMap::new();
        r.insert(MANIFEST_PATH.to_string(), (200, MEDIA_MANIFEST.to_string(), manifest));
        r.insert(blob_path(&cdigest), (200, MEDIA_CONFIG.to_string(), cfg));
        r.insert(blob_path(&ldigest), (200, MEDIA_LAYER.to_string(), layer));
        let m = Mock::start(r);
        let rootfs = scratch("zstd").join("rootfs");
        let err = expect_err(m.client().pull(&rootfs, &["amd64"], &mut |_| {}));
        assert!(format!("{err}").contains("media type"), "got {err}");
        assert!(!rootfs.exists());
    }

    // Finding 10: a layer whose bytes don't match its advertised digest is rejected before extraction.
    #[test]
    fn pull_rejects_layer_digest_mismatch() {
        let (layer, _real, diff_id) = make_layer(&[("f", b"data")]);
        let lying = "sha256:".to_string() + &"0".repeat(64);
        let cfg = config_bytes(&[diff_id]);
        let cdigest = format!("sha256:{}", crate::image::digest::sha256_hex(&cfg));
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MEDIA_MANIFEST,
            "config": { "mediaType": MEDIA_CONFIG, "size": cfg.len(), "digest": cdigest },
            "layers": [{ "mediaType": MEDIA_LAYER, "size": layer.len(), "digest": lying }],
        }).to_string().into_bytes();
        let mut r: Routes = HashMap::new();
        r.insert(MANIFEST_PATH.to_string(), (200, MEDIA_MANIFEST.to_string(), manifest));
        r.insert(blob_path(&cdigest), (200, MEDIA_CONFIG.to_string(), cfg));
        r.insert(blob_path(&lying), (200, MEDIA_LAYER.to_string(), layer));
        let m = Mock::start(r);
        let rootfs = scratch("ldigest").join("rootfs");
        let err = expect_err(m.client().pull(&rootfs, &["amd64"], &mut |_| {}));
        assert!(format!("{err}").contains("digest mismatch"), "got {err}");
        assert!(!rootfs.join("f").exists());
    }

    // Finding 11: a layer whose uncompressed digest != config diff_id is rejected.
    #[test]
    fn pull_rejects_diff_id_mismatch() {
        let (layer, ldigest, _real_diff) = make_layer(&[("f", b"data")]);
        let wrong_diff = "sha256:".to_string() + &"1".repeat(64);
        let cfg = config_bytes(&[wrong_diff]);
        let cdigest = format!("sha256:{}", crate::image::digest::sha256_hex(&cfg));
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MEDIA_MANIFEST,
            "config": { "mediaType": MEDIA_CONFIG, "size": cfg.len(), "digest": cdigest },
            "layers": [{ "mediaType": MEDIA_LAYER, "size": layer.len(), "digest": ldigest }],
        }).to_string().into_bytes();
        let mut r: Routes = HashMap::new();
        r.insert(MANIFEST_PATH.to_string(), (200, MEDIA_MANIFEST.to_string(), manifest));
        r.insert(blob_path(&cdigest), (200, MEDIA_CONFIG.to_string(), cfg));
        r.insert(blob_path(&ldigest), (200, MEDIA_LAYER.to_string(), layer));
        let m = Mock::start(r);
        let rootfs = scratch("diffid").join("rootfs");
        let err = expect_err(m.client().pull(&rootfs, &["amd64"], &mut |_| {}));
        assert!(format!("{err}").contains("diff_id"), "got {err}");
        assert!(!rootfs.join("f").exists());
    }

    // Finding 15: a config whose descriptor size != actual bytes is rejected.
    #[test]
    fn pull_rejects_config_size_mismatch() {
        let (layer, ldigest, diff_id) = make_layer(&[("f", b"z")]);
        let cfg = config_bytes(&[diff_id]);
        let cdigest = format!("sha256:{}", crate::image::digest::sha256_hex(&cfg));
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MEDIA_MANIFEST,
            "config": { "mediaType": MEDIA_CONFIG, "size": cfg.len() + 999, "digest": cdigest },
            "layers": [{ "mediaType": MEDIA_LAYER, "size": layer.len(), "digest": ldigest }],
        }).to_string().into_bytes();
        let mut r: Routes = HashMap::new();
        r.insert(MANIFEST_PATH.to_string(), (200, MEDIA_MANIFEST.to_string(), manifest));
        r.insert(blob_path(&cdigest), (200, MEDIA_CONFIG.to_string(), cfg));
        r.insert(blob_path(&ldigest), (200, MEDIA_LAYER.to_string(), layer));
        let m = Mock::start(r);
        let rootfs = scratch("cfgsize").join("rootfs");
        let err = expect_err(m.client().pull(&rootfs, &["amd64"], &mut |_| {}));
        assert!(format!("{err}").contains("size mismatch"), "got {err}");
        assert!(!rootfs.exists());
    }

    // Finding 16: a valid zero-layer manifest (scratch image) pulls Ok to an empty rootfs.
    #[test]
    fn pull_accepts_zero_layer_scratch() {
        let cfg = config_bytes(&[]);
        let cdigest = format!("sha256:{}", crate::image::digest::sha256_hex(&cfg));
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MEDIA_MANIFEST,
            "config": { "mediaType": MEDIA_CONFIG, "size": cfg.len(), "digest": cdigest },
            "layers": [],
        }).to_string().into_bytes();
        let mut r: Routes = HashMap::new();
        r.insert(MANIFEST_PATH.to_string(), (200, MEDIA_MANIFEST.to_string(), manifest));
        r.insert(blob_path(&cdigest), (200, MEDIA_CONFIG.to_string(), cfg));
        let m = Mock::start(r);
        let rootfs = scratch("scratch").join("rootfs");
        m.client().pull(&rootfs, &["amd64"], &mut |_| {}).expect("scratch pull ok");
        assert!(rootfs.is_dir(), "empty rootfs created");
        assert_eq!(std::fs::read_dir(&rootfs).unwrap().count(), 0, "rootfs is empty");
        let _ = std::fs::remove_dir_all(rootfs.parent().unwrap());
    }

    // Finding 17: a pull that fails on a later layer leaves no partial final rootfs, and a previously
    // existing rootfs stays intact.
    #[test]
    fn failed_pull_preserves_previous_rootfs() {
        let (l1, d1, diff1) = make_layer(&[("first.txt", b"1")]);
        let (l2, _real2, diff2) = make_layer(&[("second.txt", b"2")]);
        let lying2 = "sha256:".to_string() + &"2".repeat(64); // wrong digest for layer 2 -> fails
        let cfg = config_bytes(&[diff1, diff2]);
        let cdigest = format!("sha256:{}", crate::image::digest::sha256_hex(&cfg));
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MEDIA_MANIFEST,
            "config": { "mediaType": MEDIA_CONFIG, "size": cfg.len(), "digest": cdigest },
            "layers": [
                { "mediaType": MEDIA_LAYER, "size": l1.len(), "digest": d1 },
                { "mediaType": MEDIA_LAYER, "size": l2.len(), "digest": lying2 },
            ],
        }).to_string().into_bytes();
        let mut r: Routes = HashMap::new();
        r.insert(MANIFEST_PATH.to_string(), (200, MEDIA_MANIFEST.to_string(), manifest));
        r.insert(blob_path(&cdigest), (200, MEDIA_CONFIG.to_string(), cfg));
        r.insert(blob_path(&d1), (200, MEDIA_LAYER.to_string(), l1));
        r.insert(blob_path(&lying2), (200, MEDIA_LAYER.to_string(), l2));
        let m = Mock::start(r);

        // a previously pulled image already lives at rootfs.
        let rootfs = scratch("prev").join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        std::fs::write(rootfs.join("PREVIOUS"), b"keep").unwrap();

        let err = expect_err(m.client().pull(&rootfs, &["amd64"], &mut |_| {}));
        assert!(format!("{err}").contains("digest mismatch"), "got {err}");
        // the previous rootfs is intact, and no first.txt (staging) leaked into it.
        assert_eq!(std::fs::read(rootfs.join("PREVIOUS")).unwrap(), b"keep");
        assert!(!rootfs.join("first.txt").exists(), "no partial layer in final rootfs");
        // no leftover staging/backup siblings.
        let parent = rootfs.parent().unwrap();
        let strays: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("staging"))
            .collect();
        assert!(strays.is_empty(), "staging dir cleaned up");
        let _ = std::fs::remove_dir_all(parent);
    }

    // Finding 8: concurrent layer temp names are unique within one process.
    #[test]
    fn layer_temp_uniq_helper_is_monotonic() {
        let a = uniq();
        let b = uniq();
        assert_ne!(a, b);
    }

    // Finding 18: platform selection honors the OCI variant when the requested arch carries one.
    #[test]
    fn select_platform_honors_variant() {
        let list = vec![
            json!({ "platform": { "os": "linux", "architecture": "arm", "variant": "v6" }, "digest": "sha256:v6" }),
            json!({ "platform": { "os": "linux", "architecture": "arm", "variant": "v7" }, "digest": "sha256:v7" }),
        ];
        // requesting arm/v7 selects the v7 entry, not the first (v6) match.
        assert_eq!(select_platform(&list, "arm/v7"), Some("sha256:v7".to_string()));
        assert_eq!(select_platform(&list, "arm/v6"), Some("sha256:v6".to_string()));
        // a bare arch (no variant) matches arch+os and takes the first entry.
        assert_eq!(select_platform(&list, "arm"), Some("sha256:v6".to_string()));
        // an unrequestable variant yields nothing.
        assert_eq!(select_platform(&list, "arm/v9"), None);
    }
}
