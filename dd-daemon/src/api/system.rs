//! `/info`, `/auth`, and `/system/df` DTOs.

use serde::Serialize;
use serde_json::Value;

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
    /// The runtimes map Docker clients validate `DefaultRuntime` against. Must contain `default_runtime`.
    pub runtimes: std::collections::HashMap<&'static str, Runtime>,
    pub swarm: Swarm,
    pub plugins: Plugins,
    pub security_options: Vec<Value>,
    pub warnings: Vec<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Runtime {
    pub path: &'static str,
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
