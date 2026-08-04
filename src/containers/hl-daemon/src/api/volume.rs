use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Docker representation of one local volume.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Volume {
    pub created_at: String,
    pub driver: String,
    pub labels: BTreeMap<String, String>,
    pub mountpoint: String,
    pub name: String,
    pub options: BTreeMap<String, String>,
    pub scope: String,
}

#[cfg(feature = "runtime")]
impl From<hl_container::Volume> for Volume {
    fn from(value: hl_container::Volume) -> Self {
        let created_at =
            chrono::DateTime::from_timestamp_millis(i64::try_from(value.created_at_ms).unwrap_or(i64::MAX))
                .unwrap_or(chrono::DateTime::UNIX_EPOCH)
                .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        Self {
            created_at,
            driver: "local".into(),
            labels: value.labels,
            mountpoint: value.path.to_string_lossy().into_owned(),
            name: value.name,
            options: value.options,
            scope: "local".into(),
        }
    }
}

/// Docker local-volume creation request.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct VolumeCreate {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub driver: String,
    #[serde(default)]
    pub driver_opts: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_volume_spec: Option<serde_json::Value>,
    /// Unrecognised Docker request fields retained for explicit daemon validation.
    #[serde(flatten, default)]
    pub unsupported: BTreeMap<String, serde_json::Value>,
}

/// Docker volume-list response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct VolumeList {
    pub volumes: Vec<Volume>,
    pub warnings: Vec<String>,
}

/// Docker volume-prune response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct VolumePrune {
    pub volumes_deleted: Vec<String>,
    pub space_reclaimed: u64,
}
