//! The narrow traits this crate declares and something else implements.
//!
//! Each is single-purpose. There is deliberately no omnibus host trait: a
//! dispatcher should be able to reach exactly the service it was granted and
//! nothing adjacent to it.

use hl_rpc::RelativePath;

/// Why a host operation failed. Distinguishes a refusal from a breakage, so a
/// caller can tell "you may not" from "it did not work".
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostError {
    /// The named thing does not exist.
    Absent(String),
    /// The request was well formed but cannot apply in this state.
    Conflict(String),
    /// The host service failed.
    Failed(String),
    /// The host genuinely does not implement this operation.
    Unsupported(String),
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent(detail) => write!(formatter, "not found: {detail}"),
            Self::Conflict(detail) => write!(formatter, "conflict: {detail}"),
            Self::Failed(detail) => write!(formatter, "failed: {detail}"),
            Self::Unsupported(detail) => write!(formatter, "unsupported: {detail}"),
        }
    }
}

impl std::error::Error for HostError {}

/// A container as an extension sees it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ContainerSummary {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    pub created: i64,
}

/// The process table reported by a running container.
///
/// Columns are named explicitly because the daemon's process sampler is
/// platform-owned; preserving its titles keeps every row unambiguous without
/// pretending all hosts can report one fixed process schema.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ProcessList {
    pub titles: Vec<String>,
    pub processes: Vec<Vec<String>>,
    /// Host wall-clock time at which this point-in-time view was produced.
    #[serde(default)]
    pub observed_at_ms: u64,
    /// This host currently exposes only the container's initial process, not a
    /// complete namespace process tree.
    #[serde(default)]
    pub scope: ProcessScope,
    /// PIDs identify rows only within this snapshot and may be reused later.
    #[serde(default)]
    pub pid_identity: ProcessPidIdentity,
    /// Rows, columns, or cell bytes were omitted to preserve the wire bound.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessScope {
    #[default]
    Initial,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessPidIdentity {
    #[default]
    Snapshot,
}

/// Bounded captured output from a container's initial process.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ContainerOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// At least one stream was shortened to the protocol limit.
    pub truncated: bool,
    /// Standard output was shortened independently of standard error.
    #[serde(default)]
    pub stdout_truncated: bool,
    /// Standard error was shortened independently of standard output.
    #[serde(default)]
    pub stderr_truncated: bool,
    /// The process was already complete when this replay began, so no later
    /// bytes can appear. False is conservative for older hosts.
    #[serde(default)]
    pub eof: bool,
}

/// Bounded container creation authority with no host bind-mount path.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ContainerCreateSpec {
    pub image: String,
    pub name: String,
    pub entrypoint: Option<Vec<String>>,
    pub command: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub working_directory: Option<String>,
    pub user: Option<String>,
    pub labels: Vec<(String, String)>,
    pub mounts: Vec<ContainerVolumeMount>,
    pub network: Option<String>,
    pub ports: Vec<ContainerPort>,
    pub memory_mb: Option<u32>,
    pub cpus: Option<u16>,
    pub pids_limit: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ContainerVolumeMount {
    pub volume: String,
    pub target: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ContainerPort {
    pub container: u16,
    pub host: Option<u16>,
    pub protocol: String,
}

/// State of one additional process created through the container exec API.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExecutionSummary {
    pub id: String,
    pub container_id: String,
    pub running: bool,
    pub exit_code: i64,
    pub pid: i64,
    pub command: Vec<String>,
    pub user: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExecutionList {
    pub executions: Vec<ExecutionSummary>,
    pub truncated: bool,
}

/// An image as an extension sees it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ImageSummary {
    pub id: String,
    pub reference: String,
    pub size: u64,
    pub created: i64,
}

/// Bounded, useful image inspection data. Environment values and arbitrary
/// labels are intentionally not exposed through this inventory API.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ImageDetails {
    pub id: String,
    pub references: Vec<String>,
    pub created: String,
    pub size: u64,
    pub os: String,
    pub architecture: String,
    pub entrypoint: Vec<String>,
    pub command: Vec<String>,
    pub working_directory: String,
    pub user: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ImagePruneResult {
    pub deleted: u64,
    pub space_reclaimed: u64,
}

