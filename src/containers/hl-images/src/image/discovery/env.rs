//! Env recovery + sidecar JSON helpers: recover an image's environment from an on-disk OCI config,
//! persist it back into the `hl-image.json` sidecar, and extract string fields from that sidecar.

use super::*;
use serde_json::{json, Value};
use std::path::Path;

/// A JSON string array flattened to `Vec<String>` (non-array / non-string entries dropped).
pub(super) struct Sidecar<'a>(pub Option<&'a Value>);
impl Sidecar<'_> {
    pub fn strings(v: &Value) -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A string field from an optional sidecar, empty when absent.
    pub fn string(&self, key: &str) -> String {
        self.0
            .and_then(|m| m[key].as_str())
            .unwrap_or("")
            .to_string()
    }
}

/// Best-effort recovery of an image's environment from an on-disk OCI config, used by
/// [`discover_images`] when the `hl-image.json` sidecar recorded no `env` (pre-seeded / umoci-built
/// images, or images cached before the pull path persisted env). Two layouts are understood, in order:
///   1. umoci's runtime `config.json` at the image dir root -> `process.env`.
///   2. an OCI image layout in the dir (`index.json` + `blobs/sha256/`) -> manifest -> image config
///      blob -> `config.Env`.
/// Returns an empty vec if neither is present/parseable — never panics, never fails discovery.
pub(super) struct OciDisk<'a>(pub &'a Path);
impl OciDisk<'_> {
    pub fn environment(&self) -> Vec<String> {
        let dir = self.0;
        // 1. umoci runtime config: process.env.
        if let Some(cfg) = std::fs::read_to_string(dir.join("config.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        {
            let env = Sidecar::strings(&cfg["process"]["env"]);
            if !env.is_empty() {
                return env;
            }
        }
        // 2. OCI image layout: index.json -> first manifest blob -> image config blob -> config.Env.
        let read_blob = |digest: &str| -> Option<Value> {
            let hex = digest.strip_prefix("sha256:")?;
            std::fs::read_to_string(dir.join("blobs/sha256").join(hex))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        };
        let index = std::fs::read_to_string(dir.join("index.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
        if let Some(mdigest) = index
            .as_ref()
            .and_then(|i| i["manifests"].as_array())
            .and_then(|a| a.first())
            .and_then(|m| m["digest"].as_str())
        {
            if let Some(cfg) = read_blob(mdigest)
                .and_then(|m| m["config"]["digest"].as_str().map(String::from))
                .and_then(|d| read_blob(&d))
            {
                return Sidecar::strings(&cfg["config"]["Env"]);
            }
        }
        Vec::new()
    }
}

/// Persist an env recovered by [`oci_disk_env`] back into the image's `hl-image.json` sidecar so the
/// next discovery round-trips it directly (and never has to re-parse the OCI config). Merges into the
/// existing sidecar when present so other recorded fields are preserved; otherwise writes a fresh one
/// from the values [`discover_images`] already resolved. Best-effort: a write failure (e.g. a
/// read-only image store) is ignored — the in-memory env is still surfaced for this run.
#[allow(clippy::too_many_arguments)]
pub(super) fn persist_discovered_env(
    dir: &Path,
    meta: Option<&Value>,
    name: &str,
    cmd: &[String],
    env: &[String],
    entrypoint: &[String],
    workdir: &str,
    arch: Arch,
) {
    let mut m = meta.cloned().unwrap_or_else(|| {
        json!({
            "name": name, "cmd": cmd, "entrypoint": entrypoint, "workdir": workdir,
            "arch": arch.isa(), "os": arch.os(),
        })
    });
    m["env"] = json!(env);
    let _ = std::fs::write(dir.join("hl-image.json"), m.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- json_strs / meta_str: sidecar JSON field extraction ----

    #[test]
    fn json_strs_keeps_only_string_array_entries() {
        assert_eq!(
            Sidecar::strings(&json!(["a", "b"])),
            vec!["a".to_string(), "b".to_string()]
        );
        // non-string entries (numbers, null, nested) are dropped
        assert_eq!(
            Sidecar::strings(&json!(["a", 1, null, ["x"], "b"])),
            vec!["a".to_string(), "b".to_string()]
        );
        // a non-array value -> empty
        assert_eq!(Sidecar::strings(&json!("scalar")), Vec::<String>::new());
        assert_eq!(Sidecar::strings(&json!(null)), Vec::<String>::new());
    }

    #[test]
    fn meta_str_reads_string_field_else_empty() {
        let meta = Some(json!({"workdir": "/app", "port": 5432}));
        assert_eq!(Sidecar(meta.as_ref()).string("workdir"), "/app");
        // present but non-string -> empty
        assert_eq!(Sidecar(meta.as_ref()).string("port"), "");
        // absent key -> empty
        assert_eq!(Sidecar(meta.as_ref()).string("user"), "");
        // no sidecar at all -> empty
        assert_eq!(Sidecar(None).string("workdir"), "");
    }
}
