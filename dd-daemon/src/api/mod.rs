//! Typed Docker Engine API **response** DTOs.
//!
//! These `#[derive(Serialize)]` structs replace hand-rolled inline `serde_json::json!({…})` response
//! builders for the small, self-contained handlers (`/version`, `/info`, `/system/df`, `/auth`,
//! networks, volumes, events). They serialize to the EXACT same JSON shape the inline builders
//! produced — clients (docker CLI / bollard) are strict about keys, so the field renames below are
//! load-bearing. Most keys are a plain PascalCase of the snake_case field name (handled by
//! `#[serde(rename_all = "PascalCase")]`); the few that aren't carry an explicit `#[serde(rename)]`
//! (e.g. `ID`, `OSType`, `NCPU`, `MinAPIVersion`, `EndpointID`, `IPv4Address`, `EnableIPv6`, `IPAM`).

use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

// ---- /version --------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Version {
    pub version: String,
    pub api_version: &'static str,
    #[serde(rename = "MinAPIVersion")]
    pub min_api_version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub kernel_version: &'static str,
    pub git_commit: &'static str,
    pub go_version: &'static str,
    pub build_time: &'static str,
    pub experimental: bool,
    pub platform: Platform,
    pub components: Vec<Component>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Platform {
    pub name: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Component {
    pub name: &'static str,
    pub version: String,
    pub details: ComponentDetails,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ComponentDetails {
    pub api_version: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
}

// ---- /info -----------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Info {
    #[serde(rename = "ID")]
    pub id: &'static str,
    pub name: &'static str,
    pub containers: usize,
    pub containers_running: usize,
    pub containers_paused: usize,
    pub containers_stopped: usize,
    pub images: usize,
    pub volumes: usize,
    pub networks: usize,
    pub driver: &'static str,
    pub operating_system: &'static str,
    #[serde(rename = "OSType")]
    pub os_type: &'static str,
    pub architecture: &'static str,
    #[serde(rename = "NCPU")]
    pub ncpu: i64,
    pub mem_total: i64,
    pub kernel_version: &'static str,
    pub server_version: &'static str,
    pub docker_root_dir: String,
    pub cgroup_driver: &'static str,
    pub default_runtime: &'static str,
    pub swarm: Swarm,
    pub plugins: Plugins,
    pub security_options: Vec<Value>,
    pub warnings: Vec<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Swarm {
    pub local_node_state: &'static str,
    pub control_available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Plugins {
    pub volume: Vec<&'static str>,
    pub network: Vec<&'static str>,
    pub authorization: Option<Vec<&'static str>>,
    pub log: Vec<&'static str>,
}

// ---- /auth -----------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct AuthResponse {
    pub status: &'static str,
    pub identity_token: &'static str,
}

// ---- /system/df ------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct DiskUsage {
    pub layers_size: i64,
    pub images: Vec<ImageDf>,
    pub containers: Vec<ContainerDf>,
    pub volumes: Vec<VolumeDf>,
    pub build_cache: Vec<Value>,
    pub builder_size: i64,
    pub image_usage: Usage<ImageDf>,
    pub container_usage: Usage<ContainerDf>,
    pub volume_usage: Usage<VolumeDf>,
    pub build_cache_usage: Usage<Value>,
}

/// The nested `*Usage` object current clients read (`ImageUsage`, `ContainerUsage`, …).
#[derive(Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Usage<T> {
    pub active_count: i64,
    pub total_count: i64,
    pub reclaimable: i64,
    pub total_size: i64,
    pub items: Vec<T>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ImageDf {
    pub id: String,
    pub parent_id: &'static str,
    pub repo_tags: Vec<String>,
    pub created: i64,
    pub size: i64,
    pub shared_size: i64,
    pub virtual_size: i64,
    pub containers: usize,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ContainerDf {
    pub id: String,
    pub image: String,
    pub command: &'static str,
    pub created: i64,
    pub size_rw: i64,
    pub size_root_fs: i64,
    pub state: String,
    pub status: String,
    pub names: Vec<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumeDf {
    pub name: String,
    pub driver: &'static str,
    pub mountpoint: String,
    pub usage_data: VolumeUsageData,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumeUsageData {
    pub size: i64,
    pub ref_count: i64,
}

// ---- networks --------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetworkJson {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub containers: HashMap<String, NetContainer>,
    pub created: String,
    #[serde(rename = "EnableIPv6")]
    pub enable_ipv6: bool,
    pub internal: bool,
    #[serde(rename = "IPAM")]
    pub ipam: Ipam,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetContainer {
    pub name: String,
    #[serde(rename = "EndpointID")]
    pub endpoint_id: String,
    pub mac_address: String,
    #[serde(rename = "IPv4Address")]
    pub ipv4_address: String,
    #[serde(rename = "IPv6Address")]
    pub ipv6_address: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Ipam {
    pub driver: &'static str,
    pub config: Vec<IpamConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct IpamConfig {
    pub subnet: String,
    pub gateway: String,
}

// ---- volumes ---------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumeJson {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub created_at: String,
    pub scope: &'static str,
    pub labels: HashMap<String, String>,
    pub options: HashMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumeList {
    pub volumes: Vec<VolumeJson>,
    pub warnings: Vec<Value>,
}

// ---- events ----------------------------------------------------------------

/// A Docker `events` lifecycle document. `Type`/`Action`/`Actor` are the modern fields; `scope`/
/// `time`/`timeNano` are lowercase, and `status`/`id`/`from` are the legacy top-level aliases emitted
/// only for `container` events (hence `Option` + skip-if-none).
#[derive(Serialize)]
pub(crate) struct Event {
    #[serde(rename = "Type")]
    pub type_: String,
    #[serde(rename = "Action")]
    pub action: String,
    #[serde(rename = "Actor")]
    pub actor: Actor,
    pub scope: &'static str,
    pub time: i64,
    #[serde(rename = "timeNano")]
    pub time_nano: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Actor {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Attributes")]
    pub attributes: Value,
}
