//! The `dd-manifest.json` sidecar carried inside a dd save archive.
//!
//! dd's archive format is intentionally simple (not full OCI): a tar whose top level is the image's
//! `rootfs/` directory plus this manifest recording the image identity + run config. `docker save`
//! writes it, `docker load` reads it back. The field names are the on-disk contract — keep them stable.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The image identity + run config recorded alongside a saved `rootfs/`. Deserialization is lenient
/// (every field defaults) so a rootfs-only archive — or an older manifest missing the lifecycle fields —
/// still loads. Serialization writes the identity + core run config unconditionally (matching the
/// historical `docker save` sidecar) and omits the optional lifecycle fields when unset.
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
    /// `"darwin"` for a native-macOS image; absent for a normal linux image.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
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
    /// True when this manifest describes a native-macOS (darwinjail) image.
    pub fn is_darwin(&self) -> bool {
        self.os.as_deref() == Some("darwin")
    }
}
