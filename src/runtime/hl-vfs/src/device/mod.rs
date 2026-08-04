use crate::{Kind, MountSourceId, Permissions};

mod builtin;
mod description;
mod registry;

pub use builtin::{BUILTIN_DEVICES, BuiltinDevice};
pub use description::{BuiltinDescription, Entropy};
pub use registry::{Registration, Registry, Snapshot};

#[cfg(test)]
mod test;

/// Linux device number split into its architecture-independent components.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Id {
    pub major: u32,
    pub minor: u32,
}

impl Id {
    #[must_use]
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    #[must_use]
    pub const fn linux_encoded(self) -> u64 {
        ((self.major as u64 & 0xfff) << 8)
            | (self.minor as u64 & 0xff)
            | ((self.minor as u64 & !0xff) << 12)
            | ((self.major as u64 & !0xfff) << 32)
    }

    #[must_use]
    pub const fn from_linux_encoded(value: u64) -> Self {
        Self {
            major: (((value >> 8) & 0xfff) | ((value >> 32) & !0xfff)) as u32,
            minor: ((value & 0xff) | ((value >> 12) & !0xff)) as u32,
        }
    }
}

/// Generic behavior visible to VFS consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    Null,
    Zero,
    Full,
    Random,
    Urandom,
    Terminal,
    OpaqueCharacter,
    OpaqueBlock,
}

impl NodeKind {
    #[must_use]
    pub const fn file_kind(self) -> Kind {
        match self {
            Self::OpaqueBlock => Kind::Block,
            Self::Null
            | Self::Zero
            | Self::Full
            | Self::Random
            | Self::Urandom
            | Self::Terminal
            | Self::OpaqueCharacter => Kind::Character,
        }
    }
}

/// Namespace containing a device projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    Root,
    Mounted(MountSourceId),
}

/// Stable provider identity. It deliberately contains no native descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProjectedObjectId(u64);

impl ProjectedObjectId {
    pub fn new(value: u64) -> Result<Self, Error> {
        if value == 0 {
            Err(Error::InvalidObject)
        } else {
            Ok(Self(value))
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Per-registration identity preventing stale lookup after slot reuse.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NodeId {
    pub slot: u16,
    pub generation: u64,
}

/// Metadata and provider binding published atomically in the namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub id: NodeId,
    pub path: crate::GuestPathBytes,
    pub scope: Scope,
    pub device: Id,
    pub kind: NodeKind,
    pub permissions: Permissions,
    pub user: u32,
    pub group: u32,
    pub object: ProjectedObjectId,
}

/// Domain-neutral open intent supplied to a host/provider adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenCapability {
    Read,
    Write,
    ReadWrite,
    Path,
}

/// Opaque opened-object token, not a native file descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ObjectToken(pub u64);

pub trait Host {
    type Error;

    fn open(&self, object: ProjectedObjectId, capability: OpenCapability) -> Result<ObjectToken, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidPath,
    RelativePath,
    InvalidObject,
    Duplicate,
    NotFound,
    Capacity,
    Stale,
    InvalidSnapshot,
}
