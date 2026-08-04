use crate::{GuestPathBytes, PathError};

/// Opaque identity of the directory relative paths are resolved beneath.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct OpenDirectory(u64);

impl OpenDirectory {
    #[must_use]
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Bitset describing the guest's requested open behavior.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OpenIntent(u32);

impl OpenIntent {
    pub const READ: u32 = 1 << 0;
    pub const WRITE: u32 = 1 << 1;
    pub const CREATE: u32 = 1 << 2;
    pub const TRUNCATE: u32 = 1 << 3;
    pub const APPEND: u32 = 1 << 4;
    pub const PATH_ONLY: u32 = 1 << 5;
    pub const NOFOLLOW: u32 = 1 << 6;
    pub const DIRECTORY: u32 = 1 << 7;
    pub const TEMPORARY: u32 = 1 << 8;
    pub const NO_SYMLINKS: u32 = 1 << 9;
    pub const EXCLUSIVE: u32 = 1 << 10;
    const WRITING: u32 = Self::WRITE | Self::CREATE | Self::TRUNCATE | Self::APPEND | Self::TEMPORARY;

    /// Preserves raw open-plan intent bits for forward compatibility.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Returns every original bit, including unknown forward-compatible bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    const fn contains(self, bits: u32) -> bool {
        self.0 & bits == bits
    }

    const fn writes(self) -> bool {
        self.0 & Self::WRITING != 0
    }
}

/// Synthetic namespace selected before any host lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntheticFilesystem {
    Proc,
    Sys,
    Dev,
}

impl SyntheticFilesystem {
    fn classify(path: &GuestPathBytes) -> Option<Self> {
        let path = path.as_bytes();
        if path == b"/proc" || path.starts_with(b"/proc/") {
            Some(Self::Proc)
        } else if path == b"/sys" || path.starts_with(b"/sys/") {
            Some(Self::Sys)
        } else if path == b"/dev" || path.starts_with(b"/dev/") {
            Some(Self::Dev)
        } else {
            None
        }
    }
}

/// Overlay operation required by an open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayAction {
    Lookup,
    CopyUp,
    Create,
}

/// Failure to validate an open request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenError {
    Path(PathError),
}

/// Namespace-owned input to open planning.
#[derive(Clone, Debug)]
pub struct OpenRequest {
    pub guest_path: GuestPathBytes,
    pub directory: OpenDirectory,
    pub intent: OpenIntent,
    pub overlay: bool,
    pub read_only: bool,
    pub final_symlink: bool,
}

/// A host-neutral namespace decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenDecision {
    HostPath,
    Synthetic(SyntheticFilesystem),
    Overlay(OverlayAction),
    Temporary,
    PermissionDenied,
}

impl OpenDecision {
    fn select(request: &OpenRequest, path: &GuestPathBytes) -> Self {
        if request.read_only && request.intent.writes() {
            return Self::PermissionDenied;
        }
        if request.intent.contains(OpenIntent::TEMPORARY) {
            return Self::Temporary;
        }
        if let Some(filesystem) = SyntheticFilesystem::classify(path) {
            return Self::Synthetic(filesystem);
        }
        if !request.overlay {
            return Self::HostPath;
        }
        Self::Overlay(OverlayAction::select(request.intent))
    }
}

impl OverlayAction {
    fn select(intent: OpenIntent) -> Self {
        if intent.contains(OpenIntent::CREATE) {
            Self::Create
        } else if intent.writes() {
            Self::CopyUp
        } else {
            Self::Lookup
        }
    }
}

/// Validated open decision with no native descriptor or pointer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenPlan {
    path: GuestPathBytes,
    directory: OpenDirectory,
    intent: OpenIntent,
    decision: OpenDecision,
    names_symlink: bool,
}

impl OpenPlan {
    /// Builds the same namespace-only decision as `hl_open_plan_build`.
    pub fn build(request: OpenRequest) -> Result<Self, OpenError> {
        if request.guest_path.as_bytes().is_empty() {
            return Err(OpenError::Path(PathError::Empty));
        }
        let path = request.guest_path.clone();
        let decision = OpenDecision::select(&request, &path);
        let names_symlink = matches!(decision, OpenDecision::HostPath | OpenDecision::Overlay(_))
            && request.final_symlink
            && request.intent.contains(OpenIntent::PATH_ONLY | OpenIntent::NOFOLLOW);
        Ok(Self {
            path,
            directory: request.directory,
            intent: request.intent,
            decision,
            names_symlink,
        })
    }

    #[must_use]
    pub fn path(&self) -> &GuestPathBytes {
        &self.path
    }

    #[must_use]
    pub const fn directory(&self) -> OpenDirectory {
        self.directory
    }

    #[must_use]
    pub const fn intent(&self) -> OpenIntent {
        self.intent
    }

    #[must_use]
    pub const fn decision(&self) -> OpenDecision {
        self.decision
    }

    #[must_use]
    pub const fn names_symlink(&self) -> bool {
        self.names_symlink
    }
}
