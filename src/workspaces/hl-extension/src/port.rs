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
}

/// Bounded captured output from a container's initial process.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ContainerOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// At least one stream was shortened to the protocol limit.
    pub truncated: bool,
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

/// An image as an extension sees it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ImageSummary {
    pub id: String,
    pub reference: String,
    pub size: u64,
    pub created: i64,
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
    /// Whether older lines exist that this answer does not carry.
    pub truncated: bool,
}

/// The greatest number of lines one pane read may answer with.
///
/// A pane's scrollback is as large as its shell made it, and an answer is built
/// in memory on the drawing thread before it is sent. The cap is what stops a
/// single call from making the host allocate whatever a runaway command printed.
pub const PANE_LINES: usize = 2000;

/// How many lines a pane read actually returns.
///
/// An unstated tail is the whole allowance rather than everything there is, so
/// a caller that names no bound still cannot ask for an unbounded read.
#[must_use]
pub fn pane_lines(requested: Option<usize>) -> usize {
    requested.unwrap_or(PANE_LINES).clamp(1, PANE_LINES)
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
}

/// Changing container state. Granting this is granting code execution inside
/// the workspace, which the consent prompt must say plainly.
pub trait ContainerControl {
    /// # Errors
    /// Returns a host failure.
    fn create(&self, image: &str, name: &str) -> Result<String, HostError>;

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
}

/// The workspace's terminal surface.
pub trait TerminalSurface {
    /// # Errors
    /// Returns a host failure.
    fn tabs(&self) -> Result<Vec<TabSummary>, HostError>;

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
    fn inspect(&self, _name: &str) -> Result<WorkspaceConfiguration, HostError> {
        Err(workspace_control_unavailable())
    }
    fn create(&self, _configuration: &WorkspaceConfiguration) -> Result<WorkspaceConfiguration, HostError> {
        Err(workspace_control_unavailable())
    }
    fn update(
        &self,
        _name: &str,
        _configuration: &WorkspaceConfiguration,
    ) -> Result<WorkspaceConfiguration, HostError> {
        Err(workspace_control_unavailable())
    }
    fn delete(&self, _name: &str) -> Result<(), HostError> {
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

    /// # Errors
    /// Returns a host failure.
    fn write(&self, path: &RelativePath, contents: &[u8]) -> Result<(), HostError>;
}

#[cfg(test)]
mod tests {
    use super::{PANE_LINES, pane_lines};

    #[test]
    fn a_pane_read_is_bounded_however_it_is_asked_for() {
        assert_eq!(pane_lines(None), PANE_LINES, "an unstated tail is the allowance");
        assert_eq!(pane_lines(Some(10)), 10);
        assert_eq!(pane_lines(Some(usize::MAX)), PANE_LINES, "a huge tail is cut to it");
        assert_eq!(pane_lines(Some(0)), 1, "a pane read answers with something");
    }
}
