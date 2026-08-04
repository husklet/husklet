use crate::{GuestName, GuestPathBytes, ResolveHostError, VfsHost};

/// One layer in overlay precedence order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Layer {
    Upper,
    Lower(u8),
}

/// Host-neutral filesystem node kind used by overlay decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    Directory,
    Regular,
    Symlink,
    Other,
}

/// Metadata that copy-up must preserve before publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodeMetadata {
    pub permissions: u32,
    pub owner: u32,
    pub group: u32,
    pub size: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: u32,
}

/// Result of probing one path in one layer without following its final link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayerEntry {
    Absent,
    Whiteout,
    Node {
        kind: NodeKind,
        metadata: NodeMetadata,
        opaque: bool,
    },
}

/// One ordered child returned by a layer directory scan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    pub name: GuestName,
    pub kind: NodeKind,
    pub whiteout: bool,
}

/// Deterministically merged directory contents in layer-precedence order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MergedDirectory {
    pub entries: Vec<DirectoryEntry>,
}

/// Opaque identity of one staged host mutation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct MutationHandle(u64);

impl MutationHandle {
    #[must_use]
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Content operation selected by a copy-up plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyContent {
    None,
    Regular { size: u64 },
    Symlink,
}

/// Value-only copy-up transaction plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyUpPlan {
    pub path: GuestPathBytes,
    pub source: Layer,
    pub kind: NodeKind,
    pub metadata: NodeMetadata,
    pub content: CopyContent,
}

/// Value-only create transaction plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatePlan {
    pub path: GuestPathBytes,
    pub kind: NodeKind,
    pub metadata: NodeMetadata,
}

/// Visible overlay lookup result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Lookup {
    Absent {
        upper_path: GuestPathBytes,
    },
    Present {
        layer: Layer,
        kind: NodeKind,
        metadata: NodeMetadata,
    },
}

/// Overlay planning or transaction failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidPath,
    TooManyLayers,
    NotFound,
    NotDirectory,
    AlreadyExists,
    AlreadyUpper,
    ReadOnly,
    InvalidName,
    ResourceLimit,
    Host(ResolveHostError),
}

/// Narrow host capabilities consumed by overlay planning and publication.
///
/// Mutations are staged out of view. `commit_mutation` is the sole publication
/// point; rollback must discard every staged parent, metadata, and content
/// change.
pub trait Host: VfsHost {
    fn probe(&self, layer: Layer, path: &GuestPathBytes) -> Result<LayerEntry, ResolveHostError>;

    fn read_directory(&self, layer: Layer, path: &GuestPathBytes) -> Result<Vec<DirectoryEntry>, ResolveHostError>;

    fn begin_mutation(&self, path: &GuestPathBytes) -> Result<MutationHandle, ResolveHostError>;

    fn stage_parent_directories(&self, mutation: MutationHandle, path: &GuestPathBytes)
    -> Result<(), ResolveHostError>;

    fn stage_copy_up(&self, mutation: MutationHandle, plan: &CopyUpPlan) -> Result<(), ResolveHostError>;

    fn stage_create(&self, mutation: MutationHandle, plan: &CreatePlan) -> Result<(), ResolveHostError>;

    fn commit_mutation(&self, mutation: MutationHandle) -> Result<(), ResolveHostError>;

    fn rollback_mutation(&self, mutation: MutationHandle);
}
