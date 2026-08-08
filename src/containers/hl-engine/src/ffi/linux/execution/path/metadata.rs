use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hl_descriptor::OfdMetadata;
use hl_linux::AccessPlan;
use hl_linux::Errno;
use hl_runtime::XattrName;
use hl_runtime::{
    FileIdentity, FileKind, FileMetadata, FileTimestamp, FilesystemStats, GuestPath, Permissions,
    PreparedXattrMutation, ResolvedMetadata, ResolvedPathLease, RuntimePathError, RuntimeXattrMutation,
};

use super::{HostError, native};

#[path = "metadata_xattr.rs"]
mod xattr;

use xattr::{XattrTarget, XattrTransaction};

/// Host and guest names of a resolved node.
#[derive(Debug)]
struct NodeNames {
    path: PathBuf,
    guest: GuestPath,
}

/// How a resolved node names its host object.
///
/// The anchored form keeps the pinned parent and the leaf name the walk already
/// produced, so metadata is one descriptor-relative call instead of a rendered
/// path the kernel would have to walk again.
enum Location {
    Rendered(NodeNames),
    Anchored {
        parent: super::overlay_lease::ParentLease,
        name: CString,
        source: std::sync::Arc<super::source::OrdinaryContext>,
        rendered: std::sync::OnceLock<NodeNames>,
    },
}

pub(super) struct Node {
    location: Location,
    ownership: std::sync::Arc<Registry>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.location {
            Location::Rendered(rendered) => formatter.debug_struct("Node").field("path", &rendered.path).finish(),
            Location::Anchored { name, .. } => formatter.debug_struct("Node").field("name", name).finish(),
        }
    }
}

impl Node {
    pub(super) fn new(path: PathBuf, guest: GuestPath, ownership: std::sync::Arc<Registry>) -> Self {
        Self {
            location: Location::Rendered(NodeNames { path, guest }),
            ownership,
        }
    }

    pub(super) fn anchored(
        parent: super::overlay_lease::ParentLease,
        name: CString,
        source: std::sync::Arc<super::source::OrdinaryContext>,
        ownership: std::sync::Arc<Registry>,
    ) -> Self {
        Self {
            location: Location::Anchored {
                parent,
                name,
                source,
                rendered: std::sync::OnceLock::new(),
            },
            ownership,
        }
    }

    /// Materializes the host path only for the operations that cannot be
    /// expressed against the pinned parent.
    fn rendered(&self) -> Result<&NodeNames, RuntimePathError> {
        match &self.location {
            Location::Rendered(rendered) => Ok(rendered),
            Location::Anchored {
                parent,
                name,
                source,
                rendered,
            } => {
                if let Some(value) = rendered.get() {
                    return Ok(value);
                }
                let path = super::pin::Host::path(parent, name)?;
                let guest = source.guest_path(&path)?;
                let _ = rendered.set(NodeNames { path, guest });
                rendered.get().ok_or(RuntimePathError::Invalid)
            }
        }
    }

    fn path(&self) -> Result<&Path, RuntimePathError> {
        self.rendered().map(|rendered| rendered.path.as_path())
    }
}