/// Opaque identity returned when a bounded background image pull starts.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ImagePullJob {
    pub job: String,
}

/// Latest truthful state of one image pull. Byte totals are absent when the registry omits them.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ImagePullStatus {
    pub job: String,
    pub reference: String,
    pub revision: u64,
    pub state: String,
    pub status: Option<String>,
    pub layer: Option<String>,
    pub current: Option<u64>,
    pub total: Option<u64>,
    pub image: Option<ImageSummary>,
    pub error: Option<String>,
}

/// Coalesced invalidation; callers read status for the bounded full snapshot.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ImagePullChange {
    pub job: String,
    pub revision: u64,
    pub state: String,
    pub coalesced: u64,
}

/// A local volume as an extension sees it. The daemon does not calculate
/// recursive disk usage during inventory, so this deliberately carries no
/// synthetic size.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct VolumeSummary {
    pub name: String,
    pub driver: String,
    pub generation: String,
}

/// A workspace-local network as an extension sees it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct NetworkSummary {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
}

/// A terminal tab and what occupies it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TabSummary {
    pub id: String,
    pub title: String,
    pub panes: Vec<PaneSummary>,
}

/// One pane and the command running in it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PaneSummary {
    pub slot: String,
    pub working_directory: Option<String>,
    pub command: Option<String>,
    /// What occupies the pane: a shell, or an interface an extension draws.
    pub occupant: Occupant,
    /// Which extension/provider owns a surface pane; absent for terminals.
    pub provider: Option<PaneProviderIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PaneProviderIdentity {
    pub extension: String,
    pub provider: String,
}

/// What a pane holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Occupant {
    /// A terminal running a shell.
    Terminal,
    /// A surface an extension renders its interface into.
    Surface,
}

/// The text a pane is showing, as lines, oldest first.
///
/// Lines rather than one blob: a caller asking for the tail of a pane is
/// counting lines, and a host that had to cut the answer has to be able to say
/// so at the line it cut.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PaneText {
    pub slot: String,
    pub lines: Vec<String>,
    /// Zero-based cursor column in the terminal's visible grid.
    #[serde(default)]
    pub cursor_column: u32,
    /// Zero-based cursor row in the terminal's visible grid.
    #[serde(default)]
    pub cursor_row: u32,
    /// Whether older lines exist that this answer does not carry.
    pub truncated: bool,
}

pub const SEMANTIC_NODE_LIMIT: usize = 256;
pub const SEMANTIC_DEPTH_LIMIT: usize = 32;
pub const SEMANTIC_TEXT_LIMIT: usize = 256;
pub const SEMANTIC_ACTION_VALUE_LIMIT: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PaneSemanticTree {
    pub slot: String,
    pub revision: u64,
    pub root: SemanticNode,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SemanticNode {
    pub id: u64,
    pub role: String,
    pub label: Option<String>,
    pub value: Option<String>,
    pub disabled: bool,
    /// Whether invoking this node performs an irreversible operation.
    pub destructive: bool,
    pub actions: Vec<SemanticActionKind>,
    pub children: Vec<SemanticNode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticActionKind {
    Invoke,
    Change,
    Submit,
    Toggle,
    Expand,
    Focus,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PaneSemanticAction {
    pub revision: u64,
    pub node: u64,
    pub action: SemanticActionKind,
    pub value: Option<String>,
}

/// The maximum bytes one terminal-input call may inject.
pub const PANE_INPUT_BYTES: usize = 64 * 1024;
/// Maximum argv entries accepted when replacing one terminal pane process.
pub const TERMINAL_COMMAND_ARGUMENTS: usize = 64;
/// Maximum UTF-8 bytes in one terminal command argument.
pub const TERMINAL_COMMAND_ARGUMENT_BYTES: usize = 4096;
/// Maximum aggregate UTF-8 bytes in a terminal command argv.
pub const TERMINAL_COMMAND_BYTES: usize = 32 * 1024;

/// The maximum rows or columns one explicit PTY grid may request.
pub const PANE_GRID_EDGE: u16 = 1000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GridSize {
    pub columns: u16,
    pub rows: u16,
}

/// Nested layout of all visible terminal tabs.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TerminalTopology {
    pub active_tab: Option<String>,
    pub tabs: Vec<TabTopology>,
}

/// A bounded inventory of every pane an agent may subsequently inspect.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PaneInventory {
    pub panes: Vec<InspectablePane>,
    pub truncated: bool,
}

/// Discovery metadata only: contents and semantic values require their own grants.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct InspectablePane {
    pub slot: String,
    pub kind: PaneKind,
    pub provider: Option<PaneProviderIdentity>,
    pub tab: Option<String>,
    pub title: Option<String>,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PaneKind {
    Terminal,
    Surface,
    Native,
}

/// Maximum descriptors returned by one discovery call.
pub const PANE_INVENTORY_LIMIT: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TabTopology {
    pub id: String,
    pub title: String,
    pub root: LayoutNode,
}

/// A leaf pane or a nested split. `ratio_per_mille` is the first child's share.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LayoutNode {
    Pane {
        pane: PaneSummary,
        grid: Option<GridSize>,
        focused: bool,
    },
    Split {
        division: Division,
        ratio_per_mille: u16,
        first: Box<Self>,
        second: Box<Self>,
    },
}

