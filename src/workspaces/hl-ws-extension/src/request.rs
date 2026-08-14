//! The calls an extension may make and the answers it receives.
//!
//! Every call names the capability it needs, in one table, so the surface an
//! extension can reach is readable in a single place rather than inferred from
//! scattered dispatch arms.

use crate::capability::Capability;
use crate::path::RelativePath;
use crate::port::{ContainerSummary, Division, Entry, HostError, ImageSummary, TabSummary};

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
    ContainerList,
    ContainerInspect { id: String },
    ContainerCreate { image: String, name: String },
    ContainerStart { id: String },
    ContainerStop { id: String },
    ContainerRemove { id: String },
    ImageList,
    ImagePull { reference: String },
    TerminalTabs,
    TerminalOpenTab { title: String },
    TerminalSplit { slot: String, division: Division },
    TerminalSpawn { slot: String, command: Vec<String> },
    FilesystemList { path: RelativePath },
    FilesystemRead { path: RelativePath },
    FilesystemWrite { path: RelativePath, contents: Vec<u8> },
    InterfaceOpenTab { title: String },
    InterfaceRender { frame: hl_gui::Frame },
    SourceResize { mutation: hl_gui::SourceMutation },
    EventSubscribe { topic: Topic },
    EventUnsubscribe { topic: Topic },
}

impl Request {
    /// The capability this call requires. Enforcement reads this table, so a
    /// new call cannot reach a service without appearing here.
    #[must_use]
    pub const fn capability(&self) -> Capability {
        match self {
            Self::WorkspaceInfo => Capability::WorkspaceRead,
            Self::ContainerList | Self::ContainerInspect { .. } => Capability::ContainerRead,
            Self::ContainerCreate { .. }
            | Self::ContainerStart { .. }
            | Self::ContainerStop { .. }
            | Self::ContainerRemove { .. } => Capability::ContainerControl,
            Self::ImageList => Capability::ImageRead,
            Self::ImagePull { .. } => Capability::ImageWrite,
            Self::TerminalTabs => Capability::TerminalRead,
            Self::TerminalOpenTab { .. } | Self::TerminalSplit { .. } | Self::TerminalSpawn { .. } => {
                Capability::TerminalControl
            }
            Self::FilesystemList { .. } | Self::FilesystemRead { .. } => Capability::FilesystemRead,
            Self::FilesystemWrite { .. } => Capability::FilesystemWrite,
            Self::InterfaceOpenTab { .. } | Self::InterfaceRender { .. } | Self::SourceResize { .. } => {
                Capability::Interface
            }
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
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::Containers,
        Self::Images,
        Self::Volumes,
        Self::Networks,
        Self::Terminal,
        Self::Extensions,
    ];
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
    Containers(Vec<ContainerSummary>),
    Container(ContainerSummary),
    Images(Vec<ImageSummary>),
    Image(ImageSummary),
    Tabs(Vec<TabSummary>),
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
        }
    }
}

impl From<crate::authority::Denial> for Failure {
    fn from(denial: crate::authority::Denial) -> Self {
        Self::Denied {
            capability: denial.capability.as_str().into(),
            detail: denial.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Request, Topic};
    use crate::capability::Capability;
    use crate::path::RelativePath;

    #[test]
    fn reading_and_writing_calls_require_different_capabilities() {
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
