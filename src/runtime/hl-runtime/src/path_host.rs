use std::fmt::Debug;
use std::sync::Arc;

use hl_descriptor::{DescriptionIdentity, DescriptorFlags, OpenFileDescription, OperationLease, StatusFlags};
use hl_linux::{AccessPlan, Errno, FsMutationPlan, OpenAbiPlan, PathOperand};
use hl_vfs::{
    AccessIdentity, FileMetadata, FileTimestamp, FilesystemStats, GuestPath, GuestPathBytes, OpenIntent, XattrFlags,
    XattrName,
};

/// Metadata fields that belong to a resolved mount rather than the inode's
/// portable `stat(2)` projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMetadata {
    pub file: FileMetadata,
    pub birth: Option<FileTimestamp>,
    pub mount: Option<u64>,
}

impl ResolvedMetadata {
    #[must_use]
    pub const fn new(file: FileMetadata) -> Self {
        Self {
            file,
            birth: None,
            mount: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutablePath {
    pub path: GuestPathBytes,
    pub nofollow: bool,
}

/// Generation-pinned base for one relative pathname operation.
///
/// Keeping the operation lease alive prevents close/reuse from changing the
/// meaning of a previously admitted directory descriptor.
#[derive(Debug)]
pub struct DirectoryBaseLease {
    descriptor: Option<OperationLease>,
    path: GuestPath,
    confines_root: bool,
}

impl DirectoryBaseLease {
    #[must_use]
    pub const fn root(path: GuestPath) -> Self {
        Self {
            descriptor: None,
            path,
            confines_root: false,
        }
    }

    #[must_use]
    pub const fn confined_root(path: GuestPath) -> Self {
        Self {
            descriptor: None,
            path,
            confines_root: true,
        }
    }

    #[must_use]
    pub const fn descriptor(lease: OperationLease, path: GuestPath) -> Self {
        Self {
            descriptor: Some(lease),
            path,
            confines_root: false,
        }
    }

    #[must_use]
    pub fn path(&self) -> &GuestPath {
        &self.path
    }

    #[must_use]
    pub fn descriptor_lease(&self) -> Option<&OperationLease> {
        self.descriptor.as_ref()
    }

    #[must_use]
    pub const fn confines_root(&self) -> bool {
        self.confines_root
    }

    #[must_use]
    pub fn resolve_constraints(&self) -> hl_vfs::ResolveConstraints {
        hl_vfs::ResolveConstraints {
            in_root: self.confines_root,
            ..hl_vfs::ResolveConstraints::default()
        }
    }
}

/// Opaque resolved-node capability retained across host operations.
pub trait ResolvedPathLease: Debug + Send {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError>;
    fn resolved_metadata(&self) -> Result<ResolvedMetadata, RuntimePathError> {
        self.metadata().map(ResolvedMetadata::new)
    }
    fn filesystem(&self) -> Result<FilesystemStats, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn truncate(&self, _size: u64) -> Result<(), RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError>;
    fn access(&self, plan: &AccessPlan) -> Result<(), RuntimePathError>;
    fn xattr_get(&self, _name: &XattrName) -> Result<Vec<u8>, Errno> {
        Err(Errno::ENOSYS)
    }
    fn xattr_list(&self) -> Result<Vec<u8>, Errno> {
        Err(Errno::ENOSYS)
    }
    fn prepare_xattr(&self, _mutation: RuntimeXattrMutation) -> Result<Box<dyn PreparedXattrMutation>, Errno> {
        Err(Errno::ENOSYS)
    }
    fn read_image(&self, _maximum: usize) -> Result<Vec<u8>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn executable_access(&self, _path: &ExecutablePath) -> Result<(), RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeXattrMutation {
    Set {
        name: XattrName,
        value: Vec<u8>,
        flags: XattrFlags,
    },
    Remove {
        name: XattrName,
    },
}

pub trait PreparedXattrMutation: Debug + Send {
    fn commit(&mut self) -> Result<(), Errno>;
    fn rollback(self: Box<Self>);
}

/// Prepared open whose namespace effects are not yet published.
pub trait PreparedPathOpen: Debug + Send {
    fn object(&self) -> Arc<dyn OpenFileDescription>;
    fn bind(&mut self, _identity: DescriptionIdentity) -> Result<(), RuntimePathError> {
        Ok(())
    }
    fn commit(&mut self) -> Result<(), RuntimePathError>;
    fn rollback(self: Box<Self>);
}

pub trait PreparedPathMutation: Debug + Send {
    fn commit(&mut self) -> Result<(), RuntimePathError>;
    fn rollback(self: Box<Self>);
}

/// Runtime consumer port composing descriptor identity with VFS resolution.
pub trait RuntimePathHost: Send + Sync {
    fn root_base(&self) -> Result<DirectoryBaseLease, RuntimePathError>;
    fn working_base(&self, path: GuestPath) -> Result<DirectoryBaseLease, RuntimePathError> {
        Ok(DirectoryBaseLease::root(path))
    }
    fn descriptor_base(&self, lease: OperationLease) -> Result<DirectoryBaseLease, RuntimePathError>;
    fn directory_path(
        &self,
        _base: &DirectoryBaseLease,
        _operand: &PathOperand,
    ) -> Result<GuestPath, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn descriptor_node(&self, _lease: OperationLease) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn filesystem(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<FilesystemStats, RuntimePathError> {
        self.resolve(base, operand)?.filesystem()
    }
    fn resolve(
        &self,
        base: &DirectoryBaseLease,
        operand: &PathOperand,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError>;
    fn resolve_executable(
        &self,
        _base: &DirectoryBaseLease,
        _path: &ExecutablePath,
    ) -> Result<Box<dyn ResolvedPathLease>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn prepare_open(
        &self,
        base: &DirectoryBaseLease,
        plan: &OpenAbiPlan,
    ) -> Result<Box<dyn PreparedPathOpen>, RuntimePathError>;
    /// Whether an admitted open can wait for a guest peer or a blocking
    /// provider response. Implementations must return `true` unless they can
    /// prove that `prepare_open` completes without such a wait.
    fn open_may_block(
        &self,
        _base: &DirectoryBaseLease,
        _plan: &OpenAbiPlan,
    ) -> Result<bool, RuntimePathError> {
        Ok(true)
    }
    fn prepare_mutation(
        &self,
        _bases: &[DirectoryBaseLease],
        _plan: &FsMutationPlan,
        _identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn prepare_inode_link(
        &self,
        _source: OperationLease,
        _target_base: &DirectoryBaseLease,
        _target: &PathOperand,
        _identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn prepare_descriptor_chmod(
        &self,
        _source: hl_descriptor::OperationLease,
        _mode: u32,
        _identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn prepare_descriptor_chown(
        &self,
        _source: hl_descriptor::OperationLease,
        _user: Option<u32>,
        _group: Option<u32>,
        _identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn prepare_descriptor_times(
        &self,
        _source: hl_descriptor::OperationLease,
        _times: [hl_linux::TimestampChange; 2],
        _identity: &AccessIdentity,
    ) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn access_identity(&self) -> Result<AccessIdentity, RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
    fn access_identity_for(&self, _effective: bool) -> Result<AccessIdentity, RuntimePathError> {
        self.access_identity()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimePathError {
    BadDescriptor,
    Invalid,
    NotFound,
    NoDevice,
    Exists,
    Access,
    OperationNotPermitted,
    NotDirectory,
    IsDirectory,
    DirectoryNotEmpty,
    Loop,
    CrossDevice,
    ReadOnly,
    Unsupported,
    TooLarge,
    FileTooLarge,
    NoSpace,
    Quota,
    NameTooLong,
    WouldBlock,
    TextBusy,
    Io,
}

impl RuntimePathError {
    #[must_use]
    pub const fn errno(self) -> Errno {
        match self {
            Self::BadDescriptor => Errno::EBADF,
            Self::Invalid => Errno::EINVAL,
            Self::NotFound => Errno::ENOENT,
            Self::NoDevice => Errno::ENXIO,
            Self::Exists => Errno::EEXIST,
            Self::Access => Errno::EACCES,
            Self::OperationNotPermitted => Errno::EPERM,
            Self::NotDirectory => Errno::ENOTDIR,
            Self::IsDirectory => Errno::EISDIR,
            Self::DirectoryNotEmpty => Errno::ENOTEMPTY,
            Self::Loop => Errno::ELOOP,
            Self::CrossDevice => Errno::EXDEV,
            Self::ReadOnly => Errno::EROFS,
            Self::Unsupported => Errno::ENOSYS,
            Self::TooLarge => Errno::E2BIG,
            Self::FileTooLarge => Errno::EFBIG,
            Self::NoSpace => Errno::ENOSPC,
            Self::Quota => Errno::EDQUOT,
            Self::NameTooLong => Errno::ENAMETOOLONG,
            Self::WouldBlock => Errno::EAGAIN,
            Self::TextBusy => Errno::from_raw(26),
            Self::Io => Errno::EIO,
        }
    }
}

pub(crate) struct OpenDescriptorState {
    pub status: StatusFlags,
    pub flags: DescriptorFlags,
}

impl OpenDescriptorState {
    pub(crate) fn from_plan(plan: &OpenAbiPlan) -> Self {
        let intent = plan.intent.bits();
        let access = if intent & OpenIntent::WRITE == 0 {
            0
        } else if intent & OpenIntent::READ == 0 {
            1
        } else {
            2
        };
        let mut bits = access;
        if intent & OpenIntent::PATH_ONLY != 0 {
            bits |= StatusFlags::PATH_ONLY;
        }
        if intent & OpenIntent::APPEND != 0 {
            bits |= StatusFlags::APPEND;
        }
        if plan.nonblocking {
            bits |= StatusFlags::NONBLOCKING;
        }
        Self {
            status: StatusFlags::from_bits(bits),
            flags: DescriptorFlags::from_bits(if plan.close_on_exec {
                DescriptorFlags::CLOSE_ON_EXEC
            } else {
                0
            }),
        }
    }
}
