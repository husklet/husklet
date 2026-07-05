//! `/images` (and `/distribution`) DTOs, plus the `docker push` progress stream.
// Typed replacements for the inline `json!` response builders in `images.rs`. `rename_all =
// "PascalCase"` already yields the Docker keys for the common cases (`Id`, `RepoTags`, `VirtualSize`,
// `ParentId`, `SharedSize`, `WorkingDir`, `ExposedPorts`, `StopSignal`, `CreatedBy`, …); only the
// genuinely-non-PascalCase keys carry an explicit `#[serde(rename)]` (`RootFS`, the camelCase
// `Descriptor` fields). `Empty` serializes to `{}` so an image's `ExposedPorts`/`Volumes` re-materialize
// as the docker set shape `{ "5432/tcp": {} }`.

use super::ErrorMessage;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// An empty object value (`{}`), used as the value type of the `ExposedPorts`/`Volumes` sets and the
/// push-stream `progressDetail` sentinel.
#[derive(Serialize)]
pub(crate) struct Empty {}

/// One row of `GET /images/json` (`docker images`). `VirtualSize` is a required i64 in API ≤1.43;
/// `ParentId`/`RepoDigests`/`SharedSize`/`Containers` take Docker's "not calculated" sentinels.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ImageSummary {
    pub id: String,
    pub repo_tags: Vec<String>,
    pub created: i64,
    pub size: i64,
    pub virtual_size: i64,
    pub parent_id: &'static str,
    pub repo_digests: Vec<Value>,
    pub shared_size: i64,
    pub labels: HashMap<String, String>,
    pub containers: i64,
}

/// One synthetic layer of `GET /images/{name}/history` (`docker history`).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct HistoryLayer {
    pub id: String,
    pub created: i64,
    pub created_by: &'static str,
    pub tags: Vec<String>,
    pub size: i64,
    pub comment: &'static str,
}

/// `POST /images/prune` — nothing reclaimed (dd tracks no dangling images).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct PruneReport {
    pub images_deleted: Vec<Value>,
    pub space_reclaimed: i64,
}

/// `GET /distribution/{name}/json` — minimal conformant manifest descriptor.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct DistributionInspect {
    pub descriptor: Descriptor,
    pub platforms: Vec<PlatformDesc>,
}

/// The `Descriptor` sub-object — its keys are camelCase (`mediaType`), not PascalCase.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Descriptor {
    pub media_type: &'static str,
    pub digest: String,
    pub size: i64,
}

/// One entry of the distribution `Platforms` array (lowercase keys).
#[derive(Serialize)]
pub(crate) struct PlatformDesc {
    pub architecture: &'static str,
    pub os: &'static str,
}

/// `GET /images/{name}/json` (`docker image inspect`). `RootFS` needs an explicit rename (PascalCase
/// would yield `RootFs`, dropping the capital `S`).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ImageInspect {
    pub id: String,
    pub repo_tags: Vec<String>,
    pub repo_digests: Vec<Value>,
    pub architecture: String,
    pub os: String,
    pub size: i64,
    pub virtual_size: i64,
    pub created: String,
    pub config: ImageConfig,
    #[serde(rename = "RootFS")]
    pub root_fs: RootFs,
}

/// The nested image `Config`. `Entrypoint`/`StopSignal`/`Healthcheck` are `Option` so an unset value
/// serializes as `null` (docker clients distinguish null from `[]`/`""`); `ExposedPorts`/`Volumes` are
/// docker sets (`{ "dir": {} }`), sorted (BTreeMap) to match the prior `serde_json::Map`.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ImageConfig {
    pub image: String,
    pub cmd: Vec<String>,
    pub entrypoint: Option<Vec<String>>,
    pub env: Vec<String>,
    pub working_dir: String,
    pub user: String,
    pub exposed_ports: BTreeMap<String, Empty>,
    pub labels: HashMap<String, String>,
    pub stop_signal: Option<String>,
    pub volumes: BTreeMap<String, Empty>,
    pub healthcheck: Option<crate::model::HealthConfig>,
}

/// The inspect `RootFS` object. dd squashes to a single rootfs, so `Layers` is empty.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct RootFs {
    #[serde(rename = "Type")]
    pub type_: &'static str,
    pub layers: Vec<Value>,
}

/// One entry of the `DELETE /images/{name}` (`docker rmi`) report array. The external tag names the key,
/// so `Untagged(s)` serializes to `{"Untagged": s}` and `Deleted(s)` to `{"Deleted": s}`.
#[derive(Serialize)]
pub(crate) enum DeleteRecord {
    Untagged(String),
    Deleted(String),
}

