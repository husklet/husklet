//! The calls an extension may make and the answers it receives.
//!
//! Every call names the capability it needs, in one table, so the surface an
//! extension can reach is readable in a single place rather than inferred from
//! scattered dispatch arms.

use hl_rpc::{CapabilityKey, RelativePath};

use crate::capability::Capability;
use crate::port::{
    ContainerOutput, ContainerSummary, Division, Entry, ExecutionList, ExecutionSummary, HostError, ImageDetails,
    ImagePruneResult, ImagePullJob, ImagePullStatus, ImageSummary, NetworkSummary, PaneInventory, PaneText,
    ProcessList, TabSummary, TerminalTopology, VolumeSummary, WorkspaceConfiguration, WorkspaceState,
};

/// An exec environment value. Its wire representation is a string, while
/// diagnostics deliberately never reveal credential material.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct ExecEnvironmentValue(String);

impl ExecEnvironmentValue {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ExecEnvironmentValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

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
        generation: String,
        configuration_revision: String,
        configuration: WorkspaceConfiguration,
    },
    WorkspaceEnvironmentPatch {
        name: String,
        generation: String,
        configuration_revision: String,
        patch: crate::port::WorkspaceEnvironmentPatch,
    },
    WorkspaceDelete {
        name: String,
        generation: String,
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
    ExtensionList,
    ExtensionCatalogue,
    ExtensionInspect {
        name: String,
    },
    ExtensionEnable {
        name: String,
        image_digest: String,
    },
    ExtensionDisable {
        name: String,
        image_digest: String,
    },
    ExtensionRetry {
        name: String,
        image_digest: String,
    },
    ExtensionRemove {
        name: String,
        image_digest: String,
    },
    ExtensionAcquisitionStart {
        reference: String,
    },
    ExtensionAcquisitionStatus {
        job: String,
    },
    ExtensionAcquisitionCancel {
        job: String,
        revision: u64,
    },
    NotificationPublish {
        notification: crate::port::Notification,
    },
    ExtensionInstall {
        job: String,
        revision: u64,
        image_digest: String,
        granted: crate::Grant,
        containers: crate::ContainerGrant,
        images: crate::ImageGrant,
        networks: crate::NetworkGrant,
        volumes: crate::VolumeGrant,
        filesystem: crate::FilesystemGrant,
        workspace_environment: crate::WorkspaceEnvironmentGrant,
    },
    ExtensionUpdate {
        job: String,
        revision: u64,
        image_digest: String,
        granted: crate::Grant,
        containers: crate::ContainerGrant,
        images: crate::ImageGrant,
        networks: crate::NetworkGrant,
        volumes: crate::VolumeGrant,
        filesystem: crate::FilesystemGrant,
        workspace_environment: crate::WorkspaceEnvironmentGrant,
    },
    ContainerList,
    ContainerInspect {
        id: String,
    },
    ContainerInspectObserved {
        id: String,
        generation: u64,
    },
    ContainerProcesses {
        id: String,
        snapshot: Option<String>,
        after: u32,
        limit: u16,
    },
    ContainerLogs {
        id: String,
        stdout: bool,
        stderr: bool,
    },
    ExecutionInspect {
        id: String,
    },
    ExecutionList,
    ExecutionLogs {
        id: String,
        stdout: bool,
        stderr: bool,
    },
    ExecutionOutput {
        id: String,
        after: u64,
        limit: u16,
    },
    ExecutionWait {
        id: String,
        timeout_ms: u32,
    },
    ExecutionKill {
        id: String,
        signal: String,
    },
    ExecutionCancel {
        id: String,
        signal: String,
        timeout_ms: u32,
    },
    ExecutionRemove {
        id: String,
    },
    ExecutionWrite {
        id: String,
        contents: Vec<u8>,
    },
    ExecutionCloseInput {
        id: String,
    },
    ContainerCreate {
        spec: crate::port::ContainerCreateSpec,
    },
    ContainerStart {
        id: String,
        generation: u64,
    },
    ContainerStop {
        id: String,
        generation: u64,
    },
    ContainerRemove {
        id: String,
        generation: u64,
    },
    ContainerPause {
        id: String,
        generation: u64,
    },
    ContainerUnpause {
        id: String,
        generation: u64,
    },
    ContainerRestart {
        id: String,
        generation: u64,
    },
    ContainerRename {
        id: String,
        generation: u64,
        name: String,
    },
    ContainerKill {
        id: String,
        generation: u64,
        signal: String,
    },
    ContainerExec {
        id: String,
        generation: u64,
        command: Vec<String>,
        environment: Vec<(String, ExecEnvironmentValue)>,
        user: Option<String>,
        working_directory: Option<String>,
        #[serde(default)]
        stdin: bool,
    },
    /// Executes with named environment values resolved from this extension's
    /// host-protected credential store. Requires both container execution and
    /// credential read authority.
    ContainerExecCredential {
        id: String,
        generation: u64,
        command: Vec<String>,
        environment: Vec<(String, ExecEnvironmentValue)>,
        credentials: Vec<(String, String)>,
        user: Option<String>,
        working_directory: Option<String>,
        #[serde(default)]
        stdin: bool,
    },
    ContainerAttachTerminal {
        id: String,
        command: Vec<String>,
    },
    ImageList,
    ImagePullStart {
        reference: String,
    },
    ImagePullStatus {
        job: String,
    },
    ImagePullCancel {
        job: String,
    },
    ImageInspect {
        reference: String,
    },
    ImageRemove {
        reference: String,
    },
    ImagePrune,
    VolumeList,
    VolumeInspect {
        name: String,
    },
    VolumeCreate {
        name: String,
    },
    VolumeRemove {
        name: String,
        generation: String,
    },
    NetworkList,
    NetworkInspect {
        reference: String,
    },
    NetworkCreate {
        name: String,
    },
    NetworkRemove {
        reference: String,
    },
    NetworkConnect {
        reference: String,
        container: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        aliases: Vec<String>,
    },
    NetworkDisconnect {
        reference: String,
        container: String,
    },
    TerminalTabs,
    TerminalTopology,
    PaneList,
    TerminalOpenTab {
        title: String,
    },
    TerminalPinTab {
        tab: String,
        pinned: bool,
    },
    TerminalFocusTab {
        tab: String,
    },
    TerminalSplit {
        slot: String,
        division: Division,
    },
    /// Splits only the exact pane occupant snapshot the caller observed.
    TerminalSplitObserved {
        slot: String,
        generation: u64,
        revision: u64,
        division: Division,
    },
    TerminalSpawn {
        slot: String,
        command: Vec<String>,
    },
    /// Runs a command only in the exact terminal snapshot the caller observed.
    TerminalSpawnObserved {
        slot: String,
        generation: u64,
        revision: u64,
        command: Vec<String>,
    },
    /// Starts a supervised workspace command only if this exact terminal
    /// occupant snapshot still exists. Completion never depends on a prompt.
    TerminalCommandStart {
        slot: String,
        generation: u64,
        revision: u64,
        command: Vec<String>,
        working_directory: Option<String>,
        #[serde(default)]
        stdin: bool,
    },
    TerminalCommandInspect {
        id: String,
        owner: String,
        slot: String,
        generation: u64,
        revision: u64,
    },
    TerminalCommandOutput {
        id: String,
        owner: String,
        slot: String,
        generation: u64,
        revision: u64,
        after: u64,
        limit: u16,
    },
    TerminalCommandWait {
        id: String,
        owner: String,
        slot: String,
        generation: u64,
        revision: u64,
        timeout_ms: u32,
    },
    TerminalCommandCancel {
        id: String,
        owner: String,
        slot: String,
        generation: u64,
        revision: u64,
        signal: String,
        timeout_ms: u32,
    },
    TerminalCommandWrite {
        id: String,
        owner: String,
        slot: String,
        generation: u64,
        revision: u64,
        contents: Vec<u8>,
    },
    TerminalCommandCloseInput {
        id: String,
        owner: String,
        slot: String,
        generation: u64,
        revision: u64,
    },
    TerminalReadPane {
        slot: String,
        lines: Option<usize>,
    },
    PaneSemanticRead {
        slot: String,
    },
    PaneSemanticAction {
        slot: String,
        action: crate::port::PaneSemanticAction,
    },
    TerminalWritePane {
        slot: String,
        generation: u64,
        revision: u64,
        contents: Vec<u8>,
    },
    TerminalResizeGrid {
        slot: String,
        columns: u16,
        rows: u16,
    },
    TerminalResizeGridObserved {
        slot: String,
        generation: u64,
        revision: u64,
        columns: u16,
        rows: u16,
    },
    TerminalClosePane {
        slot: String,
    },
    /// Closes only the exact pane occupant snapshot the caller observed.
    TerminalClosePaneObserved {
        slot: String,
        generation: u64,
        revision: u64,
    },
    TerminalFocusPane {
        slot: String,
    },
    TerminalFocusPaneObserved {
        slot: String,
        generation: u64,
        revision: u64,
    },
    TerminalRetitlePane {
        slot: String,
        title: String,
    },
    TerminalRetitlePaneObserved {
        slot: String,
        generation: u64,
        revision: u64,
        title: String,
    },
    TerminalRatio {
        slot: String,
        ratio: f64,
    },
    /// Resizes only the exact pane snapshot the caller observed.
    TerminalRatioObserved {
        slot: String,
        generation: u64,
        revision: u64,
        ratio: f64,
    },
    TerminalSwitchOccupant {
        slot: String,
        generation: u64,
        target: crate::port::PaneOccupantTarget,
    },
    /// Switches only the exact pane occupant snapshot the caller observed.
    TerminalSwitchOccupantObserved {
        slot: String,
        generation: u64,
        revision: u64,
        target: crate::port::PaneOccupantTarget,
    },
    FilesystemInventory,
    FilesystemChanges {
        observed: String,
        after: u64,
        limit: u16,
    },
    FilesystemList {
        path: RelativePath,
    },
    FilesystemListPage {
        path: RelativePath,
        after: Option<RelativePath>,
        observed: Option<String>,
        limit: usize,
    },
    FilesystemRead {
        path: RelativePath,
    },
    FilesystemReadRange {
        path: RelativePath,
        offset: u64,
        limit: usize,
        observed: Option<String>,
    },
    /// Reads several confined file ranges in one bounded host round trip.
    FilesystemReadRanges {
        ranges: Vec<crate::port::FileRangeRequest>,
    },
    FilesystemStat {
        path: RelativePath,
    },
    FilesystemWrite {
        path: RelativePath,
        contents: Vec<u8>,
    },
    /// Replaces only the exact file identity the caller previously inspected.
    FilesystemWriteObserved {
        path: RelativePath,
        observed: String,
        contents: Vec<u8>,
    },
    FilesystemCreateObserved {
        path: RelativePath,
        contents: Vec<u8>,
    },
    FilesystemMkdir {
        path: RelativePath,
    },
    FilesystemRename {
        from: RelativePath,
        to: RelativePath,
    },
    FilesystemRenameObserved {
        from: RelativePath,
        to: RelativePath,
        observed: String,
    },
    FilesystemRemove {
        path: RelativePath,
    },
    FilesystemRemoveObserved {
        path: RelativePath,
        observed: String,
    },
    StateRead,
    StateWrite {
        observed: String,
        contents: Vec<u8>,
    },
    StateClear {
        observed: String,
    },
    PreferenceRead,
    PreferenceSet {
        observed: u64,
        key: String,
        value: crate::port::PreferenceValue,
    },
    PreferenceRemove {
        observed: u64,
        key: String,
    },
    CredentialRead {
        key: String,
    },
    CredentialSet {
        observed: u64,
        key: String,
        value: Vec<u8>,
    },
    CredentialRemove {
        observed: u64,
        key: String,
    },
    InterfaceOpenTab {
        title: String,
    },
    InterfaceSplit {
        slot: String,
        division: Division,
    },
    InterfaceWithdraw {
        slot: String,
    },
    InterfaceRender {
        frame: hl_gui::Frame,
    },
    InterfaceRenderAt {
        slot: String,
        frame: hl_gui::Frame,
    },
    SourceResize {
        mutation: hl_gui::SourceMutation,
    },
    SourceResizeAt {
        slot: String,
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
            Self::WorkspaceUpdate { .. } => Capability::WorkspaceConfigure,
            Self::WorkspaceCreate { .. }
            | Self::WorkspaceDelete { .. }
            | Self::WorkspaceStart { .. }
            | Self::WorkspaceStop { .. }
            | Self::WorkspaceRestart { .. } => Capability::WorkspaceControl,
            Self::WorkspaceEnvironmentPatch { .. } => Capability::WorkspaceEnvironmentWrite,
            Self::ExtensionList | Self::ExtensionCatalogue | Self::ExtensionInspect { .. } => Capability::ExtensionRead,
            Self::ExtensionEnable { .. } | Self::ExtensionDisable { .. } | Self::ExtensionRetry { .. } => {
                Capability::ExtensionControl
            }
            Self::ExtensionRemove { .. } => Capability::ExtensionRemove,
            Self::ExtensionAcquisitionStart { .. }
            | Self::ExtensionAcquisitionStatus { .. }
            | Self::ExtensionAcquisitionCancel { .. }
            | Self::ExtensionInstall { .. }
            | Self::ExtensionUpdate { .. } => Capability::ExtensionInstall,
            Self::ContainerList
            | Self::ContainerInspect { .. }
            | Self::ContainerInspectObserved { .. }
            | Self::ContainerProcesses { .. }
            | Self::ContainerLogs { .. }
            | Self::ExecutionInspect { .. }
            | Self::ExecutionList
            | Self::ExecutionLogs { .. }
            | Self::ExecutionOutput { .. }
            | Self::ExecutionWait { .. } => Capability::ContainerRead,
            Self::ContainerCreate { .. } => Capability::ContainerCreate,
            Self::ContainerStart { .. }
            | Self::ContainerStop { .. }
            | Self::ContainerPause { .. }
            | Self::ContainerUnpause { .. }
            | Self::ContainerRestart { .. }
            | Self::ContainerRename { .. }
            | Self::ContainerKill { .. } => Capability::ContainerLifecycle,
            Self::ContainerRemove { .. } => Capability::ContainerRemove,
            Self::ExecutionKill { .. }
            | Self::ExecutionCancel { .. }
            | Self::ExecutionRemove { .. }
            | Self::ContainerExec { .. }
            | Self::ContainerExecCredential { .. } => Capability::ContainerExecute,
            Self::ExecutionWrite { .. } | Self::ExecutionCloseInput { .. } => Capability::ContainerInput,
            Self::ContainerAttachTerminal { .. } => Capability::ContainerAttach,
            Self::ImageList | Self::ImageInspect { .. } => Capability::ImageRead,
            Self::ImagePullStart { .. } | Self::ImagePullStatus { .. } | Self::ImagePullCancel { .. } => {
                Capability::ImagePull
            }
            Self::ImageRemove { .. } => Capability::ImageRemove,
            Self::ImagePrune => Capability::ImagePrune,
            Self::VolumeList | Self::VolumeInspect { .. } => Capability::VolumeRead,
            Self::VolumeCreate { .. } | Self::VolumeRemove { .. } => Capability::VolumeWrite,
            Self::NetworkList | Self::NetworkInspect { .. } => Capability::NetworkRead,
            Self::NetworkCreate { .. }
            | Self::NetworkRemove { .. }
            | Self::NetworkConnect { .. }
            | Self::NetworkDisconnect { .. } => Capability::NetworkWrite,
            Self::TerminalTabs | Self::TerminalTopology => Capability::TerminalRead,
            Self::PaneList => Capability::PaneObserve,
            Self::TerminalWritePane { .. }
            | Self::TerminalCommandWrite { .. }
            | Self::TerminalCommandCloseInput { .. } => Capability::TerminalInput,
            Self::TerminalFocusTab { .. }
            | Self::TerminalFocusPane { .. }
            | Self::TerminalFocusPaneObserved { .. } => Capability::TerminalFocus,
            Self::TerminalSpawn { .. }
            | Self::TerminalSpawnObserved { .. }
            | Self::TerminalCommandStart { .. }
            | Self::TerminalCommandCancel { .. } => Capability::TerminalProcessControl,
            Self::TerminalOpenTab { .. }
            | Self::TerminalPinTab { .. }
            | Self::TerminalSplit { .. }
            | Self::TerminalSplitObserved { .. }
            | Self::TerminalResizeGrid { .. }
            | Self::TerminalResizeGridObserved { .. }
            | Self::TerminalClosePane { .. }
            | Self::TerminalClosePaneObserved { .. }
            | Self::TerminalRetitlePane { .. }
            | Self::TerminalRetitlePaneObserved { .. }
            | Self::TerminalRatio { .. }
            | Self::TerminalRatioObserved { .. } => Capability::TerminalLayoutControl,
            Self::TerminalSwitchOccupant { .. } | Self::TerminalSwitchOccupantObserved { .. } => {
                Capability::TerminalProcessControl
            }
            // Reading what a shell printed is what `TerminalOutput` was separated
            // out for: listing panes says a pane exists, this says what was typed
            // into it and what came back.
            Self::TerminalReadPane { .. }
            | Self::TerminalCommandInspect { .. }
            | Self::TerminalCommandOutput { .. }
            | Self::TerminalCommandWait { .. } => Capability::TerminalOutput,
            Self::PaneSemanticRead { .. } => Capability::PaneSemanticRead,
            Self::PaneSemanticAction { .. } => Capability::PaneSemanticControl,
            Self::FilesystemInventory
            | Self::FilesystemChanges { .. }
            | Self::FilesystemList { .. }
            | Self::FilesystemListPage { .. }
            | Self::FilesystemRead { .. }
            | Self::FilesystemReadRange { .. }
            | Self::FilesystemReadRanges { .. }
            | Self::FilesystemStat { .. } => Capability::FilesystemRead,
            Self::FilesystemWrite { .. }
            | Self::FilesystemWriteObserved { .. }
            | Self::FilesystemCreateObserved { .. }
            | Self::FilesystemMkdir { .. }
            | Self::FilesystemRename { .. }
            | Self::FilesystemRenameObserved { .. }
            | Self::FilesystemRemove { .. }
            | Self::FilesystemRemoveObserved { .. } => Capability::FilesystemWrite,
            Self::StateRead => Capability::StateRead,
            Self::StateWrite { .. } | Self::StateClear { .. } => Capability::StateWrite,
            Self::PreferenceRead => Capability::PreferenceRead,
            Self::PreferenceSet { .. } | Self::PreferenceRemove { .. } => Capability::PreferenceWrite,
            Self::CredentialRead { .. } => Capability::CredentialRead,
            Self::CredentialSet { .. } | Self::CredentialRemove { .. } => Capability::CredentialWrite,
            Self::InterfaceOpenTab { .. }
            | Self::InterfaceSplit { .. }
            | Self::InterfaceWithdraw { .. }
            | Self::InterfaceRender { .. }
            | Self::InterfaceRenderAt { .. }
            | Self::SourceResize { .. }
            | Self::SourceResizeAt { .. } => Capability::Interface,
            Self::NotificationPublish { .. } => Capability::NotificationPublish,
            Self::EventSubscribe { topic } | Self::EventUnsubscribe { topic } => topic.capability(),
        }
    }

    /// The path this call reaches, when it names one. A call returning a path
    /// here is confined to the extension's declared roots.
    #[must_use]
    pub const fn path(&self) -> Option<&RelativePath> {
        match self {
            Self::FilesystemList { path }
            | Self::FilesystemListPage { path, .. }
            | Self::FilesystemRead { path }
            | Self::FilesystemReadRange { path, .. }
            | Self::FilesystemStat { path }
            | Self::FilesystemWrite { path, .. }
            | Self::FilesystemWriteObserved { path, .. }
            | Self::FilesystemCreateObserved { path, .. }
            | Self::FilesystemMkdir { path }
            | Self::FilesystemRemove { path }
            | Self::FilesystemRemoveObserved { path, .. } => Some(path),
            Self::FilesystemRename { from, .. } | Self::FilesystemRenameObserved { from, .. } => Some(from),
            _ => None,
        }
    }
}