impl ResolvedPathLease for Node {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        let mut value = match &self.location {
            Location::Anchored { parent, name, .. } => HostMetadata::anchored(parent, name)?,
            Location::Rendered(rendered) => HostMetadata::path(&rendered.path)?,
        };
        self.ownership.project(&mut value);
        Ok(value)
    }
    fn resolved_metadata(&self) -> Result<ResolvedMetadata, RuntimePathError> {
        let mut value = match &self.location {
            Location::Anchored { parent, name, .. } => HostMetadata::anchored_resolved(parent, name)?,
            Location::Rendered(rendered) => HostMetadata::path_resolved(&rendered.path)?,
        };
        self.ownership.project(&mut value.file);
        Ok(value)
    }
    fn filesystem(&self) -> Result<FilesystemStats, RuntimePathError> {
        let rendered = self.rendered()?;
        super::filesystem::HostFilesystem::read(&rendered.path, &rendered.guest)
    }
    fn truncate(&self, size: u64) -> Result<(), RuntimePathError> {
        std::fs::OpenOptions::new()
            .write(true)
            .open(self.path()?)
            .map_err(HostError::map)?
            .set_len(size)
            .map_err(HostError::map)
    }
    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        std::fs::read_link(self.path()?)
            .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
            .map_err(HostError::map)
    }
    fn access(&self, plan: &AccessPlan) -> Result<(), RuntimePathError> {
        let path = self.path()?;
        let metadata = if plan.operand.nofollow {
            std::fs::symlink_metadata(path)
        } else {
            std::fs::metadata(path)
        }
        .map_err(HostError::map)?;
        let bits = plan.access.bits();
        if bits & 1 != 0 && metadata.mode() & 0o111 == 0 {
            return Err(RuntimePathError::Access);
        }
        if bits & 2 != 0 && metadata.mode() & 0o222 == 0 {
            return Err(RuntimePathError::Access);
        }
        if bits & 4 != 0 && metadata.mode() & 0o444 == 0 {
            return Err(RuntimePathError::Access);
        }
        Ok(())
    }
    fn xattr_get(&self, name: &XattrName) -> Result<Vec<u8>, Errno> {
        XattrTarget::Path(self.xattr_path()?).get(name)
    }
    fn xattr_list(&self) -> Result<Vec<u8>, Errno> {
        XattrTarget::Path(self.xattr_path()?).list()
    }
    fn prepare_xattr(&self, mutation: RuntimeXattrMutation) -> Result<Box<dyn PreparedXattrMutation>, Errno> {
        XattrTransaction::prepare(XattrTarget::Path(self.xattr_path()?), mutation)
    }
}

impl Node {
    fn xattr_path(&self) -> Result<PathBuf, Errno> {
        self.path().map(Path::to_path_buf).map_err(RuntimePathError::errno)
    }
}

#[derive(Debug)]
pub(super) struct DescriptorNode {
    lease: hl_descriptor::OperationLease,
    filesystem: Option<FilesystemStats>,
    ownership: std::sync::Arc<Registry>,
}

impl DescriptorNode {
    pub(super) fn new(
        lease: hl_descriptor::OperationLease,
        filesystem: Option<FilesystemStats>,
        ownership: std::sync::Arc<Registry>,
    ) -> Self {
        Self {
            lease,
            filesystem,
            ownership,
        }
    }
}

impl ResolvedPathLease for DescriptorNode {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        let mut raw = self.lease.metadata().map_err(|_| RuntimePathError::BadDescriptor)?;
        self.ownership.project_ofd(&mut raw);
        HostMetadata::ofd(raw)
    }
    fn resolved_metadata(&self) -> Result<ResolvedMetadata, RuntimePathError> {
        let mut raw = self.lease.metadata().map_err(|_| RuntimePathError::BadDescriptor)?;
        self.ownership.project_ofd(&mut raw);
        let file = HostMetadata::ofd(raw)?;
        let native = self
            .lease
            .domain_extension()
            .and_then(|value| value.downcast_ref::<super::NativeFile>());
        let Some(native) = native else {
            return Ok(ResolvedMetadata::new(file));
        };
        let host = native.xattr_file().map_err(|_| RuntimePathError::BadDescriptor)?;
        HostMetadata::file_resolved(&host, file)
    }
    fn filesystem(&self) -> Result<FilesystemStats, RuntimePathError> {
        self.filesystem.ok_or(RuntimePathError::Unsupported)
    }
    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        self.lease
            .domain_extension()
            .and_then(|value| value.downcast_ref::<super::NativeFile>())
            .ok_or(RuntimePathError::Invalid)?
            .read_link()
    }
    fn access(&self, _: &AccessPlan) -> Result<(), RuntimePathError> {
        Ok(())
    }
    fn xattr_get(&self, name: &XattrName) -> Result<Vec<u8>, Errno> {
        self.xattr_target()?.get(name)
    }
    fn xattr_list(&self) -> Result<Vec<u8>, Errno> {
        self.xattr_target()?.list()
    }
    fn prepare_xattr(&self, mutation: RuntimeXattrMutation) -> Result<Box<dyn PreparedXattrMutation>, Errno> {
        XattrTransaction::prepare(self.xattr_target()?, mutation)
    }
}

impl DescriptorNode {
    fn xattr_target(&self) -> Result<XattrTarget, Errno> {
        self.lease
            .domain_extension()
            .and_then(|value| value.downcast_ref::<super::NativeFile>())
            .ok_or(Errno::EBADF)?
            .xattr_file()
            .map(XattrTarget::File)
            .map_err(|_| Errno::EBADF)
    }
}

pub(super) struct HostMetadata;

