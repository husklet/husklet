use super::{Container, ImageDelete, ImageSummary};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Version {
    pub version: String,
    #[serde(rename = "ApiVersion")]
    pub api_version: String,
    pub min_api_version: String,
    pub os: String,
    pub arch: String,
}

/// Truthful subset of Docker's daemon information response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SystemInfo {
    #[serde(rename = "ID")]
    pub id: String,
    pub containers: i64,
    pub containers_running: i64,
    pub containers_paused: i64,
    pub containers_stopped: i64,
    pub images: i64,
    pub driver: String,
    pub memory_limit: bool,
    #[serde(rename = "NCPU")]
    pub ncpu: i64,
    #[serde(rename = "OSType")]
    pub os_type: String,
    pub architecture: String,
    pub operating_system: String,
    pub name: String,
    pub server_version: String,
}

/// Docker system disk-usage response derived from the durable stores.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DiskUsage {
    pub layers_size: i64,
    pub images: Vec<ImageSummary>,
    pub containers: Vec<Container>,
    pub volumes: Vec<VolumeUsage>,
}

/// Result of pruning every unused daemon-owned resource.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct SystemPrune {
    pub containers_deleted: Vec<String>,
    pub images_deleted: Vec<ImageDelete>,
    pub networks_deleted: Vec<String>,
    pub volumes_deleted: Vec<String>,
    pub space_reclaimed: u64,
}

/// Durable volume disk usage and reference accounting.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct VolumeUsage {
    pub name: String,
    pub mountpoint: String,
    pub usage_data: UsageData,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UsageData {
    pub size: i64,
    pub ref_count: i64,
}

/// Docker plugin metadata. The daemon currently returns an empty collection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Plugin {
    pub name: String,
    pub enabled: bool,
}