/// A stream of host state an extension can follow.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Topic {
    Containers,
    ContainerInventory,
    Executions,
    Images,
    ImagePulls,
    Volumes,
    Networks,
    Terminal,
    PaneChanges,
    Extensions,
    ExtensionAcquisitions,
    WorkspaceLifecycle,
    WorkspaceEvents,
    Filesystem,
}

impl Topic {
    /// The capability required to follow this topic. Checked when subscribing
    /// and again on every emission, so a revoked grant stops the stream.
    #[must_use]
    pub const fn capability(self) -> Capability {
        match self {
            Self::Containers => Capability::ContainerRead,
            Self::ContainerInventory => Capability::ContainerRead,
            Self::Executions => Capability::ContainerRead,
            Self::Images => Capability::ImageRead,
            Self::ImagePulls => Capability::ImagePull,
            Self::Volumes => Capability::VolumeRead,
            Self::Networks => Capability::NetworkRead,
            Self::Terminal => Capability::TerminalRead,
            Self::PaneChanges => Capability::PaneObserve,
            Self::Extensions => Capability::ExtensionRead,
            Self::ExtensionAcquisitions => Capability::ExtensionInstall,
            Self::WorkspaceLifecycle => Capability::WorkspaceRead,
            Self::WorkspaceEvents => Capability::WorkspaceEvents,
            Self::Filesystem => Capability::FilesystemRead,
        }
    }

