use super::DockerError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CommitOptions {
    pub container: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default = "default_pause")]
    pub pause: bool,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default, rename = "changes")]
    pub changes: Vec<String>,
    /// Query fields not implemented by the typed commit contract.
    #[serde(flatten)]
    pub unsupported: BTreeMap<String, String>,
}

const fn default_pause() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageCommit {
    pub id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct BuildPrune {
    pub space_reclaimed: u64,
}

/// Docker registry-push progress record.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PushProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "progressDetail", default, skip_serializing_if = "Option::is_none")]
    pub progress_detail: Option<ProgressDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(rename = "errorDetail", default, skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<DockerError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aux: Option<PushAux>,
}

/// Byte counters carried by Docker image-transfer progress records.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProgressDetail {
    pub current: i64,
    pub total: i64,
}

impl PushProgress {
    #[cfg(feature = "runtime")]
    pub(crate) fn bytes(&self) -> bytes::Bytes {
        let mut bytes = serde_json::to_vec(self).expect("push progress is serializable");
        bytes.push(b'\n');
        bytes.into()
    }
}

/// Final content identity reported by a successful push.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PushAux {
    pub tag: String,
    pub digest: String,
    pub size: i64,
}

/// One Docker Hub-style image-search result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Search {
    pub description: String,
    pub is_official: bool,
    pub is_automated: bool,
    pub name: String,
    pub star_count: i64,
}

/// Docker image summary returned by `GET /images/json`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
#[serde(default)]
pub struct ImageSummary {
    #[serde(rename = "Id")]
    pub id: String,
    pub repo_tags: Vec<String>,
    pub repo_digests: Vec<String>,
    pub created: i64,
    pub size: i64,
    pub shared_size: i64,
    pub virtual_size: i64,
    pub labels: BTreeMap<String, String>,
    pub containers: i64,
}

impl ImageSummary {
    /// First repository tag, falling back to Docker's short image identity.
    #[must_use]
    pub fn name(&self) -> String {
        self.repo_tags
            .first()
            .cloned()
            .unwrap_or_else(|| self.id.trim_start_matches("sha256:").chars().take(12).collect())
    }
}

/// Docker image inspection response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct InspectImage {
    #[serde(rename = "Id")]
    pub id: String,
    pub repo_tags: Vec<String>,
    pub repo_digests: Vec<String>,
    pub created: String,
    pub size: i64,
    pub virtual_size: i64,
    pub os: String,
    pub architecture: String,
    pub config: ImageConfig,
}

/// Docker distribution inspection response for a locally resolved image.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Distribution {
    pub descriptor: hl_images::Descriptor,
    pub platforms: Vec<hl_images::Platform>,
}

/// OCI process defaults and labels exposed by Docker image inspection.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageConfig {
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    pub working_dir: String,
    pub user: String,
    pub labels: BTreeMap<String, String>,
    #[serde(rename = "OnBuild")]
    pub onbuild: Vec<String>,
    pub exposed_ports: BTreeMap<String, serde_json::Value>,
    pub volumes: BTreeMap<String, serde_json::Value>,
    pub healthcheck: Option<serde_json::Value>,
    pub stop_signal: Option<String>,
}

/// One Docker-compatible image history response entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageHistory {
    #[serde(rename = "Id")]
    pub id: String,
    pub created: i64,
    pub created_by: String,
    pub tags: Vec<String>,
    pub size: i64,
    pub comment: String,
}

/// One mutation reported by Docker's image-remove endpoint.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageDelete {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untagged: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<String>,
}

/// Result of reclaiming unreferenced image content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImagePrune {
    pub images_deleted: Vec<ImageDelete>,
    pub space_reclaimed: i64,
}

/// Result of loading a Docker image archive.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageLoad {
    pub stream: String,
}

/// One newline-delimited Docker image transfer update.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PullProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "progressDetail", default, skip_serializing_if = "Option::is_none")]
    pub progress_detail: Option<ProgressDetail>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(rename = "errorDetail", default, skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<DockerError>,
}

impl PullProgress {
    #[cfg(feature = "runtime")]
    pub(crate) fn bytes(&self) -> bytes::Bytes {
        let mut bytes = serde_json::to_vec(self).expect("pull progress is serializable");
        bytes.push(b'\n');
        bytes.into()
    }
}

#[cfg(test)]
mod tests {
    use super::{BuildPrune, ImageCommit, ImageSummary, ProgressDetail, PullProgress};

    #[test]
    fn pull_progress_preserves_full_content_identity() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let progress = PullProgress {
            status: Some("Downloaded newer image".into()),
            id: Some(digest.clone()),
            ..PullProgress::default()
        };
        let encoded = serde_json::to_value(progress).unwrap();
        assert_eq!(encoded["id"], digest);
    }

    #[test]
    fn image_transfer_progress_uses_exact_docker_optional_shapes() {
        let progress = PullProgress {
            status: Some("Downloading".into()),
            id: Some("abc123".into()),
            progress_detail: Some(ProgressDetail { current: 5, total: 10 }),
            ..PullProgress::default()
        };
        assert_eq!(
            serde_json::to_value(progress).unwrap(),
            serde_json::json!({
                "status": "Downloading",
                "id": "abc123",
                "progressDetail": {"current": 5, "total": 10}
            })
        );

        let status = serde_json::to_value(PullProgress {
            status: Some("Digest: sha256:deadbeef".into()),
            ..PullProgress::default()
        })
        .unwrap();
        assert_eq!(status, serde_json::json!({"status": "Digest: sha256:deadbeef"}));
        assert!(status.get("progressDetail").is_none());
        assert!(status.get("id").is_none());

        let error = serde_json::to_value(PullProgress {
            error: Some("pull failed".into()),
            error_detail: Some(super::DockerError {
                message: "pull failed".into(),
            }),
            ..PullProgress::default()
        })
        .unwrap();
        assert_eq!(
            error,
            serde_json::json!({
                "errorDetail": {"message": "pull failed"},
                "error": "pull failed"
            })
        );
    }

    #[test]
    fn build_prune_and_commit_use_docker_pascal_case_shapes() {
        assert_eq!(
            serde_json::to_value(BuildPrune { space_reclaimed: 0 }).unwrap(),
            serde_json::json!({"SpaceReclaimed": 0})
        );
        assert_eq!(
            serde_json::to_value(ImageCommit { id: "sha256:id".into() }).unwrap(),
            serde_json::json!({"Id": "sha256:id"})
        );
    }

    #[test]
    fn name_prefers_first_repo_tag() {
        let image = ImageSummary {
            repo_tags: vec!["alpine:latest".into(), "alpine:3.20".into()],
            ..Default::default()
        };
        assert_eq!(image.name(), "alpine:latest");
    }

    #[test]
    fn name_falls_back_to_short_id_when_untagged() {
        let image = ImageSummary {
            id: "sha256:0123456789abcdeffedcba9876543210".into(),
            ..Default::default()
        };
        assert_eq!(image.name(), "0123456789ab");
    }
}
