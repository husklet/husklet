//! The calls an extension may make and the answers it receives.
//!
//! Every call names the capability it needs, in one table, so the surface an
//! extension can reach is readable in a single place rather than inferred from
//! scattered dispatch arms.

use hl_rpc::{CapabilityKey, RelativePath};

use crate::capability::Capability;
use crate::port::{
    ContainerOutput, ContainerSummary, Division, Entry, ExecutionSummary, HostError, ImageSummary, PaneText,
    NetworkSummary, ProcessList, TabSummary, TerminalTopology, VolumeSummary, WorkspaceConfiguration, WorkspaceState,
};

/// A call from an extension.
///
/// Adjacently tagged rather than internally tagged: an internal tag silently
/// ignores unmodelled arguments, and a call carrying an argument this host does
/// not implement must be refused, not quietly executed without it.
///
/// Not `Eq`: an interface description carries measurements, and a measurement
/// has no total equality.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "call", content = "with", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    WorkspaceInfo,
    WorkspaceList,
    WorkspaceInspect {
        name: String,
    },
    WorkspaceCreate {
        configuration: WorkspaceConfiguration,
    },
    WorkspaceUpdate {
        name: String,
        configuration: WorkspaceConfiguration,
    },
    WorkspaceDelete {
        name: String,
    },
    WorkspaceStart {
        name: String,
    },
    WorkspaceStop {
        name: String,
    },
    WorkspaceRestart {
        name: String,
    },
    ContainerList,
    ContainerInspect {
        id: String,
    },
    ContainerProcesses {
        id: String,
    },
    ContainerLogs {
        id: String,
        stdout: bool,
        stderr: bool,
    },
    ExecutionInspect {
        id: String,
    },
    ContainerCreate {
        image: String,
        name: String,
    },
    ContainerStart {
        id: String,
    },
    ContainerStop {
        id: String,
    },
    ContainerRemove {
        id: String,
    },
    ContainerPause {
        id: String,
    },
    ContainerUnpause {
        id: String,
    },
    ContainerRestart {
        id: String,
    },
    ContainerKill {
        id: String,
        signal: String,
    },
    ContainerExec {
        id: String,
        command: Vec<String>,
        user: Option<String>,
        working_directory: Option<String>,
    },
    ImageList,
    ImagePull {
        reference: String,
    },
    VolumeList,
    VolumeInspect { name: String },
    VolumeCreate { name: String },
    VolumeRemove { name: String },
    NetworkList,
    NetworkInspect { reference: String },
    NetworkCreate { name: String },
    NetworkRemove { reference: String },
    NetworkConnect { reference: String, container: String },
    NetworkDisconnect { reference: String, container: String },
    TerminalTabs,
    TerminalTopology,
    TerminalOpenTab {
        title: String,
    },
    TerminalSplit {
        slot: String,
        division: Division,
    },
    TerminalSpawn {
        slot: String,
        command: Vec<String>,
    },
    TerminalReadPane {
        slot: String,
        lines: Option<usize>,
    },
    TerminalWritePane {
        slot: String,
        contents: Vec<u8>,
    },
    TerminalResizeGrid {
        slot: String,
        columns: u16,
        rows: u16,
    },
    TerminalClosePane {
        slot: String,
    },
    TerminalFocusPane {
        slot: String,
    },
    TerminalRatio {
        slot: String,
        ratio: f64,
    },
    FilesystemList {
        path: RelativePath,
    },
    FilesystemRead {
        path: RelativePath,
    },
    FilesystemWrite {
        path: RelativePath,
        contents: Vec<u8>,
    },
    InterfaceOpenTab {
        title: String,
    },
    InterfaceSplit {
        slot: String,
        division: Division,
    },
    InterfaceRender {
        frame: hl_gui::Frame,
    },
    SourceResize {
        mutation: hl_gui::SourceMutation,
    },
    EventSubscribe {
        topic: Topic,
    },
    EventUnsubscribe {
        topic: Topic,
    },
}

