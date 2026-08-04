use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

/// Docker representation of one local volume.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Volume {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_at: String,
    pub driver: String,
    #[serde(default, deserialize_with = "deserialize_null_map")]
    pub labels: BTreeMap<String, String>,
    pub mountpoint: String,
    pub name: String,
    #[serde(default, deserialize_with = "deserialize_null_map")]
    pub options: BTreeMap<String, String>,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_data: Option<UsageData>,
}

/// Disk accounting attached to a volume by `GET /system/df`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UsageData {
    pub size: i64,
    pub ref_count: i64,
}

fn deserialize_null_map<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<BTreeMap<String, String>>::deserialize(deserializer)?.unwrap_or_default())
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
            usage_data: None,
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

#[cfg(test)]
mod tests {
    use super::{UsageData, Volume};
    use std::collections::BTreeMap;

    fn volume() -> Volume {
        Volume {
            created_at: "2026-08-04T12:00:00.000000000Z".into(),
            driver: "local".into(),
            labels: BTreeMap::from([("purpose".into(), "test".into())]),
            mountpoint: "/volumes/data".into(),
            name: "data".into(),
            options: BTreeMap::from([("type".into(), "none".into())]),
            scope: "local".into(),
            usage_data: None,
        }
    }

    #[test]
    fn usage_shape() {
        let mut volume = volume();
        let ordinary = serde_json::to_value(&volume).unwrap();
        assert_eq!(
            ordinary,
            serde_json::json!({
                "CreatedAt": "2026-08-04T12:00:00.000000000Z",
                "Driver": "local",
                "Labels": {"purpose": "test"},
                "Mountpoint": "/volumes/data",
                "Name": "data",
                "Options": {"type": "none"},
                "Scope": "local"
            })
        );

        volume.usage_data = Some(UsageData { size: 5, ref_count: 1 });
        assert_eq!(
            serde_json::to_value(volume).unwrap()["UsageData"],
            serde_json::json!({"Size": 5, "RefCount": 1})
        );
    }

    #[test]
    fn null_decode() {
        let volume: Volume = serde_json::from_value(serde_json::json!({
            "Driver": "local",
            "Labels": null,
            "Mountpoint": "/volumes/data",
            "Name": "data",
            "Options": null,
            "Scope": "local",
            "UsageData": null
        }))
        .unwrap();
        assert!(volume.created_at.is_empty());
        assert!(volume.labels.is_empty());
        assert!(volume.options.is_empty());
        assert_eq!(volume.usage_data, None);

        let encoded = serde_json::to_value(volume).unwrap();
        assert!(encoded.get("CreatedAt").is_none());
        assert!(encoded.get("UsageData").is_none());
    }
}