    pub const ALL: &'static [Self] = &[
        Self::Containers,
        Self::ContainerInventory,
        Self::Executions,
        Self::Images,
        Self::ImagePulls,
        Self::Volumes,
        Self::Networks,
        Self::Terminal,
        Self::PaneChanges,
        Self::Extensions,
        Self::ExtensionAcquisitions,
        Self::WorkspaceLifecycle,
        Self::WorkspaceEvents,
        Self::Filesystem,
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
    WorkspaceEnvironmentPatch(crate::port::WorkspaceEnvironmentPatchResult),
    Workspaces(Vec<WorkspaceState>),
    Extensions(Vec<crate::port::ExtensionSummary>),
    ExtensionCatalogue(crate::port::ExtensionCatalogue),
    Extension(crate::port::ExtensionSummary),
    ExtensionAcquisitionJob(crate::port::ExtensionAcquisitionJob),
    ExtensionAcquisition(crate::port::ExtensionAcquisitionStatus),
    Containers(Vec<ContainerSummary>),
    Container(ContainerSummary),
    Processes(ProcessList),
    Logs(ContainerOutput),
    ExecutionOutput(crate::port::ExecutionOutputPage),
    Execution(ExecutionSummary),
    Executions(ExecutionList),
    TerminalCommand(crate::port::TerminalCommand),
    TerminalCommandOutput(crate::port::TerminalCommandOutput),
    TerminalCommandInput(crate::port::TerminalCommandInput),
    Images(crate::port::ImageInventory),
    Image(ImageSummary),
    ImagePullJob(ImagePullJob),
    ImagePull(ImagePullStatus),
    ImageDetails(ImageDetails),
    ImagePrune(ImagePruneResult),
    Volumes(crate::port::VolumeInventory),
    Volume(VolumeSummary),
    Networks(crate::port::NetworkInventory),
    Network(NetworkSummary),
    Tabs(Vec<TabSummary>),
    Topology(TerminalTopology),
    Panes(PaneInventory),
    Text(PaneText),
    Semantics(crate::port::PaneSemanticTree),
    FileInventory(crate::port::FileInventory),
    FileChanges(crate::port::FileChangePage),
    Entries(Vec<Entry>),
    DirectoryPage(crate::port::DirectoryPage),
    Entry(Entry),
    Contents(Vec<u8>),
    FileRange(crate::port::FileRange),
    FileRanges(Vec<crate::port::FileRange>),
    State(crate::port::ExtensionState),
    Preferences(crate::port::ExtensionPreferences),
    Credential(crate::port::ExtensionCredential),
    Revision(u64),
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
    Unavailable { detail: String },
    Absent { detail: String },
    Conflict { detail: String },
    Failed { detail: String },
    Unsupported { call: String },
}

