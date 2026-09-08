//! What an extension is allowed to do.
//!
//! The permissions this domain declares. The concept of a permission, the grant
//! that holds a set of them, and the check itself all live in `hl-rpc`; the list
//! belongs here, because it is the list of things a workspace can be asked for.

/// One permission an extension may hold.
///
/// Read and write are always separate variants so an authority check is set
/// membership rather than verb parsing, and the two most dangerous grants —
/// reading pane output and controlling containers — cannot ride along with a
/// milder one.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
pub enum Capability {
    #[serde(rename = "workspaces:read")]
    WorkspaceRead,
    /// Creating, changing, starting, stopping, or deleting workspaces.
    #[serde(rename = "workspaces:control")]
    WorkspaceControl,
    /// Observing keyboard, focus, and pointer activity across the workspace window.
    #[serde(rename = "workspaces:events")]
    WorkspaceEvents,
    #[serde(rename = "workspace-environment:read")]
    WorkspaceEnvironmentRead,
    #[serde(rename = "workspace-environment:write")]
    WorkspaceEnvironmentWrite,
    #[serde(rename = "containers:read")]
    ContainerRead,
    /// Creates a new container from an explicitly consented image and configuration.
    #[serde(rename = "containers:create")]
    ContainerCreate,
    /// Starts detached processes inside explicitly consented containers and
    /// controls only those execution records.
    #[serde(rename = "containers:execute")]
    ContainerExecute,
    /// Starts, stops, pauses, resumes, restarts, renames, or signals an
    /// explicitly consented container without granting deletion.
    #[serde(rename = "containers:lifecycle")]
    ContainerLifecycle,
    /// Permanently removes an explicitly consented container.
    #[serde(rename = "containers:remove")]
    ContainerRemove,
    /// Opens an interactive, kill-on-disconnect terminal in an existing container.
    /// Kept separate from detached container mutation and ordinary terminal control.
    #[serde(rename = "containers:attach")]
    ContainerAttach,
    #[serde(rename = "images:read")]
    ImageRead,
    #[serde(rename = "images:pull")]
    ImagePull,
    #[serde(rename = "images:remove")]
    ImageRemove,
    #[serde(rename = "images:prune")]
    ImagePrune,
    #[serde(rename = "volumes:read")]
    VolumeRead,
    #[serde(rename = "volumes:write")]
    VolumeWrite,
    #[serde(rename = "networks:read")]
    NetworkRead,
    #[serde(rename = "networks:write")]
    NetworkWrite,
    #[serde(rename = "terminals:read")]
    TerminalRead,
    /// Injecting bytes into an existing terminal pane.
    #[serde(rename = "terminals:input")]
    TerminalInput,
    /// Creating, removing, focusing, or rearranging terminal panes and tabs.
    #[serde(rename = "terminals:layout-control")]
    TerminalLayoutControl,
    /// Replacing the process running in an existing terminal pane.
    #[serde(rename = "terminals:process-control")]
    TerminalProcessControl,
    /// Reading the bytes flowing through a pane. Deliberately separate from
    /// `TerminalRead`: listing panes and reading what was typed into a shell
    /// are different kinds of access.
    #[serde(rename = "terminals:output")]
    TerminalOutput,
    /// Observing bounded pane-change metadata. This reveals activity and stable
    /// pane identities, but never terminal bytes or semantic values.
    #[serde(rename = "panes:observe")]
    PaneObserve,
    #[serde(rename = "panes:semantic-read")]
    PaneSemanticRead,
    #[serde(rename = "panes:semantic-control")]
    PaneSemanticControl,
    /// Reading installed extension identity and lifecycle status.
    #[serde(rename = "extensions:read")]
    ExtensionRead,
    /// Enabling, disabling, or removing installed extension records.
    #[serde(rename = "extensions:control")]
    ExtensionControl,
    /// Acquiring and consent-committing extension images.
    #[serde(rename = "extensions:install")]
    ExtensionInstall,
    #[serde(rename = "filesystem:read")]
    FilesystemRead,
    #[serde(rename = "filesystem:write")]
    FilesystemWrite,
    /// Reads only this extension's private host-managed state blob.
    #[serde(rename = "state:read")]
    StateRead,
    /// Replaces or clears only this extension's private host-managed state blob.
    #[serde(rename = "state:write")]
    StateWrite,
    #[serde(rename = "interface:render")]
    Interface,
    /// Publishes bounded user-visible notifications outside an extension surface.
    #[serde(rename = "notifications:publish")]
    NotificationPublish,
}

