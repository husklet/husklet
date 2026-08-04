use super::{Container, ImageDelete, ImageSummary};
use crate::api::Volume;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Version {
    pub version: String,
    #[serde(rename = "ApiVersion")]
    pub api_version: String,
    #[serde(rename = "MinAPIVersion")]
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
    pub volumes: Vec<Volume>,
    pub build_cache: Vec<BuildCache>,
}

/// Docker build-cache usage row returned by `GET /system/df`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BuildCache {
    #[serde(rename = "ID")]
    pub id: String,
    pub parents: Option<Vec<String>>,
    #[serde(rename = "Type")]
    pub kind: BuildCacheKind,
    pub description: String,
    pub in_use: bool,
    pub shared: bool,
    pub size: i64,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub usage_count: i64,
}

/// Docker's declared build-cache record kinds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum BuildCacheKind {
    #[serde(rename = "internal")]
    Internal,
    #[serde(rename = "frontend")]
    Frontend,
    #[serde(rename = "source.local")]
    SourceLocal,
    #[serde(rename = "source.git.checkout")]
    SourceGitCheckout,
    #[serde(rename = "exec.cachemount")]
    ExecCachemount,
    #[serde(rename = "regular")]
    Regular,
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

/// Docker plugin metadata. The daemon currently returns an empty collection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Plugin {
    pub name: String,
    pub enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::{BuildCache, BuildCacheKind, Version};

    #[test]
    fn build_cache_keys() {
        let value = serde_json::to_value(BuildCache {
            id: "cache-id".into(),
            parents: None,
            kind: BuildCacheKind::SourceLocal,
            description: "local context".into(),
            in_use: false,
            shared: true,
            size: 42,
            created_at: "2026-08-04T00:00:00Z".into(),
            last_used_at: None,
            usage_count: 3,
        })
        .expect("build cache should serialize");

        assert_eq!(value["ID"], "cache-id");
        assert_eq!(value["Parents"], serde_json::Value::Null);
        assert_eq!(value["Type"], "source.local");
        assert_eq!(value["LastUsedAt"], serde_json::Value::Null);
        assert!(value.get("Id").is_none());
    }

    #[test]
    fn version_keys() {
        let value = serde_json::to_value(Version {
            version: "0.1.0".into(),
            api_version: "1.43".into(),
            min_api_version: "1.24".into(),
            os: "linux".into(),
            arch: "amd64".into(),
        })
        .expect("version should serialize");

        assert_eq!(value["ApiVersion"], "1.43");
        assert_eq!(value["MinAPIVersion"], "1.24");
        assert!(value.get("MinApiVersion").is_none());
    }
}
