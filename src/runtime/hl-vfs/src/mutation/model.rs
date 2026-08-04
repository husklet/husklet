use crate::{
    AccessError, GuestName, GuestPathBytes, Kind, Metadata, NodeHandle, OpenPlan, Permissions, ResolveError,
    ResolvedComponent, Timestamp,
};

/// Opaque identity of one unpublished host-side namespace transaction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct VfsTransaction(u64);

impl VfsTransaction {
    #[must_use]
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Parent pin supplied to a transaction. The pin remains owned by the resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinnedParent<'name> {
    pub node: NodeHandle,
    pub name: &'name ResolvedComponent,
}

/// Linux renameat2 flags after fixed-ABI validation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenameFlags(u8);

impl RenameFlags {
    pub const NOREPLACE: u8 = 1;
    pub const EXCHANGE: u8 = 2;
    pub const WHITEOUT: u8 = 4;

    pub fn from_bits(bits: u32) -> Result<Self, Error> {
        if bits & !0x7 != 0
            || bits & u32::from(Self::EXCHANGE) != 0 && bits & u32::from(Self::NOREPLACE | Self::WHITEOUT) != 0
        {
            return Err(Error::InvalidArgument);
        }
        Ok(Self(bits as u8))
    }

    #[must_use]
    pub const fn contains(self, bit: u8) -> bool {
        self.0 & bit != 0
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// Timestamp selection accepted by utimensat after guest ABI decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimestampUpdate {
    Set(Timestamp),
    Now,
    Omit,
}

/// Host-neutral operation staged out of namespace view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    CopyUp {
        parent: NodeHandle,
        name: GuestName,
        recursive: bool,
    },
    Create {
        parent: NodeHandle,
        name: GuestName,
        kind: Kind,
        permissions: Permissions,
        device: u64,
    },
    Symlink {
        parent: NodeHandle,
        name: GuestName,
        target: GuestPathBytes,
    },
    HardLink {
        source_parent: NodeHandle,
        source_name: GuestName,
        target_parent: NodeHandle,
        target_name: GuestName,
        follow_source: bool,
    },
    Remove {
        parent: NodeHandle,
        name: GuestName,
        directory: bool,
        whiteout_lower: bool,
    },
    Rename {
        source_parent: NodeHandle,
        source_name: GuestName,
        target_parent: NodeHandle,
        target_name: GuestName,
        flags: RenameFlags,
        whiteout_lower: bool,
    },
    SetMetadata {
        parent: NodeHandle,
        name: GuestName,
        metadata: Metadata,
        nofollow: bool,
    },
    SetTimes {
        parent: NodeHandle,
        name: GuestName,
        accessed: TimestampUpdate,
        modified: TimestampUpdate,
        nofollow: bool,
    },
    Open {
        parent: NodeHandle,
        name: GuestName,
        plan: OpenPlan,
        permissions: Permissions,
    },
    Truncate {
        parent: NodeHandle,
        name: GuestName,
        size: u64,
    },
}

/// Stable values emitted only after a successful transaction publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WatchEvent {
    Created { path: GuestPathBytes, directory: bool },
    Deleted { path: GuestPathBytes, directory: bool },
    MovedFrom { path: GuestPathBytes, cookie: u32 },
    MovedTo { path: GuestPathBytes, cookie: u32 },
    Attributes { path: GuestPathBytes },
    Modified { path: GuestPathBytes },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostError {
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    DirectoryNotEmpty,
    PermissionDenied,
    ReadOnly,
    CrossMount,
    Busy,
    ResourceLimit,
    Unsupported,
    Io,
    Race,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Resolve(ResolveError),
    Host(HostError),
    InvalidArgument,
    InvalidName,
    ReadOnly,
    PermissionDenied,
    OperationNotPermitted,
    CrossMount,
    AlreadyExists,
    NotFound,
    NotDirectory,
    IsDirectory,
    DirectoryNotEmpty,
    Busy,
}

impl From<ResolveError> for Error {
    fn from(value: ResolveError) -> Self {
        Self::Resolve(value)
    }
}

impl From<HostError> for Error {
    fn from(value: HostError) -> Self {
        match value {
            HostError::NotFound => Self::NotFound,
            HostError::AlreadyExists => Self::AlreadyExists,
            HostError::NotDirectory => Self::NotDirectory,
            HostError::IsDirectory => Self::IsDirectory,
            HostError::DirectoryNotEmpty => Self::DirectoryNotEmpty,
            HostError::PermissionDenied => Self::PermissionDenied,
            HostError::ReadOnly => Self::ReadOnly,
            HostError::CrossMount => Self::CrossMount,
            HostError::Busy => Self::Busy,
            other => Self::Host(other),
        }
    }
}

impl Error {
    pub(crate) const fn from_access(error: AccessError) -> Self {
        match error {
            AccessError::InvalidAccess => Self::InvalidArgument,
            AccessError::PermissionDenied => Self::PermissionDenied,
            AccessError::OperationNotPermitted => Self::OperationNotPermitted,
        }
    }
}

/// Host mechanism consumed by the mutation policy.
///
/// `stage` must not publish. `commit` is the sole namespace publication point;
/// failure leaves no visible mutation. `rollback` is idempotent.
pub trait VfsMutationHost: crate::VfsHost {
    fn metadata_node(&self, node: NodeHandle) -> Result<Metadata, HostError>;

    fn metadata_at(&self, parent: NodeHandle, name: &GuestName, nofollow: bool) -> Result<Option<Metadata>, HostError>;

    fn begin(&self, parents: &[PinnedParent<'_>]) -> Result<VfsTransaction, HostError>;

    fn stage(&self, transaction: VfsTransaction, action: &Action) -> Result<(), HostError>;

    fn commit(&self, transaction: VfsTransaction) -> Result<(), HostError>;

    fn rollback(&self, transaction: VfsTransaction);
}

pub(crate) struct TransactionGuard<'host, H: VfsMutationHost> {
    host: &'host H,
    transaction: VfsTransaction,
    committed: bool,
}

impl<'host, H: VfsMutationHost> TransactionGuard<'host, H> {
    pub(crate) const fn new(host: &'host H, transaction: VfsTransaction) -> Self {
        Self {
            host,
            transaction,
            committed: false,
        }
    }

    pub(crate) fn mark_committed(&mut self) {
        self.committed = true;
    }
}

impl<H: VfsMutationHost> Drop for TransactionGuard<'_, H> {
    fn drop(&mut self) {
        if !self.committed {
            self.host.rollback(self.transaction);
        }
    }
}