impl Capability {
    /// The name this permission is written with in a manifest and on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceRead => "workspaces:read",
            Self::WorkspaceControl => "workspaces:control",
            Self::WorkspaceEvents => "workspaces:events",
            Self::WorkspaceEnvironmentRead => "workspace-environment:read",
            Self::WorkspaceEnvironmentWrite => "workspace-environment:write",
            Self::ContainerRead => "containers:read",
            Self::ContainerCreate => "containers:create",
            Self::ContainerExecute => "containers:execute",
            Self::ContainerLifecycle => "containers:lifecycle",
            Self::ContainerRemove => "containers:remove",
            Self::ContainerAttach => "containers:attach",
            Self::ImageRead => "images:read",
            Self::ImagePull => "images:pull",
            Self::ImageRemove => "images:remove",
            Self::ImagePrune => "images:prune",
            Self::VolumeRead => "volumes:read",
            Self::VolumeWrite => "volumes:write",
            Self::NetworkRead => "networks:read",
            Self::NetworkWrite => "networks:write",
            Self::TerminalRead => "terminals:read",
            Self::TerminalInput => "terminals:input",
            Self::TerminalLayoutControl => "terminals:layout-control",
            Self::TerminalProcessControl => "terminals:process-control",
            Self::TerminalOutput => "terminals:output",
            Self::PaneObserve => "panes:observe",
            Self::PaneSemanticRead => "panes:semantic-read",
            Self::PaneSemanticControl => "panes:semantic-control",
            Self::ExtensionRead => "extensions:read",
            Self::ExtensionControl => "extensions:control",
            Self::ExtensionInstall => "extensions:install",
            Self::FilesystemRead => "filesystem:read",
            Self::FilesystemWrite => "filesystem:write",
            Self::StateRead => "state:read",
            Self::StateWrite => "state:write",
            Self::Interface => "interface:render",
            Self::NotificationPublish => "notifications:publish",
        }
    }

    /// Whether holding this permits mutation. Used only to describe a grant to
    /// a person at install time; enforcement is always by exact variant.
    #[must_use]
    pub const fn mutates(self) -> bool {
        matches!(
            self,
            Self::WorkspaceControl
                | Self::WorkspaceEnvironmentWrite
                | Self::ContainerCreate
                | Self::ContainerExecute
                | Self::ContainerLifecycle
                | Self::ContainerRemove
                | Self::ContainerAttach
                | Self::ImagePull
                | Self::ImageRemove
                | Self::ImagePrune
                | Self::VolumeWrite
                | Self::NetworkWrite
                | Self::TerminalInput
                | Self::TerminalLayoutControl
                | Self::TerminalProcessControl
                | Self::PaneSemanticControl
                | Self::ExtensionControl
                | Self::ExtensionInstall
                | Self::FilesystemWrite
                | Self::StateWrite
                | Self::NotificationPublish
        )
    }

    /// Whether this grant amounts to running code inside the workspace. The
    /// install prompt has to say so plainly rather than imply a sandbox.
    #[must_use]
    pub const fn executes(self) -> bool {
        matches!(
            self,
            Self::WorkspaceControl
                | Self::ContainerCreate
                | Self::ContainerExecute
                | Self::ContainerAttach
                | Self::TerminalInput
                | Self::TerminalProcessControl
        )
    }

    /// Every permission this domain declares.
    pub const ALL: &'static [Self] = &[
        Self::WorkspaceRead,
        Self::WorkspaceControl,
        Self::WorkspaceEvents,
        Self::WorkspaceEnvironmentRead,
        Self::WorkspaceEnvironmentWrite,
        Self::ContainerRead,
        Self::ContainerCreate,
        Self::ContainerExecute,
        Self::ContainerLifecycle,
        Self::ContainerRemove,
        Self::ContainerAttach,
        Self::ImageRead,
        Self::ImagePull,
        Self::ImageRemove,
        Self::ImagePrune,
        Self::VolumeRead,
        Self::VolumeWrite,
        Self::NetworkRead,
        Self::NetworkWrite,
        Self::TerminalRead,
        Self::TerminalInput,
        Self::TerminalLayoutControl,
        Self::TerminalProcessControl,
        Self::TerminalOutput,
        Self::PaneObserve,
        Self::PaneSemanticRead,
        Self::PaneSemanticControl,
        Self::ExtensionRead,
        Self::ExtensionControl,
        Self::ExtensionInstall,
        Self::FilesystemRead,
        Self::FilesystemWrite,
        Self::StateRead,
        Self::StateWrite,
        Self::Interface,
        Self::NotificationPublish,
    ];
}

impl hl_rpc::Capability for Capability {
    const DOMAIN: &'static str = "workspace";
    const ALL: &'static [Self] = Self::ALL;

    fn name(&self) -> &'static str {
        self.as_str()
    }

    fn executes(&self) -> bool {
        Self::executes(*self)
    }
}

/// A granted set of this domain's permissions.
pub type Grant = hl_rpc::Grant<Capability>;

#[cfg(test)]
mod tests {
    use super::{Capability, Grant};

    #[test]
    fn a_grant_reports_exactly_what_it_holds() {
        let grant = Grant::new([Capability::ContainerRead, Capability::Interface]);
        assert!(grant.holds(Capability::ContainerRead));
        assert!(!grant.holds(Capability::ContainerLifecycle));
        assert_eq!(grant.len(), 2);
    }

    #[test]
    fn reading_never_implies_writing() {
        let grant = Grant::new([
            Capability::ContainerRead,
            Capability::ImageRead,
            Capability::FilesystemRead,
            Capability::TerminalRead,
        ]);
        for capability in Capability::ALL.iter().filter(|entry| entry.mutates()) {
            assert!(!grant.holds(*capability), "{capability:?} must not be implied");
        }
        assert!(!grant.holds(Capability::TerminalOutput));
    }

    #[test]
    fn a_wider_request_is_narrowed_to_the_recorded_grant() {
        let recorded = Grant::new([Capability::ContainerRead]);
        let requested = Grant::new([Capability::ContainerRead, Capability::ContainerLifecycle]);

        assert!(!recorded.covers(&requested));
        assert_eq!(recorded.missing(&requested), vec![Capability::ContainerLifecycle]);
        assert_eq!(recorded.intersect(&requested), recorded);
    }

    #[test]
    fn execution_grants_are_identified_for_the_consent_prompt() {
        assert!(Grant::new([Capability::ContainerExecute]).executes());
        assert!(!Grant::new([Capability::ContainerLifecycle]).executes());
        assert!(Grant::new([Capability::WorkspaceControl]).executes());
        assert!(Grant::new([Capability::TerminalInput]).executes());
        assert!(Grant::new([Capability::TerminalProcessControl]).executes());
        assert!(!Grant::new([Capability::TerminalLayoutControl]).executes());
        assert!(!Grant::new([Capability::ContainerRead, Capability::Interface]).executes());
    }
}