/// The greatest number of lines one pane read may answer with.
///
/// A pane's scrollback is as large as its shell made it, and an answer is built
/// in memory on the drawing thread before it is sent. The cap is what stops a
/// single call from making the host allocate whatever a runaway command printed.
pub const PANE_LINES: usize = 2000;
/// Maximum UTF-8 payload carried by one terminal screen reply.
pub const PANE_TEXT_BYTES: usize = 512 * 1024;

/// How many lines a pane read actually returns.
///
/// An unstated tail is the whole allowance rather than everything there is, so
/// a caller that names no bound still cannot ask for an unbounded read.
#[must_use]
pub fn pane_lines(requested: Option<usize>) -> usize {
    requested.unwrap_or(PANE_LINES).clamp(1, PANE_LINES)
}

/// Retains the newest complete terminal lines that fit one bounded reply.
#[must_use]
pub fn bounded_pane_text(mut text: PaneText) -> PaneText {
    let mut used = 0usize;
    let mut kept = Vec::new();
    for line in text.lines.into_iter().rev() {
        let needed = line.len().saturating_add(1);
        if used.saturating_add(needed) > PANE_TEXT_BYTES {
            text.truncated = true;
            break;
        }
        used += needed;
        kept.push(line);
    }
    kept.reverse();
    text.lines = kept;
    text
}

/// A workspace as an extension sees it from the outside.
///
/// Deliberately thin. The host knows which workspaces are configured and
/// whether each one's execution domain is up; what is running *inside* another
/// workspace is that workspace's daemon's to answer, and is reported only for
/// the one this extension is hosted by, through [`ContainerInventory`].
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceState {
    pub name: String,
    pub architecture: String,
    pub image: String,
    /// Whether this workspace's execution domain is accepting connections.
    pub running: bool,
    /// Whether this is the workspace the calling extension is hosted by.
    pub current: bool,
}

/// Complete extension-facing workspace configuration.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceConfiguration {
    #[serde(default)]
    pub generation: String,
    pub name: String,
    pub image: String,
    pub architecture: String,
    pub storage: Option<String>,
    pub shell: Option<String>,
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    pub environment: Vec<(String, String)>,
    pub mounts: Vec<WorkspaceMount>,
    pub docker_socket: bool,
    pub scrollback: Option<u64>,
    pub vpn: Option<String>,
    pub execution_lifetime: String,
    pub terminal: WorkspaceTerminal,
}

/// One host path exposed inside a workspace.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceMount {
    pub host: String,
    pub container: String,
    pub read_only: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceTerminal {
    pub font_family: Option<String>,
    pub font_size: Option<u16>,
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor_shape: Option<String>,
    pub cursor_blink: Option<bool>,
}

/// How a pane is divided.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Division {
    Beside,
    Below,
}