impl From<HostError> for Failure {
    fn from(error: HostError) -> Self {
        match error {
            HostError::Unavailable(detail) => Self::Unavailable { detail },
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
            Request::PaneSemanticRead { slot: "7".into() }.capability(),
            Capability::PaneSemanticRead
        );
        assert_eq!(
            Request::PaneSemanticAction {
                slot: "7".into(),
                action: crate::port::PaneSemanticAction {
                    generation: 0,
                    revision: 1,
                    node: 2,
                    action: crate::port::SemanticActionKind::Invoke,
                    value: None,
                },
            }
            .capability(),
            Capability::PaneSemanticControl
        );
        assert_eq!(
            Request::EventSubscribe {
                topic: Topic::WorkspaceEvents
            }
            .capability(),
            Capability::WorkspaceEvents
        );
        assert_eq!(Request::ContainerList.capability(), Capability::ContainerRead);
        assert_eq!(
            Request::ContainerStop {
                id: "a".into(),
                generation: 4
            }
            .capability(),
            Capability::ContainerLifecycle
        );
        assert_eq!(
            Request::ContainerCreate {
                spec: crate::port::ContainerCreateSpec {
                    image: "alpine:3.20".into(),
                    name: "worker".into(),
                    hostname: None,
                    entrypoint: None,
                    command: Vec::new(),
                    environment: Vec::new(),
                    working_directory: None,
                    user: None,
                    labels: Vec::new(),
                    mounts: Vec::new(),
                    network: None,
                    ports: Vec::new(),
                    memory_mb: None,
                    cpus: None,
                    pids_limit: None,
                },
            }
            .capability(),
            Capability::ContainerCreate
        );
        assert_eq!(
            Request::ContainerExec {
                environment: Vec::new(),
                id: "a".into(),
                generation: 4,
                command: vec!["true".into()],
                user: None,
                working_directory: None,
                stdin: false,
            }
            .capability(),
            Capability::ContainerExecute
        );
        assert_eq!(
            Request::ContainerAttachTerminal {
                id: "a".repeat(64),
                command: vec!["sh".into()],
            }
            .capability(),
            Capability::ContainerAttach
        );
        assert_eq!(Request::ImageList.capability(), Capability::ImageRead);
        assert_eq!(
            Request::ImagePullStart {
                reference: "registry-1.docker.io/library/alpine:latest".into()
            }
            .capability(),
            Capability::ImagePull
        );
    }

