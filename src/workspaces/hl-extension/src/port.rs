//! The narrow traits this crate declares and something else implements.
//!
//! Each is single-purpose. There is deliberately no omnibus host trait: a
//! dispatcher should be able to reach exactly the service it was granted and
//! nothing adjacent to it.

use crate::path::RelativePath;

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
}

impl std::fmt::Display for HostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent(detail) => write!(formatter, "not found: {detail}"),
            Self::Conflict(detail) => write!(formatter, "conflict: {detail}"),
            Self::Failed(detail) => write!(formatter, "failed: {detail}"),
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