/// One entry in a listed directory.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Entry {
    pub path: RelativePath,
    pub directory: bool,
    pub size: u64,
}

/// One installed extension and its durable lifecycle policy.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionSummary {
    pub name: String,
    pub image_digest: String,
    pub status: String,
}

pub const EXTENSION_REFERENCE_BYTES: usize = 512;
pub const EXTENSION_JOB_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionAcquisitionJob {
    pub job: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionCandidate {
    pub name: crate::ExtensionName,
    pub version: String,
    pub image_digest: String,
    pub requested: crate::Grant,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionAcquisitionProgress {
    pub status: String,
    pub id: Option<String>,
    pub current: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExtensionAcquisitionStatus {
    pub job: String,
    pub reference: String,
    pub revision: u64,
    pub state: String,
    #[serde(default)]
    pub progress: Option<ExtensionAcquisitionProgress>,
    pub candidate: Option<ExtensionCandidate>,
    pub error: Option<String>,
}

/// Installed-extension inventory and persisted lifecycle controls.
pub trait ExtensionStore {
    fn list(&self) -> Result<Vec<ExtensionSummary>, HostError> {
        Err(HostError::Unsupported("extension inventory is unavailable".into()))
    }
    fn inspect(&self, _name: &str) -> Result<ExtensionSummary, HostError> {
        Err(HostError::Unsupported("extension inspection is unavailable".into()))
    }
    fn enable(&self, _name: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("extension enable is unavailable".into()))
    }
    fn disable(&self, _name: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("extension disable is unavailable".into()))
    }
    fn remove(&self, _name: &str, _image_digest: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("extension removal is unavailable".into()))
    }
    fn acquisition_start(&self, _reference: &str) -> Result<ExtensionAcquisitionJob, HostError> {
        Err(HostError::Unsupported("extension acquisition is unavailable".into()))
    }
    fn acquisition_status(&self, _job: &str) -> Result<ExtensionAcquisitionStatus, HostError> {
        Err(HostError::Unsupported("extension acquisition is unavailable".into()))
    }
    fn acquisition_cancel(&self, _job: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("extension acquisition is unavailable".into()))
    }
    fn install(&self, _job: &str, _revision: u64, _granted: &crate::Grant) -> Result<ExtensionSummary, HostError> {
        Err(HostError::Unsupported("extension installation is unavailable".into()))
    }
    fn update(&self, _job: &str, _revision: u64, _granted: &crate::Grant) -> Result<ExtensionSummary, HostError> {
        Err(HostError::Unsupported("extension update is unavailable".into()))
    }
}

/// Reading container state.
pub trait ContainerInventory {
    /// # Errors
    /// Returns a host failure.
    fn list(&self) -> Result<Vec<ContainerSummary>, HostError>;

    /// # Errors
    /// Returns `HostError::Absent` when no such container exists.
    fn inspect(&self, id: &str) -> Result<ContainerSummary, HostError>;

    /// Lists the live processes in one running container.
    ///
    /// # Errors
    /// Returns an absence, inactive-container conflict, unsupported sampler,
    /// or host failure honestly as supplied by the daemon.
    fn processes(&self, _id: &str) -> Result<ProcessList, HostError> {
        Err(HostError::Unsupported(
            "container process listing is unsupported by this host".into(),
        ))
    }

    /// Reads bounded stdout and stderr captured from the initial process.
    ///
    /// # Errors
    /// Returns a host failure.
    fn logs(&self, _id: &str, _stdout: bool, _stderr: bool) -> Result<ContainerOutput, HostError> {
        Err(HostError::Unsupported(
            "container logs are unsupported by this host".into(),
        ))
    }

    /// Inspects an additional process by its exec identity.
    ///
    /// # Errors
    /// Returns `HostError::Absent` when no such execution exists.
    fn execution(&self, _id: &str) -> Result<ExecutionSummary, HostError> {
        Err(HostError::Unsupported(
            "execution inspection is unsupported by this host".into(),
        ))
    }

    fn executions(&self) -> Result<ExecutionList, HostError> {
        Err(HostError::Unsupported(
            "execution listing is unsupported by this host".into(),
        ))
    }