/// `POST /images/load` (`docker load`) success — the single `{"stream": …}` line.
#[derive(Serialize)]
pub(crate) struct LoadResponse {
    pub stream: String,
}

/// `docker import` success — the single `{"status": <new image id>}` progress line.
#[derive(Serialize)]
pub(crate) struct ImportStatus {
    pub status: String,
}

// ---- push progress stream --------------------------------------------------
// The NDJSON status lines `docker push` renders. Keys are docker's lowercase `status`/`id` and the
// camelCase `progressDetail`; `Option` + skip keeps the exact per-line key set (`Preparing` has no
// `progressDetail`, the plain status lines have neither).

#[derive(Serialize)]
pub(crate) struct ProgressDetail {
    pub current: i64,
    pub total: i64,
}

#[derive(Serialize)]
pub(crate) struct StreamStatus {
    pub status: String,
    #[serde(rename = "progressDetail", skip_serializing_if = "Option::is_none")]
    pub progress_detail: Option<ProgressDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// The push stream's `aux` line (`{"progressDetail": {}, "aux": {...}}`) — the docker CLI parses it to
/// print `digest: … size: …`.
#[derive(Serialize)]
pub(crate) struct AuxLine {
    #[serde(rename = "progressDetail")]
    pub progress_detail: Empty,
    pub aux: Aux,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Aux {
    pub tag: String,
    pub digest: String,
    pub size: i64,
}

// ---- pull progress stream --------------------------------------------------
// The NDJSON status lines `docker pull` renders (`stream.rs`). Structurally identical to the push
// `StreamStatus` — docker's lowercase `status`/`id` plus the camelCase `progressDetail` — but kept a
// distinct type so pull/push wire shapes evolve independently. `Option` + skip reproduces the exact
// per-line key set: the plain `Pulling from …`/`Digest: …`/`Status: …` lines carry only `status`
// (no `id`, no `progressDetail`), so those keys must be OMITTED, not serialized as `null`. Reuses the
// push-stream `ProgressDetail { current, total }` for the moving download/extract bars.
#[derive(Serialize)]
pub(crate) struct PullProgress {
    pub status: String,
    #[serde(rename = "progressDetail", skip_serializing_if = "Option::is_none")]
    pub progress_detail: Option<ProgressDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// The pull stream's error line (`{"errorDetail": {"message": …}, "error": …}`). `errorDetail` is
/// camelCase; both it and top-level `error` carry the same message. Reuses [`ErrorMessage`] for the
/// nested `{"message": …}` object.
#[derive(Serialize)]
pub(crate) struct PullError {
    #[serde(rename = "errorDetail")]
    pub error_detail: ErrorMessage,
    pub error: String,
}

#[cfg(test)]
mod pull_tests {
    use super::*;
    use serde_json::json;

    // A moving-bar line carries all three keys (status + progressDetail + id) in the docker shape.
    #[test]
    fn pull_progress_full_matches_inline_json() {
        let dto = PullProgress {
            status: "Downloading".into(),
            progress_detail: Some(ProgressDetail {
                current: 5,
                total: 10,
            }),
            id: Some("abc123".into()),
        };
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            json!({ "status": "Downloading", "progressDetail": { "current": 5, "total": 10 }, "id": "abc123" })
        );
    }

    // A plain status line (`Digest:`/`Status:`) has NEITHER `progressDetail` NOR `id`: both must be
    // OMITTED (skip_serializing_if), never emitted as `null`.
    #[test]
    fn pull_progress_status_only_omits_optional_keys() {
        let dto = PullProgress {
            status: "Digest: sha256:deadbeef".into(),
            progress_detail: None,
            id: None,
        };
        let v = serde_json::to_value(&dto).unwrap();
        assert_eq!(v, json!({ "status": "Digest: sha256:deadbeef" }));
        // Prove the keys are absent (skipped), not present-with-null.
        assert!(v.get("progressDetail").is_none());
        assert!(v.get("id").is_none());
    }

    // The bare `progressDetail` object.
    #[test]
    fn progress_detail_matches_inline_json() {
        let dto = ProgressDetail {
            current: 512,
            total: 1024,
        };
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            json!({ "current": 512, "total": 1024 })
        );
    }

    // The error line: camelCase `errorDetail` wrapping `{message}`, plus top-level `error`.
    #[test]
    fn pull_error_matches_inline_json() {
        let dto = PullError {
            error_detail: ErrorMessage {
                message: "pull failed".into(),
            },
            error: "pull failed".into(),
        };
        assert_eq!(
            serde_json::to_value(&dto).unwrap(),
            json!({ "errorDetail": { "message": "pull failed" }, "error": "pull failed" })
        );
    }
}