/// Guest ownership of every inode this container created or chowned, keyed by host device and
/// inode. Entries hold no descriptor, so the table costs memory rather than a per-inode fd; the
/// creation paths that free an inode call [`Registry::forget`] so a recycled inode number cannot
/// inherit a stale owner.
#[derive(Debug, Default)]
pub(super) struct Registry(Mutex<BTreeMap<(u64, u64), Ownership>>);

#[derive(Debug, Clone, Copy)]
struct Ownership {
    user: u32,
    group: u32,
}

impl Registry {
    pub(super) fn set(&self, descriptor: RawFd, user: u32, group: u32) -> Result<(), RuntimePathError> {
        let status = Self::stat(descriptor)?;
        self.insert(status.st_dev, status.st_ino, user, group);
        Ok(())
    }

    fn insert(&self, device: u64, inode: u64, user: u32, group: u32) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((device, inode), Ownership { user, group });
    }

    fn lookup(&self, device: u64, inode: u64) -> Option<Ownership> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(device, inode))
            .copied()
    }

    /// Drops the record for an inode whose last link is going away, so the number is safe to reuse.
    pub(super) fn forget(&self, device: u64, inode: u64) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(device, inode));
    }

    fn stat(descriptor: RawFd) -> Result<libc::stat, RuntimePathError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the descriptor stays live and fstat writes status without retaining either.
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } != 0 {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: a successful fstat initialized the complete value.
        Ok(unsafe { status.assume_init() })
    }

    /// Records the guest owner of a just-created entry, following Linux `inode_init_owner`: the
    /// creator's filesystem uid owns it, and a setgid parent lends its own group instead of the
    /// creator's. Hard links are not creations — they share the source inode's existing record.
    pub(super) fn record_created(&self, parent: RawFd, name: &std::ffi::CStr, user: u32, group: u32) {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent and name stay live and fstatat writes status without retaining them.
        if unsafe { libc::fstatat(parent, name.as_ptr(), status.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return;
        }
        // SAFETY: a successful fstatat initialized the complete value.
        let status = unsafe { status.assume_init() };
        self.record(parent, &status, user, group);
    }

    /// The anonymous form, for an `O_TMPFILE` inode that has no name to stat through.
    pub(super) fn record_created_fd(&self, parent: RawFd, child: RawFd, user: u32, group: u32) {
        if let Ok(status) = Self::stat(child) {
            self.record(parent, &status, user, group);
        }
    }

    fn record(&self, parent: RawFd, status: &libc::stat, user: u32, group: u32) {
        let group = self.inherited_group(parent).unwrap_or(group);
        self.insert(status.st_dev, status.st_ino, user, group);
    }

    /// The group a setgid parent lends its children, in guest terms.
    fn inherited_group(&self, parent: RawFd) -> Option<u32> {
        let directory = Self::stat(parent).ok()?;
        if directory.st_mode & libc::S_ISGID == 0 {
            return None;
        }
        Some(
            self.lookup(directory.st_dev, directory.st_ino)
                .map_or(directory.st_gid, |owner| owner.group),
        )
    }

    pub(super) fn project(&self, metadata: &mut FileMetadata) {
        if let Some(owner) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(metadata.identity.device, metadata.identity.inode))
        {
            metadata.user = owner.user;
            metadata.group = owner.group;
        }
    }

    pub(super) fn project_ofd(&self, metadata: &mut OfdMetadata) {
        if let Some(owner) = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(metadata.device, metadata.inode))
        {
            metadata.user = owner.user;
            metadata.group = owner.group;
        }
    }
}

impl HostMetadata {
    pub(super) fn path(path: &Path) -> Result<FileMetadata, RuntimePathError> {
        let value = std::fs::symlink_metadata(path).map_err(HostError::map)?;
        Self::value(&value)
    }

    /// Stats the leaf through the pinned parent, which also selects the layer
    /// that owns it, so no host path is rendered and no walk is repeated.
    pub(super) fn anchored(
        parent: &super::overlay_lease::ParentLease,
        name: &CString,
    ) -> Result<FileMetadata, RuntimePathError> {
        let (_, status) = super::pin::Host::visible_status(parent, name)?;
        Self::status(&status)
    }