    fn execution_logs(&self, _id: &str, _stdout: bool, _stderr: bool) -> Result<ContainerOutput, HostError> {
        Err(HostError::Unsupported(
            "execution logs are unsupported by this host".into(),
        ))
    }

    /// Waits at most `timeout_ms` for an execution to stop, then returns its final state.
    fn execution_wait(&self, _id: &str, _timeout_ms: u32) -> Result<ExecutionSummary, HostError> {
        Err(HostError::Unsupported(
            "execution waiting is unsupported by this host".into(),
        ))
    }
}

/// Changing container state. Granting this is granting code execution inside
/// the workspace, which the consent prompt must say plainly.
pub trait ContainerControl {
    /// # Errors
    /// Returns a host failure.
    fn create(&self, image: &str, name: &str) -> Result<String, HostError>;

    fn create_spec(&self, spec: &ContainerCreateSpec) -> Result<String, HostError> {
        if spec.entrypoint.is_none()
            && spec.command.is_empty()
            && spec.environment.is_empty()
            && spec.working_directory.is_none()
            && spec.user.is_none()
            && spec.labels.is_empty()
            && spec.mounts.is_empty()
            && spec.network.is_none()
            && spec.ports.is_empty()
            && spec.memory_mb.is_none()
            && spec.cpus.is_none()
            && spec.pids_limit.is_none()
        {
            self.create(&spec.image, &spec.name)
        } else {
            Err(HostError::Unsupported(
                "configured container creation is unavailable".into(),
            ))
        }
    }

    /// # Errors
    /// Returns a host failure.
    fn start(&self, id: &str) -> Result<(), HostError>;

    /// # Errors
    /// Returns a host failure.
    fn stop(&self, id: &str) -> Result<(), HostError>;

    /// # Errors
    /// Returns a host failure.
    fn remove(&self, id: &str) -> Result<(), HostError>;

    /// Suspends a running container.
    fn pause(&self, _id: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported(
            "container pause is unsupported by this host".into(),
        ))
    }

    /// Resumes a paused container.
    fn unpause(&self, _id: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported(
            "container unpause is unsupported by this host".into(),
        ))
    }

    /// Stops and starts a running container.
    fn restart(&self, _id: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported(
            "container restart is unsupported by this host".into(),
        ))
    }

    /// Delivers a validated Linux signal to a running container.
    fn kill(&self, _id: &str, _signal: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported(
            "container signaling is unsupported by this host".into(),
        ))
    }

    /// Delivers a validated signal to one additional execution, without
    /// signaling its owning container.
    fn execution_kill(&self, _id: &str, _signal: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported(
            "execution signaling is unsupported by this host".into(),
        ))
    }

    /// Removes one stopped execution record and its captured output.
    fn execution_remove(&self, _id: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported(
            "execution removal is unsupported by this host".into(),
        ))
    }

    /// Starts an additional process detached from the extension connection and
    /// returns its durable exec identity.
    fn execute(
        &self,
        _id: &str,
        _command: &[String],
        _user: Option<&str>,
        _working_directory: Option<&str>,
    ) -> Result<String, HostError> {
        Err(HostError::Unsupported(
            "container exec is unsupported by this host".into(),
        ))
    }
}

/// Reading and fetching images.
pub trait ImageStore {
    /// # Errors
    /// Returns a host failure.
    fn list(&self) -> Result<Vec<ImageSummary>, HostError>;

    /// # Errors
    /// Returns a host failure.
    fn pull(&self, reference: &str) -> Result<ImageSummary, HostError>;

    fn pull_start(&self, _reference: &str) -> Result<ImagePullJob, HostError> {
        Err(HostError::Unsupported("image pull progress is unavailable".into()))
    }
    fn pull_status(&self, _job: &str) -> Result<ImagePullStatus, HostError> {
        Err(HostError::Unsupported("image pull progress is unavailable".into()))
    }
    fn pull_cancel(&self, _job: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("image pull cancellation is unavailable".into()))
    }
    fn pull_changes(&self) -> Vec<ImagePullChange> {
        Vec::new()
    }

    fn inspect(&self, _reference: &str) -> Result<ImageDetails, HostError> {
        Err(HostError::Unsupported("image inspection is unavailable".into()))
    }

    fn remove(&self, _reference: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("image removal is unavailable".into()))
    }

    fn prune(&self) -> Result<ImagePruneResult, HostError> {
        Err(HostError::Unsupported("image pruning is unavailable".into()))
    }
}