impl Request {
    /// The capability this call requires. Enforcement reads this table, so a
    /// new call cannot reach a service without appearing here.
    #[must_use]
    pub const fn capability(&self) -> Capability {
        match self {
            Self::WorkspaceInfo | Self::WorkspaceList | Self::WorkspaceInspect { .. } => Capability::WorkspaceRead,
            Self::WorkspaceCreate { .. }
            | Self::WorkspaceUpdate { .. }
            | Self::WorkspaceDelete { .. }
            | Self::WorkspaceStart { .. }
            | Self::WorkspaceStop { .. }
            | Self::WorkspaceRestart { .. } => Capability::WorkspaceControl,
            Self::ContainerList
            | Self::ContainerInspect { .. }
            | Self::ContainerProcesses { .. }
            | Self::ContainerLogs { .. }
            | Self::ExecutionInspect { .. } => Capability::ContainerRead,
            Self::ContainerCreate { .. }
            | Self::ContainerStart { .. }
            | Self::ContainerStop { .. }
            | Self::ContainerRemove { .. }
            | Self::ContainerPause { .. }
            | Self::ContainerUnpause { .. }
            | Self::ContainerRestart { .. }
            | Self::ContainerKill { .. }
            | Self::ContainerExec { .. } => Capability::ContainerControl,
            Self::ImageList => Capability::ImageRead,
            Self::ImagePull { .. } => Capability::ImageWrite,
            Self::VolumeList | Self::VolumeInspect { .. } => Capability::VolumeRead,
            Self::VolumeCreate { .. } | Self::VolumeRemove { .. } => Capability::VolumeWrite,
            Self::NetworkList | Self::NetworkInspect { .. } => Capability::NetworkRead,
            Self::NetworkCreate { .. }
            | Self::NetworkRemove { .. }
            | Self::NetworkConnect { .. }
            | Self::NetworkDisconnect { .. } => Capability::NetworkWrite,
            Self::TerminalTabs | Self::TerminalTopology => Capability::TerminalRead,
            Self::TerminalOpenTab { .. }
            | Self::TerminalSplit { .. }
            | Self::TerminalSpawn { .. }
            | Self::TerminalWritePane { .. }
            | Self::TerminalResizeGrid { .. }
            | Self::TerminalClosePane { .. }
            | Self::TerminalFocusPane { .. }
            | Self::TerminalRatio { .. } => Capability::TerminalControl,
            // Reading what a shell printed is what `TerminalOutput` was separated
            // out for: listing panes says a pane exists, this says what was typed
            // into it and what came back.
            Self::TerminalReadPane { .. } => Capability::TerminalOutput,
            Self::FilesystemList { .. } | Self::FilesystemRead { .. } => Capability::FilesystemRead,
            Self::FilesystemWrite { .. } => Capability::FilesystemWrite,
            Self::InterfaceOpenTab { .. }
            | Self::InterfaceSplit { .. }
            | Self::InterfaceRender { .. }
            | Self::SourceResize { .. } => Capability::Interface,
            Self::EventSubscribe { topic } | Self::EventUnsubscribe { topic } => topic.capability(),
        }
    }

    /// The path this call reaches, when it names one. A call returning a path
    /// here is confined to the extension's declared roots.
    #[must_use]
    pub const fn path(&self) -> Option<&RelativePath> {
        match self {
            Self::FilesystemList { path } | Self::FilesystemRead { path } | Self::FilesystemWrite { path, .. } => {
                Some(path)
            }
            _ => None,
        }
    }
}

/// A stream of host state an extension can follow.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Topic {
    Containers,
    Images,
    Volumes,
    Networks,
    Terminal,
    Extensions,
    WorkspaceEvents,
}

impl Topic {
    /// The capability required to follow this topic. Checked when subscribing
    /// and again on every emission, so a revoked grant stops the stream.
    #[must_use]
    pub const fn capability(self) -> Capability {
        match self {
            Self::Containers => Capability::ContainerRead,
            Self::Images => Capability::ImageRead,
            Self::Volumes => Capability::VolumeRead,
            Self::Networks => Capability::NetworkRead,
            Self::Terminal => Capability::TerminalRead,
            Self::Extensions => Capability::WorkspaceRead,
            Self::WorkspaceEvents => Capability::WorkspaceEvents,
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::Containers,
        Self::Images,
        Self::Volumes,
        Self::Networks,
        Self::Terminal,
        Self::Extensions,
        Self::WorkspaceEvents,
    ];
}

impl hl_rpc::Topic for Topic {
    fn requirement(&self) -> CapabilityKey {
        use hl_rpc::Capability as _;

        self.capability().key()
    }
}

/// Describes the workspace itself. Deliberately carries no secret: an
/// extension is told what the workspace is, never how to authenticate as it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub architecture: String,
    pub image: String,
}

/// The answer to a call.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "reply", content = "with", rename_all = "snake_case")]
pub enum Reply {
    Workspace(WorkspaceInfo),
    WorkspaceConfiguration(WorkspaceConfiguration),
    Workspaces(Vec<WorkspaceState>),
    Containers(Vec<ContainerSummary>),
    Container(ContainerSummary),
    Processes(ProcessList),
    Logs(ContainerOutput),
    Execution(ExecutionSummary),
    Images(Vec<ImageSummary>),
    Image(ImageSummary),
    Volumes(Vec<VolumeSummary>),
    Volume(VolumeSummary),
    Networks(Vec<NetworkSummary>),
    Network(NetworkSummary),
    Tabs(Vec<TabSummary>),
    Topology(TerminalTopology),
    Text(PaneText),
    Entries(Vec<Entry>),
    Contents(Vec<u8>),
    Identity(String),
    Done,
}