    pub(super) fn anchored_resolved(
        parent: &super::overlay_lease::ParentLease,
        name: &CString,
    ) -> Result<ResolvedMetadata, RuntimePathError> {
        let (directory, status) = super::pin::Host::visible_status(parent, name)?;
        let file = Self::status(&status)?;
        let (birth, mount) = Self::status_extensions(directory.as_raw_fd(), name, &status);
        Ok(ResolvedMetadata { birth, mount, file })
    }

    pub(super) fn file(file: &File) -> Result<FileMetadata, RuntimePathError> {
        let value = file.metadata().map_err(HostError::map)?;
        Self::value(&value)
    }

    pub(super) fn path_resolved(path: &Path) -> Result<ResolvedMetadata, RuntimePathError> {
        let value = std::fs::symlink_metadata(path).map_err(HostError::map)?;
        let file = Self::value(&value)?;
        let (birth, mount) = Self::path_extensions(path, &value);
        Ok(ResolvedMetadata { birth, mount, file })
    }

    pub(super) fn file_resolved(file: &File, metadata: FileMetadata) -> Result<ResolvedMetadata, RuntimePathError> {
        let value = file.metadata().map_err(HostError::map)?;
        let (birth, mount) = Self::file_extensions(file, &value);
        Ok(ResolvedMetadata {
            birth,
            mount,
            file: metadata,
        })
    }

    #[cfg(target_os = "macos")]
    fn path_extensions(_path: &Path, value: &std::fs::Metadata) -> (Option<FileTimestamp>, Option<u64>) {
        use std::os::macos::fs::MetadataExt;

        (
            Some(Self::time(value.st_birthtime(), value.st_birthtime_nsec())),
            Some(value.dev()),
        )
    }

    #[cfg(target_os = "macos")]
    fn file_extensions(_file: &File, value: &std::fs::Metadata) -> (Option<FileTimestamp>, Option<u64>) {
        Self::path_extensions(Path::new(""), value)
    }