/// Reading and safely changing local volumes.
pub trait VolumeStore {
    fn list(&self) -> Result<Vec<VolumeSummary>, HostError> {
        Err(HostError::Unsupported("volume inventory is unavailable".into()))
    }
    fn inspect(&self, _name: &str) -> Result<VolumeSummary, HostError> {
        Err(HostError::Unsupported("volume inspection is unavailable".into()))
    }
    fn create(&self, _name: &str) -> Result<VolumeSummary, HostError> {
        Err(HostError::Unsupported("volume creation is unavailable".into()))
    }
    fn remove(&self, _name: &str, _generation: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("volume removal is unavailable".into()))
    }
}

/// Reading and safely changing workspace-local networks.
pub trait NetworkStore {
    fn list(&self) -> Result<Vec<NetworkSummary>, HostError> {
        Err(HostError::Unsupported("network inventory is unavailable".into()))
    }
    fn inspect(&self, _reference: &str) -> Result<NetworkSummary, HostError> {
        Err(HostError::Unsupported("network inspection is unavailable".into()))
    }
    fn create(&self, _name: &str) -> Result<String, HostError> {
        Err(HostError::Unsupported("network creation is unavailable".into()))
    }
    fn remove(&self, _reference: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("network removal is unavailable".into()))
    }
    fn connect(&self, _reference: &str, _container: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("network connection is unavailable".into()))
    }
    fn disconnect(&self, _reference: &str, _container: &str) -> Result<(), HostError> {
        Err(HostError::Unsupported("network disconnection is unavailable".into()))
    }
}

/// The workspace's terminal surface.
pub trait TerminalSurface {
    /// Opens a non-persisted terminal tab running `command` directly in the
    /// immutable container identity. The attachment owns the process and must
    /// kill it when the pane disconnects.
    fn attach_container(&self, _id: &str, _command: &[String]) -> Result<String, HostError> {
        Err(HostError::Unsupported(
            "container terminal attachment is unavailable".into(),
        ))
    }

    /// # Errors
    /// Returns a host failure.
    fn tabs(&self) -> Result<Vec<TabSummary>, HostError>;

    /// Nested tabs/splits and current focus. Implementations that only support
    /// the legacy flat listing must refuse rather than synthesize a tree.
    fn topology(&self) -> Result<TerminalTopology, HostError> {
        Err(HostError::Unsupported("terminal topology is unavailable".into()))
    }

    fn pane_inventory(&self) -> Result<PaneInventory, HostError> {
        Err(HostError::Unsupported("pane discovery is unavailable".into()))
    }

    /// # Errors
    /// Returns a host failure.
    fn open_tab(&self, title: &str) -> Result<String, HostError>;

    /// # Errors
    /// Returns a host failure.
    fn split(&self, slot: &str, division: Division) -> Result<String, HostError>;

    /// # Errors
    /// Returns a host failure.
    fn spawn(&self, slot: &str, command: &[String]) -> Result<(), HostError>;

    /// The text one pane is showing, at most `lines` of it, newest last.
    ///
    /// # Errors
    /// Returns `HostError::Absent` when no pane is open under the slot.
    fn read(&self, slot: &str, lines: usize) -> Result<PaneText, HostError>;

    fn semantics(&self, _slot: &str) -> Result<PaneSemanticTree, HostError> {
        Err(HostError::Unsupported("pane semantics are unavailable".into()))
    }

    fn semantic_action(&self, _slot: &str, _action: &PaneSemanticAction) -> Result<(), HostError> {
        Err(HostError::Unsupported("pane semantic actions are unavailable".into()))
    }

    /// Additional domain authority required by a semantic action. Extension
    /// surfaces default to their explicit semantic-control grant; native panes
    /// override this from their product-owned registry.
    fn semantic_requirement(&self, _slot: &str, _node: u64) -> Result<crate::Capability, HostError> {
        Ok(crate::Capability::PaneSemanticControl)
    }

