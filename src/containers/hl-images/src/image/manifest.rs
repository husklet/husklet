//! The `hl-manifest.json` sidecar carried inside a hl save archive.
//!
//! hl's archive format is intentionally simple (not full OCI): a tar whose top level is the image's
//! `rootfs/` directory plus this manifest recording the image identity + run config. `docker save`
//! writes it, `docker load` reads it back. The field names are the on-disk contract — keep them stable.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The image identity + run config recorded alongside a saved `rootfs/`. `name` (the image identity) is
/// required; every OTHER field defaults, so an older manifest missing the lifecycle fields still loads
/// (a truly rootfs-only archive carries no manifest at all and is handled by the load path's fallback).
/// Serialization writes the identity + core run config unconditionally (matching the historical
/// `docker save` sidecar) and omits the optional lifecycle fields when unset.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// The image reference (`repository:tag`), restored as the loaded image's name.
    pub name: String,
    /// The default command (OCI `Cmd`).
    #[serde(default)]
    pub cmd: Vec<String>,
    /// The environment (`K=V` lines).
    #[serde(default)]
    pub env: Vec<String>,
    /// The entrypoint (prepended to the command).
    #[serde(default)]
    pub entrypoint: Vec<String>,
    /// The working directory (empty if unset).
    #[serde(default)]
    pub workdir: String,
    /// The default run user (empty if unset).
    #[serde(default)]
    pub user: String,
    /// The exposed-port keys (e.g. `"5432/tcp"`).
    #[serde(default)]
    pub exposed_ports: Vec<String>,
    /// The image OS (`"linux"`); absent for a normal linux image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// The guest instruction set (`"x86_64"` / `"aarch64"`), recorded so an ELF-less linux image (scratch/
    /// distroless) restores its arch on load instead of the ELF-sniff-then-arm64 fallback. Omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// Image labels (`LABEL` / `--label`), preserved across save/load. Omitted when empty.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub labels: std::collections::HashMap<String, String>,
    /// The `docker stop` signal (`Config.StopSignal`); omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_signal: Option<String>,
    /// The dirs that get an anonymous volume at run (`Config.Volumes` keys); omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub img_volumes: Vec<String>,
    /// The container healthcheck probe, carried as the raw OCI/docker JSON so this crate stays free of
    /// the daemon's `HealthConfig` type; omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<Value>,
}

impl Manifest {
    /// True when this manifest's `os` is one hl can run: absent/empty or `linux`. A PRESENT but
    /// unsupported os (e.g. `"windows"`, `"darwin"`) is NOT supported and the load path must reject it
    /// rather than importing it as Linux.
    pub fn os_is_supported(&self) -> bool {
        matches!(self.os.as_deref(), None | Some("") | Some("linux"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn full_roundtrip() {
        let m = Manifest {
            name: "busybox:latest".to_string(),
            cmd: vec!["/bin/sh".to_string()],
            env: vec!["PATH=/usr/bin".to_string(), "HOME=/root".to_string()],
            entrypoint: vec!["/entry".to_string()],
            workdir: "/app".to_string(),
            user: "1000".to_string(),
            exposed_ports: vec!["5432/tcp".to_string()],
            os: Some("linux".to_string()),
            arch: None,
            labels: std::collections::HashMap::new(),
            stop_signal: Some("SIGQUIT".to_string()),
            img_volumes: vec!["/data".to_string()],
            healthcheck: Some(json!({"Test": ["CMD", "true"]})),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&s).unwrap();
        // Manifest has no PartialEq; compare via the JSON projection instead.
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::to_value(&back).unwrap()
        );
        assert_eq!(back.os.as_deref(), Some("linux"));
    }

    #[test]
    fn defaults_fill_missing_lifecycle_fields() {
        // A manifest carrying only its identity (older sidecar / rootfs-only save) still loads:
        // every non-identity field defaults.
        let m: Manifest = serde_json::from_str(r#"{"name":"alpine:3.19"}"#).unwrap();
        assert_eq!(m.name, "alpine:3.19");
        assert!(m.cmd.is_empty());
        assert!(m.env.is_empty());
        assert!(m.entrypoint.is_empty());
        assert_eq!(m.workdir, "");
        assert_eq!(m.user, "");
        assert!(m.exposed_ports.is_empty());
        assert_eq!(m.os, None);
        assert_eq!(m.stop_signal, None);
        assert!(m.img_volumes.is_empty());
        assert_eq!(m.healthcheck, None);
    }

    #[test]
    fn bare_empty_object() {
        // NOTE: `name` is the one field WITHOUT `#[serde(default)]`, so a bare `{}` does NOT
        // deserialize (contradicting the doc comment's "every field defaults"). Locking actual
        // behavior; see report.
        let r = serde_json::from_str::<Manifest>("{}");
        assert!(r.is_err());
    }

    // Finding 8 — os support classification: only absent/empty/linux are runnable.
    #[test]
    fn os_is_supported_classifies_os() {
        let mk = |os: Option<&str>| Manifest {
            name: "x:1".to_string(),
            os: os.map(str::to_string),
            ..Default::default()
        };
        assert!(mk(None).os_is_supported());
        assert!(mk(Some("")).os_is_supported());
        assert!(mk(Some("linux")).os_is_supported());
        assert!(!mk(Some("darwin")).os_is_supported());
        assert!(!mk(Some("windows")).os_is_supported());
        assert!(!mk(Some("plan9")).os_is_supported());
    }

    #[test]
    fn optional_fields_omitted_when_unset() {
        // empty img_volumes / None healthcheck (and None os/stop_signal) are omitted from the JSON.
        let m = Manifest {
            name: "img:latest".to_string(),
            ..Default::default()
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.contains("img_volumes"), "empty img_volumes omitted: {s}");
        assert!(!s.contains("healthcheck"), "None healthcheck omitted: {s}");
        assert!(!s.contains("stop_signal"), "None stop_signal omitted: {s}");
        assert!(!s.contains("\"os\""), "None os omitted: {s}");
        // core run-config fields are written unconditionally
        assert!(s.contains("\"name\""));
        assert!(s.contains("\"cmd\""));

        // when set, they ARE present
        let m2 = Manifest {
            name: "img:latest".to_string(),
            img_volumes: vec!["/data".to_string()],
            healthcheck: Some(json!({"Test": ["NONE"]})),
            ..Default::default()
        };
        let s2 = serde_json::to_string(&m2).unwrap();
        assert!(s2.contains("img_volumes"));
        assert!(s2.contains("healthcheck"));
    }
}
