//! `/containers` DTOs — `top`, `stats`, `Mounts[]`, `inspect`, `ps` list rows, create ack, and the
//! published-port shapes.

use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

// ---- containers: `docker top` ----------------------------------------------

/// `GET /containers/{id}/top` — a single synthetic process row (dd has no guest process tree).
#[derive(Serialize)]
pub(crate) struct ContainerTop {
    #[serde(rename = "Titles")]
    pub titles: Vec<&'static str>,
    #[serde(rename = "Processes")]
    pub processes: Vec<Vec<String>>,
}

// ---- containers: `docker stats` --------------------------------------------
// The stats document keeps Docker's LOWERCASE snake_case keys verbatim (NOT PascalCase), so these
// structs use their plain field names with no rename.

#[derive(Serialize)]
pub(crate) struct ContainerStats {
    pub read: String,
    pub name: String,
    pub id: String,
    pub pids_stats: PidsStats,
    pub cpu_stats: CpuStats,
    pub precpu_stats: CpuStats,
    pub memory_stats: MemoryStats,
    pub blkio_stats: BlkioStats,
    /// Always the empty object `{}` (dd reports no per-interface net counters).
    pub networks: BTreeMap<String, Value>,
}

#[derive(Serialize)]
pub(crate) struct PidsStats {
    pub current: u64,
}

/// One `cpu_stats`/`precpu_stats` block in Docker's shape.
#[derive(Serialize)]
pub(crate) struct CpuStats {
    pub cpu_usage: CpuUsage,
    pub system_cpu_usage: u64,
    pub online_cpus: u32,
    pub throttling_data: ThrottlingData,
}

#[derive(Serialize)]
pub(crate) struct CpuUsage {
    pub total_usage: u64,
    pub usage_in_kernelmode: u64,
    pub usage_in_usermode: u64,
}

#[derive(Serialize)]
pub(crate) struct ThrottlingData {
    pub periods: u64,
    pub throttled_periods: u64,
    pub throttled_time: u64,
}

#[derive(Serialize)]
pub(crate) struct MemoryStats {
    pub usage: u64,
    pub limit: u64,
    /// Always the empty object `{}` (dd has no cgroup memory breakdown).
    pub stats: BTreeMap<String, Value>,
}

/// All eight recursive blkio arrays, always empty (dd has no block-IO accounting).
#[derive(Serialize)]
pub(crate) struct BlkioStats {
    pub io_service_bytes_recursive: Vec<Value>,
    pub io_serviced_recursive: Vec<Value>,
    pub io_queue_recursive: Vec<Value>,
    pub io_service_time_recursive: Vec<Value>,
    pub io_wait_time_recursive: Vec<Value>,
    pub io_merged_recursive: Vec<Value>,
    pub io_time_recursive: Vec<Value>,
    pub sectors_recursive: Vec<Value>,
}

impl BlkioStats {
    pub fn empty() -> Self {
        BlkioStats {
            io_service_bytes_recursive: vec![],
            io_serviced_recursive: vec![],
            io_queue_recursive: vec![],
            io_service_time_recursive: vec![],
            io_wait_time_recursive: vec![],
            io_merged_recursive: vec![],
            io_time_recursive: vec![],
            sectors_recursive: vec![],
        }
    }
}

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

// ---- containers: `docker ps` list rows -------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerSummary {
    pub id: String,
    pub image: String,
    pub command: String,
    pub created: i64,
    pub state: String,
    pub status: String,
    pub exit_code: i64,
    pub ports: Vec<Value>,
    pub labels: HashMap<String, String>,
    pub mounts: Vec<Value>,
    pub names: Vec<String>,
    /// `--size` only: the writable-layer size; omitted otherwise (docker omits the key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_rw: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_root_fs: Option<i64>,
}

// ---- containers: create ack ------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct CreateResponse {
    pub id: String,
    pub warnings: Vec<Value>,
}

// ---- containers: prune / update acks ---------------------------------------

/// `POST /containers/prune` report — the ids of the removed (exited) containers plus reclaimed bytes
/// (always 0; dd does not size container writable layers).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainersPruneReport {
    pub containers_deleted: Vec<String>,
    pub space_reclaimed: i64,
}

/// `POST /containers/{id}/update` ack — `{"Warnings": []}`. dd applies no live resource limits, so the
/// envelope is always empty.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerUpdateResponse {
    pub warnings: Vec<Value>,
}

// ---- containers: `docker diff` changes -------------------------------------

/// One entry of the `GET /containers/{id}/changes` (`docker diff`) array — a changed container-absolute
/// path and its kind (`0`=Modified, `1`=Added, `2`=Deleted).
#[derive(Serialize)]
pub(crate) struct ContainerChange {
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Kind")]
    pub kind: u8,
}

// ---- containers: published-port shapes -------------------------------------
// The two distinct port renderings docker clients read: the top-level `Ports[]` array (list/`ps`) and
// the nested `NetworkSettings.Ports` bindings (`docker port`). Keys aren't plain PascalCase (`IP`,
// `HostIp`), so each field carries an explicit rename.

/// One entry of the top-level `Ports` array (`docker ps` / list JSON).
#[derive(Serialize)]
pub(crate) struct PortSummary {
    #[serde(rename = "PublicPort")]
    pub public_port: u16,
    #[serde(rename = "PrivatePort")]
    pub private_port: u16,
    #[serde(rename = "Type")]
    pub type_: String,
    #[serde(rename = "IP")]
    pub ip: String,
}

/// One binding of the `NetworkSettings.Ports` map value array (`{"HostIp","HostPort"}`); `HostPort` is
/// a string (docker renders it as text).
#[derive(Serialize)]
pub(crate) struct PortBinding {
    #[serde(rename = "HostIp")]
    pub host_ip: String,
    #[serde(rename = "HostPort")]
    pub host_port: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_prune_report_shape() {
        // Populated case.
        assert_eq!(
            serde_json::to_value(ContainersPruneReport {
                containers_deleted: vec!["c1".into(), "c2".into()],
                space_reclaimed: 0,
            })
            .unwrap(),
            serde_json::json!({"ContainersDeleted": ["c1", "c2"], "SpaceReclaimed": 0})
        );
        // Empty case — the deleted list stays an empty array (never null).
        assert_eq!(
            serde_json::to_value(ContainersPruneReport {
                containers_deleted: vec![],
                space_reclaimed: 0,
            })
            .unwrap(),
            serde_json::json!({"ContainersDeleted": [], "SpaceReclaimed": 0})
        );
    }

    #[test]
    fn container_update_response_shape() {
        assert_eq!(
            serde_json::to_value(ContainerUpdateResponse { warnings: vec![] }).unwrap(),
            serde_json::json!({"Warnings": []})
        );
    }

    #[test]
    fn container_change_shape() {
        assert_eq!(
            serde_json::to_value(ContainerChange {
                path: "/etc/hosts".into(),
                kind: 0,
            })
            .unwrap(),
            serde_json::json!({"Path": "/etc/hosts", "Kind": 0})
        );
        // The Kind is a bare number for every kind (added/deleted included).
        assert_eq!(
            serde_json::to_value(ContainerChange {
                path: "/tmp/new".into(),
                kind: 2,
            })
            .unwrap(),
            serde_json::json!({"Path": "/tmp/new", "Kind": 2})
        );
    }
}
