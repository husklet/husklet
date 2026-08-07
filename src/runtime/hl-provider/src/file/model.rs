//! Provider-file public values and typed failures.

use std::fmt;

use hl_descriptor::{Readiness, StatusFlags};

use crate::{NamespaceError, ProviderError, RemoteId, TransferCapability};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Access {
    Read,
    Write,
    ReadWrite,
}
pub type FileAccess = Access;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Metadata {
    pub permissions: u32,
    pub user: u32,
    pub group: u32,
    pub size: u64,
    pub stable_object: u64,
}
pub type FileMetadata = Metadata;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub remote: RemoteId,
    pub service: u64,
    pub access: FileAccess,
    pub status: StatusFlags,
    pub offset: u64,
    pub readiness: Readiness,
    pub identity_namespace: u64,
    pub path: Vec<u8>,
}
pub type FileSnapshot = Snapshot;

#[derive(Debug)]
#[must_use = "a rebind value owns one restored remote-close obligation"]
pub struct Rebind {
    pub(crate) snapshot: FileSnapshot,
    pub(crate) capability: TransferCapability,
}
pub type FileRebind = Rebind;

impl FileRebind {
    #[must_use]
    pub fn snapshot(&self) -> &FileSnapshot {
        &self.snapshot
    }
}

/// Names the file operation whose reply failed to decode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplyOperation {
    Open,
    Read,
    Write,
    Seek,
    Stat,
    Poll,
    Close,
}

/// Names the argument a caller supplied out of range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallArgument {
    PathLength(usize),
    Whence(u8),
    IdentityNamespace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
    Provider(ProviderError),
    Namespace(NamespaceError),
    Linux(i32),
    MalformedReply(ReplyOperation),
    InvalidArgument(CallArgument),
    PayloadTooLarge { size: usize, maximum: usize },
    Retired,
}
pub type FileError = Error;

impl fmt::Display for FileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "provider file {self:?}")
    }
}

impl std::error::Error for FileError {}

impl From<ProviderError> for FileError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

impl From<NamespaceError> for FileError {
    fn from(error: NamespaceError) -> Self {
        Self::Namespace(error)
    }
}