/// Why a call failed.
///
/// A refusal is always reported as a refusal. An extension that believes it can
/// list containers and receives an empty list will misbehave far worse than one
/// told plainly that it may not.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum Failure {
    Denied { capability: String, detail: String },
    Absent { detail: String },
    Conflict { detail: String },
    Failed { detail: String },
    Unsupported { call: String },
}

impl From<HostError> for Failure {
    fn from(error: HostError) -> Self {
        match error {
            HostError::Absent(detail) => Self::Absent { detail },
            HostError::Conflict(detail) => Self::Conflict { detail },
            HostError::Failed(detail) => Self::Failed { detail },
            HostError::Unsupported(call) => Self::Unsupported { call },
        }
    }
}

impl From<hl_rpc::Denial> for Failure {
    fn from(denial: hl_rpc::Denial) -> Self {
        Self::Denied {
            capability: denial.capability.name().into(),
            detail: denial.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Request, Topic};
    use crate::capability::Capability;
    use hl_rpc::RelativePath;

    #[test]
    fn reading_and_writing_calls_require_different_capabilities() {
        assert_eq!(
            Request::EventSubscribe { topic: Topic::WorkspaceEvents }.capability(),
            Capability::WorkspaceEvents
        );
        assert_eq!(Request::ContainerList.capability(), Capability::ContainerRead);
        assert_eq!(
            Request::ContainerStop { id: "a".into() }.capability(),
            Capability::ContainerControl
        );
        assert_eq!(Request::ImageList.capability(), Capability::ImageRead);
        assert_eq!(
            Request::ImagePull {
                reference: "alpine".into()
            }
            .capability(),
            Capability::ImageWrite
        );
    }

    #[test]
    fn reading_a_panes_text_is_gated_apart_from_listing_panes() {
        assert_eq!(Request::TerminalTabs.capability(), Capability::TerminalRead);
        assert_eq!(
            Request::TerminalReadPane {
                slot: "1".into(),
                lines: None,
            }
            .capability(),
            Capability::TerminalOutput
        );
        for request in [
            Request::TerminalClosePane { slot: "1".into() },
            Request::TerminalFocusPane { slot: "1".into() },
            Request::TerminalRatio {
                slot: "1".into(),
                ratio: 0.5,
            },
        ] {
            assert_eq!(request.capability(), Capability::TerminalControl, "{request:?}");
        }
        assert_eq!(
            Request::InterfaceSplit {
                slot: "1".into(),
                division: crate::port::Division::Beside,
            }
            .capability(),
            Capability::Interface
        );
        assert_eq!(Request::WorkspaceList.capability(), Capability::WorkspaceRead);
        assert_eq!(
            Request::WorkspaceDelete { name: "other".into() }.capability(),
            Capability::WorkspaceControl
        );
    }

    #[test]
    fn every_filesystem_call_exposes_its_path_for_confinement() {
        let path = RelativePath::new("logs/app.log").expect("path");
        for request in [
            Request::FilesystemList { path: path.clone() },
            Request::FilesystemRead { path: path.clone() },
            Request::FilesystemWrite {
                path: path.clone(),
                contents: Vec::new(),
            },
        ] {
            assert_eq!(request.path(), Some(&path), "{request:?} must be confined");
        }
        assert_eq!(Request::ContainerList.path(), None);
    }

    #[test]
    fn every_topic_names_the_capability_that_gates_it() {
        for topic in Topic::ALL {
            let request = Request::EventSubscribe { topic: *topic };
            assert_eq!(request.capability(), topic.capability());
        }
        assert_eq!(Topic::Containers.capability(), Capability::ContainerRead);
        assert_eq!(Topic::Terminal.capability(), Capability::TerminalRead);
    }

    #[test]
    fn an_unknown_call_is_refused_rather_than_guessed_at() {
        let refused: Result<Request, _> = serde_json::from_str("{\"call\":\"containers_destroy_everything\"}");
        assert!(refused.is_err(), "an unknown call must not be accepted");

        let extra: Result<Request, _> = serde_json::from_str("{\"call\":\"container_list\",\"force\":true}");
        assert!(extra.is_err(), "an unmodelled argument must not be ignored");

        let unmodelled_argument: Result<Request, _> =
            serde_json::from_str("{\"call\":\"container_stop\",\"with\":{\"id\":\"c1\",\"force\":true}}");
        assert!(
            unmodelled_argument.is_err(),
            "a call asking for behaviour this host does not implement must be refused, not run without it"
        );

        let accepted: Request =
            serde_json::from_str("{\"call\":\"container_stop\",\"with\":{\"id\":\"c1\"}}").expect("valid");
        assert_eq!(accepted, Request::ContainerStop { id: "c1".into() });
    }
}