    /// Writes raw bytes into a terminal pane, without appending a newline.
    fn write(&self, _slot: &str, _contents: &[u8]) -> Result<(), HostError> {
        Err(HostError::Unsupported("terminal input is unavailable".into()))
    }

    /// Requests an exact PTY grid. A later native allocation may supersede it.
    fn resize_grid(&self, _slot: &str, _grid: GridSize) -> Result<(), HostError> {
        Err(HostError::Unsupported("terminal grid control is unavailable".into()))
    }

    /// Closes one pane. Closing the only pane of a tab closes the tab, which is
    /// what closing that pane already does when a person does it.
    ///
    /// # Errors
    /// Returns `HostError::Absent` when no pane is open under the slot.
    fn close(&self, slot: &str) -> Result<(), HostError>;

    /// Moves keyboard focus to one pane.
    ///
    /// # Errors
    /// Returns `HostError::Absent` when no pane is open under the slot.
    fn focus(&self, slot: &str) -> Result<(), HostError>;

    /// Sets how much of its split one pane takes, as a fraction in `0.05..=0.95`.
    ///
    /// # Errors
    /// Returns `HostError::Absent` when no pane is open under the slot, and
    /// `HostError::Conflict` when the pane is not inside a split.
    fn ratio(&self, slot: &str, ratio: f64) -> Result<(), HostError>;

    /// Divides one pane and gives the new pane to the caller to draw into,
    /// rather than starting a shell in it.
    ///
    /// # Errors
    /// Returns `HostError::Absent` when no pane is open under the slot, and
    /// `HostError::Conflict` when the caller is not an extension that can draw.
    fn surface(&self, slot: &str, division: Division) -> Result<String, HostError>;
}

/// The workspaces this host knows about.
pub trait WorkspaceInventory {
    /// # Errors
    /// Returns a host failure when the configured workspaces cannot be read.
    fn workspaces(&self) -> Result<Vec<WorkspaceState>, HostError>;
}

/// Creating, configuring, and controlling workspace execution domains.
pub trait WorkspaceControl {
    /// Current lifecycle sequence for this host process.
    fn lifecycle_revision(&self) -> u64 {
        0
    }
    /// Successful mutations after `revision`, oldest first and bounded by the host.
    fn lifecycle_since(&self, _revision: u64) -> Result<Vec<crate::WorkspaceLifecycleChange>, HostError> {
        Ok(Vec::new())
    }
    fn inspect(&self, _name: &str) -> Result<WorkspaceConfiguration, HostError> {
        Err(workspace_control_unavailable())
    }
    fn create(&self, _configuration: &WorkspaceConfiguration) -> Result<WorkspaceConfiguration, HostError> {
        Err(workspace_control_unavailable())
    }
    fn adopt(&self, _configuration: &WorkspaceConfiguration) -> Result<WorkspaceConfiguration, HostError> {
        Err(workspace_control_unavailable())
    }
    fn update(
        &self,
        _name: &str,
        _generation: &str,
        _configuration: &WorkspaceConfiguration,
    ) -> Result<WorkspaceConfiguration, HostError> {
        Err(workspace_control_unavailable())
    }
    fn delete(&self, _name: &str, _generation: &str) -> Result<(), HostError> {
        Err(workspace_control_unavailable())
    }
    fn start(&self, _name: &str) -> Result<(), HostError> {
        Err(workspace_control_unavailable())
    }
    fn stop(&self, _name: &str) -> Result<(), HostError> {
        Err(workspace_control_unavailable())
    }
    fn restart(&self, _name: &str) -> Result<(), HostError> {
        Err(workspace_control_unavailable())
    }
}

fn workspace_control_unavailable() -> HostError {
    HostError::Failed("workspace control is unavailable from this host".into())
}

/// Files beneath the extension's declared roots.
pub trait WorkspaceFiles {
    /// # Errors
    /// Returns a host failure.
    fn list(&self, path: &RelativePath) -> Result<Vec<Entry>, HostError>;

