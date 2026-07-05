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
use std::collections::{BTreeMap, HashMap};

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

/// `POST /networks/create` ack — `{"Id": <id>, "Warning": ""}`.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetworkCreateResponse {
    pub id: String,
    pub warning: String,
}

/// `POST /networks/prune` report — the names of the removed user-defined networks.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct NetworksPruneReport {
    pub networks_deleted: Vec<String>,
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

/// `POST /volumes/prune` report — the names of removed volumes plus reclaimed bytes (always 0; dd
/// does not size volume contents).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct VolumesPruneReport {
    pub volumes_deleted: Vec<String>,
    pub space_reclaimed: i64,
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

// ---- exec ------------------------------------------------------------------

/// `POST /containers/{id}/exec` ack — `{"Id": <exec id>}`.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ExecCreateResponse {
    pub id: String,
}

/// `GET /exec/{id}/json` (`docker exec` inspect). `ID`/`ContainerID` need explicit renames (PascalCase
/// would drop the capitalization).
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct ExecInspect {
    #[serde(rename = "ID")]
    pub id: String,
    pub running: bool,
    pub exit_code: i64,
    #[serde(rename = "ContainerID")]
    pub container_id: String,
    pub process_config: ExecProcessConfig,
}

/// The nested `ProcessConfig` — docker's lowercase keys verbatim (no PascalCase).
#[derive(Serialize)]
pub(crate) struct ExecProcessConfig {
    pub tty: bool,
    pub privileged: bool,
    pub entrypoint: String,
    pub arguments: Vec<String>,
}

// ---- images -----------------------------------------------------------------
// Typed replacements for the inline `json!` response builders in `images.rs`. `rename_all =
// "PascalCase"` already yields the Docker keys for the common cases (`Id`, `RepoTags`, `VirtualSize`,
// `ParentId`, `SharedSize`, `WorkingDir`, `ExposedPorts`, `StopSignal`, `CreatedBy`, …); only the
// genuinely-non-PascalCase keys carry an explicit `#[serde(rename)]` (`RootFS`, the camelCase
// `Descriptor` fields). `Empty` serializes to `{}` so an image's `ExposedPorts`/`Volumes` re-materialize
// as the docker set shape `{ "5432/tcp": {} }`.

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
