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
    #[serde(rename = "containers:read")]
    ContainerRead,
    #[serde(rename = "containers:control")]
    ContainerControl,
    /// Opens an interactive, kill-on-disconnect terminal in an existing container.
    /// Kept separate from detached container mutation and ordinary terminal control.
    #[serde(rename = "containers:attach")]
    ContainerAttach,
    #[serde(rename = "images:read")]
    ImageRead,
    #[serde(rename = "images:write")]
    ImageWrite,
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
    #[serde(rename = "terminals:control")]
    TerminalControl,
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
    #[serde(rename = "interface:render")]
    Interface,
}

impl Capability {
    /// The name this permission is written with in a manifest and on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceRead => "workspaces:read",
            Self::WorkspaceControl => "workspaces:control",
            Self::WorkspaceEvents => "workspaces:events",
            Self::ContainerRead => "containers:read",
            Self::ContainerControl => "containers:control",
            Self::ContainerAttach => "containers:attach",
            Self::ImageRead => "images:read",
            Self::ImageWrite => "images:write",
            Self::VolumeRead => "volumes:read",
            Self::VolumeWrite => "volumes:write",
            Self::NetworkRead => "networks:read",
            Self::NetworkWrite => "networks:write",
            Self::TerminalRead => "terminals:read",
            Self::TerminalControl => "terminals:control",
            Self::TerminalOutput => "terminals:output",
            Self::PaneObserve => "panes:observe",
            Self::PaneSemanticRead => "panes:semantic-read",
            Self::PaneSemanticControl => "panes:semantic-control",
            Self::ExtensionRead => "extensions:read",
            Self::ExtensionControl => "extensions:control",
            Self::ExtensionInstall => "extensions:install",
            Self::FilesystemRead => "filesystem:read",
            Self::FilesystemWrite => "filesystem:write",
            Self::Interface => "interface:render",
        }
    }

    /// Whether holding this permits mutation. Used only to describe a grant to
    /// a person at install time; enforcement is always by exact variant.
    #[must_use]
    pub const fn mutates(self) -> bool {
        matches!(
            self,
            Self::WorkspaceControl
                | Self::ContainerControl
                | Self::ContainerAttach
                | Self::ImageWrite
                | Self::VolumeWrite
                | Self::NetworkWrite
                | Self::TerminalControl
                | Self::PaneSemanticControl
                | Self::ExtensionControl
                | Self::ExtensionInstall
                | Self::FilesystemWrite
        )
    }

    /// Whether this grant amounts to running code inside the workspace. The
    /// install prompt has to say so plainly rather than imply a sandbox.
    #[must_use]
    pub const fn executes(self) -> bool {
        matches!(
            self,
            Self::WorkspaceControl | Self::ContainerControl | Self::ContainerAttach | Self::TerminalControl
        )
    }

    /// Every permission this domain declares.
    pub const ALL: &'static [Self] = &[
        Self::WorkspaceRead,
        Self::WorkspaceControl,
        Self::WorkspaceEvents,
        Self::ContainerRead,
        Self::ContainerControl,
        Self::ContainerAttach,
        Self::ImageRead,
        Self::ImageWrite,
        Self::VolumeRead,
        Self::VolumeWrite,
        Self::NetworkRead,
        Self::NetworkWrite,
        Self::TerminalRead,
        Self::TerminalControl,
        Self::TerminalOutput,
        Self::PaneObserve,
        Self::PaneSemanticRead,
        Self::PaneSemanticControl,
        Self::ExtensionRead,
        Self::ExtensionControl,
        Self::ExtensionInstall,
        Self::FilesystemRead,
        Self::FilesystemWrite,
        Self::Interface,
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
        assert!(!grant.holds(Capability::ContainerControl));
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
        let requested = Grant::new([Capability::ContainerRead, Capability::ContainerControl]);

        assert!(!recorded.covers(&requested));
        assert_eq!(recorded.missing(&requested), vec![Capability::ContainerControl]);
        assert_eq!(recorded.intersect(&requested), recorded);
    }

    #[test]
    fn execution_grants_are_identified_for_the_consent_prompt() {
        assert!(Grant::new([Capability::ContainerControl]).executes());
        assert!(Grant::new([Capability::WorkspaceControl]).executes());
        assert!(Grant::new([Capability::TerminalControl]).executes());
        assert!(!Grant::new([Capability::ContainerRead, Capability::Interface]).executes());
    }
}
