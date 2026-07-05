use serde::{Deserialize, Serialize};

/// `--mount` spec (HostConfig.Mounts[]). `typ` is "bind" or "volume"; `source` is a host path (bind) or
/// volume name (volume); `target` is the in-container path. `read_only` is metadata (the JIT's Volume
/// mechanism can't mark a mount read-only). Wired into the rootfs via the same path as `-v`/Binds.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Mount {
    #[serde(rename = "Type", default)]
    pub(crate) typ: String,
    #[serde(rename = "Source", default)]
    pub(crate) source: String,
    #[serde(rename = "Target", default)]
    pub(crate) target: String,
    #[serde(rename = "ReadOnly", default)]
    pub(crate) read_only: bool,
}

/// A named volume — a directory under the volumes root that containers can bind by name.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Vol {
    pub(crate) name: String,
    pub(crate) mountpoint: String,
    pub(crate) created_at: i64,
    #[serde(default)]
    pub(crate) driver: String, // `docker volume create --driver` (default "local")
    #[serde(default)]
    pub(crate) options: std::collections::HashMap<String, String>, // --opt
    #[serde(default)]
    pub(crate) labels: std::collections::HashMap<String, String>, // --label
}
