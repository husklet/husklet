//! What an extension is allowed to do, and the grant it was given.

use std::collections::BTreeSet;

/// One permission an extension may hold.
///
/// Read and write are always separate variants so an authority check is set
/// membership rather than verb parsing, and the two most dangerous grants —
/// reading pane output and controlling containers — cannot ride along with a
/// milder one.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    WorkspaceRead,
    ContainerRead,
    ContainerControl,
    ImageRead,
    ImageWrite,
    VolumeRead,
    VolumeWrite,
    NetworkRead,
    NetworkWrite,
    TerminalRead,
    TerminalControl,
    /// Reading the bytes flowing through a pane. Deliberately separate from
    /// `TerminalRead`: listing panes and reading what was typed into a shell
    /// are different kinds of access.
    TerminalOutput,
    FilesystemRead,
    FilesystemWrite,
    Interface,
}

impl Capability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceRead => "workspace-read",
            Self::ContainerRead => "container-read",
            Self::ContainerControl => "container-control",
            Self::ImageRead => "image-read",
            Self::ImageWrite => "image-write",
            Self::VolumeRead => "volume-read",
            Self::VolumeWrite => "volume-write",
            Self::NetworkRead => "network-read",
            Self::NetworkWrite => "network-write",
            Self::TerminalRead => "terminal-read",
            Self::TerminalControl => "terminal-control",
            Self::TerminalOutput => "terminal-output",
            Self::FilesystemRead => "filesystem-read",
            Self::FilesystemWrite => "filesystem-write",
            Self::Interface => "interface",
        }
    }

    /// Whether holding this permits mutation. Used only to describe a grant to
    /// a person at install time; enforcement is always by exact variant.
    #[must_use]
    pub const fn mutates(self) -> bool {
        matches!(
            self,
            Self::ContainerControl
                | Self::ImageWrite
                | Self::VolumeWrite
                | Self::NetworkWrite
                | Self::TerminalControl
                | Self::FilesystemWrite
        )
    }

    /// Whether this grant amounts to running code inside the workspace. The
    /// install prompt has to say so plainly rather than imply a sandbox.
    #[must_use]
    pub const fn executes(self) -> bool {
        matches!(self, Self::ContainerControl | Self::TerminalControl)
    }

    pub const ALL: &'static [Self] = &[
        Self::WorkspaceRead,
        Self::ContainerRead,
        Self::ContainerControl,
        Self::ImageRead,
        Self::ImageWrite,
        Self::VolumeRead,
        Self::VolumeWrite,
        Self::NetworkRead,
        Self::NetworkWrite,
        Self::TerminalRead,
        Self::TerminalControl,
        Self::TerminalOutput,
        Self::FilesystemRead,
        Self::FilesystemWrite,
        Self::Interface,
    ];
}

/// A granted set of permissions.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct Grant {
    held: BTreeSet<Capability>,
}

impl Grant {
    #[must_use]
    pub fn new(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            held: capabilities.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn holds(&self, capability: Capability) -> bool {
        self.held.contains(&capability)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.held.iter().copied()
    }

    /// Whether every capability in `other` is already held. A re-consent prompt
    /// is required exactly when this is false.
    #[must_use]
    pub fn covers(&self, other: &Self) -> bool {
        other.held.is_subset(&self.held)
    }

    /// The permissions in `other` that this grant does not hold.
    #[must_use]
    pub fn missing(&self, other: &Self) -> Vec<Capability> {
        other.held.difference(&self.held).copied().collect()
    }

    /// Narrows to what both hold. An updated manifest asking for more must
    /// start from the recorded grant, never widen itself.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            held: self.held.intersection(&other.held).copied().collect(),
        }
    }

    /// Whether this grant amounts to code execution inside the workspace.
    #[must_use]
    pub fn executes(&self) -> bool {
        self.held.iter().copied().any(Capability::executes)
    }
}

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
        assert!(Grant::new([Capability::TerminalControl]).executes());
        assert!(!Grant::new([Capability::ContainerRead, Capability::Interface]).executes());
    }
}