    /// # Errors
    /// Returns a host failure.
    fn read(&self, path: &RelativePath) -> Result<Vec<u8>, HostError>;

    /// Reads metadata for exactly one confined workspace-relative path.
    fn stat(&self, _path: &RelativePath) -> Result<Entry, HostError> {
        Err(HostError::Unsupported("filesystem metadata is unavailable".into()))
    }

    /// # Errors
    /// Returns a host failure.
    fn write(&self, path: &RelativePath, contents: &[u8]) -> Result<(), HostError>;

    fn mkdir(&self, _path: &RelativePath) -> Result<(), HostError> {
        Err(HostError::Unsupported("directory creation is unavailable".into()))
    }

    fn rename(&self, _from: &RelativePath, _to: &RelativePath) -> Result<(), HostError> {
        Err(HostError::Unsupported("filesystem rename is unavailable".into()))
    }

    fn remove(&self, _path: &RelativePath) -> Result<(), HostError> {
        Err(HostError::Unsupported("filesystem removal is unavailable".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_pane_text, pane_lines, Division, LayoutNode, Occupant, PaneSummary, PaneText, PANE_LINES,
        PANE_TEXT_BYTES,
    };

    #[test]
    fn a_pane_read_is_bounded_however_it_is_asked_for() {
        assert_eq!(pane_lines(None), PANE_LINES, "an unstated tail is the allowance");
        assert_eq!(pane_lines(Some(10)), 10);
        assert_eq!(pane_lines(Some(usize::MAX)), PANE_LINES, "a huge tail is cut to it");
        assert_eq!(pane_lines(Some(0)), 1, "a pane read answers with something");
    }

    #[test]
    fn terminal_text_retains_only_newest_complete_lines_within_the_wire_budget() {
        let text = PaneText {
            slot: "pane".into(),
            lines: vec![
                "old".repeat(PANE_TEXT_BYTES / 3),
                "middle".repeat(PANE_TEXT_BYTES / 6),
                "new".into(),
            ],
            cursor_column: 4,
            cursor_row: 2,
            truncated: false,
        };
        let bounded = bounded_pane_text(text);
        assert!(bounded.truncated);
        assert_eq!(bounded.lines.last().map(String::as_str), Some("new"));
        assert_eq!((bounded.cursor_column, bounded.cursor_row), (4, 2));
        assert!(bounded.lines.iter().map(|line| line.len() + 1).sum::<usize>() <= PANE_TEXT_BYTES);
    }

    #[test]
    fn legacy_output_decodes_without_claiming_eof_or_per_stream_completeness() {
        let output: super::ContainerOutput = serde_json::from_value(serde_json::json!({
            "stdout": [], "stderr": [], "truncated": false
        }))
        .expect("legacy output");
        assert!(!output.eof);
        assert!(!output.stdout_truncated);
        assert!(!output.stderr_truncated);
    }

    #[test]
    fn legacy_process_rows_decode_as_an_incomplete_snapshot_scoped_view() {
        let processes: super::ProcessList = serde_json::from_value(serde_json::json!({
            "titles": ["PID"], "processes": [["1"]]
        }))
        .expect("legacy process rows");
        assert_eq!(processes.scope, super::ProcessScope::Initial);
        assert_eq!(processes.pid_identity, super::ProcessPidIdentity::Snapshot);
        assert_eq!(processes.observed_at_ms, 0);
        assert!(!processes.truncated);
    }

    #[test]
    fn nested_layout_has_a_stable_tagged_wire_shape() {
        let pane = || LayoutNode::Pane {
            pane: PaneSummary {
                slot: "s1".into(),
                working_directory: None,
                command: None,
                occupant: Occupant::Terminal,
                provider: None,
            },
            grid: None,
            focused: true,
        };
        let layout = LayoutNode::Split {
            division: Division::Beside,
            ratio_per_mille: 500,
            first: Box::new(pane()),
            second: Box::new(pane()),
        };
        let value = serde_json::to_value(layout).expect("layout");
        assert_eq!(value["kind"], "split");
        assert_eq!(value["division"], "beside");
        assert_eq!(value["first"]["kind"], "pane");
    }
}