    #[test]
    fn revision_only_semantic_actions_fail_closed_at_the_wire_boundary() {
        let legacy = serde_json::json!({
            "call": "pane_semantic_action",
            "with": {
                "slot": "7",
                "action": { "revision": 1, "node": 2, "action": "invoke", "value": null }
            }
        });
        assert!(serde_json::from_value::<Request>(legacy).is_err());
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
            Request::TerminalClosePaneObserved {
                slot: "1".into(),
                generation: 2,
                revision: 3,
            },
            Request::TerminalRetitlePane {
                slot: "1".into(),
                title: "Build logs".into(),
            },
            Request::TerminalRatio {
                slot: "1".into(),
                ratio: 0.5,
            },
            Request::TerminalRatioObserved {
                slot: "1".into(),
                generation: 2,
                revision: 3,
                ratio: 0.5,
            },
        ] {
            assert_eq!(request.capability(), Capability::TerminalLayoutControl, "{request:?}");
        }
        assert_eq!(
            Request::TerminalFocusPane { slot: "1".into() }.capability(),
            Capability::TerminalFocus
        );
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
            Request::WorkspaceDelete {
                name: "other".into(),
                generation: "0123456789abcdef0123456789abcdef".into(),
            }
            .capability(),
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
            Request::FilesystemMkdir { path: path.clone() },
            Request::FilesystemRemove { path: path.clone() },
        ] {
            assert_eq!(request.path(), Some(&path), "{request:?} must be confined");
        }
        let rename = Request::FilesystemRename {
            from: path.clone(),
            to: RelativePath::new("logs/new.log").unwrap(),
        };
        assert_eq!(rename.path(), Some(&path));
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
        assert_eq!(Topic::Extensions.capability(), Capability::ExtensionRead);
        assert_eq!(Topic::ExtensionAcquisitions.capability(), Capability::ExtensionInstall);
        assert_eq!(Topic::WorkspaceLifecycle.capability(), Capability::WorkspaceRead);
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
            serde_json::from_str("{\"call\":\"container_stop\",\"with\":{\"id\":\"c1\",\"generation\":4}}")
                .expect("valid");
        assert_eq!(
            accepted,
            Request::ContainerStop {
                id: "c1".into(),
                generation: 4
            }
        );

        let rename = Request::ContainerRename {
            id: "a".repeat(64),
            generation: 4,
            name: "worker_2.prod".into(),
        };
        assert_eq!(
            serde_json::to_value(&rename).expect("rename wire request"),
            serde_json::json!({
                "call": "container_rename",
                "with": { "id": "a".repeat(64), "generation": 4, "name": "worker_2.prod" }
            })
        );
        assert_eq!(
            serde_json::from_value::<Request>(serde_json::to_value(&rename).unwrap()).unwrap(),
            rename
        );
    }
}