    #[cfg(target_os = "linux")]
    fn path_extensions(path: &Path, _value: &std::fs::Metadata) -> (Option<FileTimestamp>, Option<u64>) {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return (None, None);
        };
        Self::linux_extensions(libc::AT_FDCWD, path.as_ptr(), libc::AT_SYMLINK_NOFOLLOW)
    }

    #[cfg(target_os = "linux")]
    fn file_extensions(file: &File, _value: &std::fs::Metadata) -> (Option<FileTimestamp>, Option<u64>) {
        let empty = c"";
        Self::linux_extensions(file.as_raw_fd(), empty.as_ptr(), libc::AT_EMPTY_PATH)
    }

    #[cfg(target_os = "linux")]
    fn linux_extensions(
        directory: RawFd,
        path: *const libc::c_char,
        flags: libc::c_int,
    ) -> (Option<FileTimestamp>, Option<u64>) {
        let mut value = std::mem::MaybeUninit::<libc::statx>::zeroed();
        // SAFETY: path is a live NUL-terminated string, value is writable, and
        // statx retains neither pointer nor descriptor.
        let result = unsafe {
            libc::syscall(
                libc::SYS_statx,
                directory,
                path,
                flags,
                libc::STATX_BTIME | libc::STATX_MNT_ID,
                value.as_mut_ptr(),
            )
        };
        if result != 0 {
            return (None, None);
        }
        // SAFETY: successful statx initialized the complete output structure.
        let value = unsafe { value.assume_init() };
        let birth = (value.stx_mask & libc::STATX_BTIME != 0).then_some(FileTimestamp {
            seconds: value.stx_btime.tv_sec,
            nanoseconds: value.stx_btime.tv_nsec,
        });
        let mount = (value.stx_mask & libc::STATX_MNT_ID != 0).then_some(value.stx_mnt_id);
        (birth, mount)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn path_extensions(_path: &Path, _value: &std::fs::Metadata) -> (Option<FileTimestamp>, Option<u64>) {
        (None, None)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn file_extensions(_file: &File, _value: &std::fs::Metadata) -> (Option<FileTimestamp>, Option<u64>) {
        (None, None)
    }

    #[cfg(target_os = "linux")]
    fn status_extensions(
        directory: RawFd,
        name: &CString,
        _status: &libc::stat,
    ) -> (Option<FileTimestamp>, Option<u64>) {
        Self::linux_extensions(directory, name.as_ptr(), libc::AT_SYMLINK_NOFOLLOW)
    }

    #[cfg(target_os = "macos")]
    fn status_extensions(
        _directory: RawFd,
        _name: &CString,
        status: &libc::stat,
    ) -> (Option<FileTimestamp>, Option<u64>) {
        (
            Some(Self::time(status.st_birthtime, status.st_birthtime_nsec)),
            Some(status.st_dev as u64),
        )
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn status_extensions(
        _directory: RawFd,
        _name: &CString,
        _status: &libc::stat,
    ) -> (Option<FileTimestamp>, Option<u64>) {
        (None, None)
    }

    /// Builds the guest-facing record from a raw `fstatat` result. The casts
    /// are identities on Linux but narrow or widen on macOS, whose `stat` uses
    /// different field widths.
    #[allow(clippy::unnecessary_cast)]
    fn status(value: &libc::stat) -> Result<FileMetadata, RuntimePathError> {
        let kind = Self::kind(value.st_mode as u32)?;
        Ok(FileMetadata {
            identity: FileIdentity {
                device: value.st_dev as u64,
                inode: value.st_ino as u64,
            },
            kind,
            permissions: Permissions::from_bits((value.st_mode & 0o7777) as u16),
            links: value.st_nlink as u64,
            user: value.st_uid,
            group: value.st_gid,
            special_device: value.st_rdev as u64,
            size: value.st_size as u64,
            blocks_512: value.st_blocks as u64,
            accessed: Self::time(value.st_atime, value.st_atime_nsec as i64),
            modified: Self::time(value.st_mtime, value.st_mtime_nsec as i64),
            changed: Self::time(value.st_ctime, value.st_ctime_nsec as i64),
        })
    }

    fn kind(mode: u32) -> Result<FileKind, RuntimePathError> {
        match mode & native::mode::TYPE_MASK {
            native::mode::FIFO => Ok(FileKind::Fifo),
            native::mode::CHARACTER => Ok(FileKind::Character),
            native::mode::DIRECTORY => Ok(FileKind::Directory),
            native::mode::BLOCK => Ok(FileKind::Block),
            native::mode::REGULAR => Ok(FileKind::Regular),
            native::mode::SYMLINK => Ok(FileKind::Symlink),
            native::mode::SOCKET => Ok(FileKind::Socket),
            _ => Err(RuntimePathError::Invalid),
        }
    }

    fn value(value: &std::fs::Metadata) -> Result<FileMetadata, RuntimePathError> {
        let kind = Self::kind(value.mode())?;
        Ok(FileMetadata {
            identity: FileIdentity {
                device: value.dev(),
                inode: value.ino(),
            },
            kind,
            permissions: Permissions::from_bits((value.mode() & 0o7777) as u16),
            links: value.nlink(),
            user: value.uid(),
            group: value.gid(),
            special_device: value.rdev(),
            size: value.size(),
            blocks_512: value.blocks(),
            accessed: Self::time(value.atime(), value.atime_nsec()),
            modified: Self::time(value.mtime(), value.mtime_nsec()),
            changed: Self::time(value.ctime(), value.ctime_nsec()),
        })
    }

    pub(super) fn ofd(value: OfdMetadata) -> Result<FileMetadata, RuntimePathError> {
        let kind = match value.kind {
            1 => FileKind::Fifo,
            2 => FileKind::Character,
            4 => FileKind::Directory,
            6 => FileKind::Block,
            8 => FileKind::Regular,
            10 => FileKind::Symlink,
            12 => FileKind::Socket,
            _ => return Err(RuntimePathError::Invalid),
        };
        Ok(FileMetadata {
            identity: FileIdentity {
                device: value.device,
                inode: value.inode,
            },
            kind,
            permissions: Permissions::from_bits(value.permissions),
            links: value.links,
            user: value.user,
            group: value.group,
            special_device: value.special_device,
            size: value.size,
            blocks_512: value.blocks_512,
            accessed: FileTimestamp {
                seconds: value.accessed.seconds,
                nanoseconds: value.accessed.nanoseconds,
            },
            modified: FileTimestamp {
                seconds: value.modified.seconds,
                nanoseconds: value.modified.nanoseconds,
            },
            changed: FileTimestamp {
                seconds: value.changed.seconds,
                nanoseconds: value.changed.nanoseconds,
            },
        })
    }

    fn time(seconds: i64, nanoseconds: i64) -> FileTimestamp {
        FileTimestamp {
            seconds,
            nanoseconds: u32::try_from(nanoseconds).unwrap_or(0),
        }
    }
}
