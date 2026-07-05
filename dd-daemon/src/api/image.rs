//! `/images` (and `/distribution`) DTOs, plus the `docker push` progress stream.
// Typed replacements for the inline `json!` response builders in `images.rs`. `rename_all =
// "PascalCase"` already yields the Docker keys for the common cases (`Id`, `RepoTags`, `VirtualSize`,
// `ParentId`, `SharedSize`, `WorkingDir`, `ExposedPorts`, `StopSignal`, `CreatedBy`, …); only the
// genuinely-non-PascalCase keys carry an explicit `#[serde(rename)]` (`RootFS`, the camelCase
// `Descriptor` fields). `Empty` serializes to `{}` so an image's `ExposedPorts`/`Volumes` re-materialize
// as the docker set shape `{ "5432/tcp": {} }`.

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
