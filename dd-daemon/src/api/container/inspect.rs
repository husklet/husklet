//! `/containers/{id}/json` (`docker inspect`) DTOs — the resolved `Mounts[]` entries, the big inspect
//! document (State/Config/HostConfig/NetworkSettings), and the nested `NetworkSettings.Ports` binding.

use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

// ---- containers: resolved `Mounts[]` entries -------------------------------
// One entry of the inspect/list `Mounts` array. bind/volume mounts share [`MountPoint`]; tmpfs mounts
// use [`TmpfsMountPoint`] because docker orders their keys differently (Mode after RW, no Propagation).
// `Name`/`Driver`/`Mode` are omitted (not null) when absent, matching the inline shapes exactly.

#[derive(Serialize)]
pub(crate) struct MountPoint {
    #[serde(rename = "Type")]
    pub type_: String,
    #[serde(rename = "Name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "Source")]
    pub source: String,
    #[serde(rename = "Destination")]
    pub destination: String,
    #[serde(rename = "Driver", skip_serializing_if = "Option::is_none")]
    pub driver: Option<&'static str>,
    #[serde(rename = "Mode", skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(rename = "RW")]
    pub rw: bool,
    #[serde(rename = "Propagation")]
    pub propagation: &'static str,
}

#[derive(Serialize)]
pub(crate) struct TmpfsMountPoint {
    #[serde(rename = "Type")]
    pub type_: &'static str,
    #[serde(rename = "Source")]
    pub source: &'static str,
    #[serde(rename = "Destination")]
    pub destination: String,
    #[serde(rename = "RW")]
    pub rw: bool,
    #[serde(rename = "Mode")]
    pub mode: &'static str,
}

// ---- containers: `docker inspect` ------------------------------------------
// The big inspect document. Reuses the model's own `Serialize` types (`HealthState`, `HealthConfig`,
// `Ulimit`, `RestartPolicy`, `DeviceMapping`, `Mount`) which already carry the exact Docker key renames.

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerInspect {
    pub id: String,
    pub image: String,
    pub created: String,
    pub name: String,
    pub state: ContainerState,
    pub config: ContainerConfig,
    pub restart_count: i64,
    pub mounts: Vec<Value>,
    pub host_config: HostConfigJson,
    pub network_settings: NetworkSettingsJson,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerState {
    pub status: String,
    pub exit_code: i64,
    pub running: bool,
    pub paused: bool,
    pub restarting: bool,
    #[serde(rename = "OOMKilled")]
    pub oom_killed: bool,
    pub dead: bool,
    pub error: String,
    pub pid: i64,
    pub started_at: String,
    pub finished_at: String,
    /// Only present when a HEALTHCHECK is configured (docker omits the key otherwise).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<crate::model::HealthState>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerConfig {
    pub cmd: Vec<String>,
    pub hostname: String,
    pub image: String,
    pub env: Vec<String>,
    pub labels: HashMap<String, String>,
    /// `null` when no HEALTHCHECK is configured, else the resolved probe config.
    pub healthcheck: Option<crate::model::HealthConfig>,
    /// `null` when unset, else the configured stop signal (e.g. `"SIGQUIT"`).
    pub stop_signal: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct HostConfigJson {
    pub binds: Vec<String>,
    pub memory: i64,
    pub pids_limit: i64,
    pub nano_cpus: i64,
    pub readonly_rootfs: bool,
    pub ulimits: Vec<crate::model::Ulimit>,
    pub restart_policy: crate::model::RestartPolicy,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub devices: Vec<crate::model::DeviceMapping>,
    pub mounts: Vec<crate::model::Mount>,
    pub tmpfs: HashMap<String, String>,
    pub stop_timeout: i64,
    pub privileged: bool,
    pub security_opt: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetworkSettingsJson {
    /// The `NetworkSettings.Ports` map (built by `ports_map_json`).
    pub ports: Value,
    #[serde(rename = "IPAddress")]
    pub ip_address: String,
    pub gateway: String,
    /// Per-network endpoint identities, keyed by network name (sorted, matching the prior `serde_json::Map`).
    pub networks: BTreeMap<String, EndpointJson>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct EndpointJson {
    #[serde(rename = "NetworkID")]
    pub network_id: String,
    #[serde(rename = "IPAddress")]
    pub ip_address: String,
    pub gateway: String,
    #[serde(rename = "IPPrefixLen")]
    pub ip_prefix_len: i64,
    pub mac_address: String,
}

// ---- containers: published-port binding (`NetworkSettings.Ports`) ----------
// The nested `NetworkSettings.Ports` map value array (`{"HostIp","HostPort"}`); keys aren't plain
// PascalCase (`HostIp`), so each field carries an explicit rename.

/// One binding of the `NetworkSettings.Ports` map value array (`{"HostIp","HostPort"}`); `HostPort` is
/// a string (docker renders it as text).
#[derive(Serialize)]
pub(crate) struct PortBinding {
    #[serde(rename = "HostIp")]
    pub host_ip: String,
    #[serde(rename = "HostPort")]
    pub host_port: String,
}
