use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hl_descriptor::OfdMetadata;
use hl_linux::AccessPlan;
use hl_linux::Errno;
use hl_runtime::{
    FileIdentity, FileKind, FileMetadata, FileTimestamp, FilesystemStats, GuestPath, Permissions,
    PreparedXattrMutation, ResolvedMetadata, ResolvedPathLease, RuntimePathError, RuntimeXattrMutation,
};
use hl_runtime::{XattrFlags, XattrName};

use super::{HostError, native};

#[derive(Debug)]
pub(super) struct Node {
    path: PathBuf,
    guest: GuestPath,
    ownership: std::sync::Arc<Registry>,
}

impl Node {
    pub(super) fn new(path: PathBuf, guest: GuestPath, ownership: std::sync::Arc<Registry>) -> Self {
        Self { path, guest, ownership }
    }
}

impl ResolvedPathLease for Node {
    fn metadata(&self) -> Result<FileMetadata, RuntimePathError> {
        let mut value = HostMetadata::path(&self.path)?;
        self.ownership.project(&mut value);
        Ok(value)
    }
    fn resolved_metadata(&self) -> Result<ResolvedMetadata, RuntimePathError> {
        let mut value = HostMetadata::path_resolved(&self.path)?;
        self.ownership.project(&mut value.file);
        Ok(value)
    }
    fn filesystem(&self) -> Result<FilesystemStats, RuntimePathError> {
        super::filesystem::HostFilesystem::read(&self.path, &self.guest)
    }
    fn truncate(&self, size: u64) -> Result<(), RuntimePathError> {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&self.path)
            .map_err(HostError::map)?
            .set_len(size)
            .map_err(HostError::map)
    }
    fn read_link(&self) -> Result<Vec<u8>, RuntimePathError> {
        std::fs::read_link(&self.path)
            .map(|path| path.as_os_str().as_encoded_bytes().to_vec())
            .map_err(HostError::map)
    }
    fn access(&self, plan: &AccessPlan) -> Result<(), RuntimePathError> {
        let metadata = if plan.operand.nofollow {
            std::fs::symlink_metadata(&self.path)
        } else {
            std::fs::metadata(&self.path)
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
        XattrTarget::Path(self.path.clone()).get(name)
    }
    fn xattr_list(&self) -> Result<Vec<u8>, Errno> {
        XattrTarget::Path(self.path.clone()).list()
    }
    fn prepare_xattr(&self, mutation: RuntimeXattrMutation) -> Result<Box<dyn PreparedXattrMutation>, Errno> {
        XattrTransaction::prepare(XattrTarget::Path(self.path.clone()), mutation)
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

#[derive(Debug)]
enum XattrTarget {
    Path(PathBuf),
    File(File),
}

impl XattrTarget {
    fn name(name: &XattrName) -> Result<CString, Errno> {
        CString::new(name.as_bytes()).map_err(|_| Errno::EINVAL)
    }

    fn errno() -> Errno {
        Errno::from_host(Errno::from_raw(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO),
        ))
    }

    fn get(&self, name: &XattrName) -> Result<Vec<u8>, Errno> {
        let name = Self::name(name)?;
        let read = |output: *mut libc::c_void, size| unsafe {
            match self {
                Self::Path(path) => {
                    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| Errno::EINVAL)?;
                    Ok(libc::lgetxattr(path.as_ptr(), name.as_ptr(), output, size))
                }
                Self::File(file) => Ok(libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), output, size)),
            }
        };
        let size = read(std::ptr::null_mut(), 0)?;
        if size < 0 {
            return Err(Self::errno());
        }
        let mut value = vec![0; size as usize];
        let count = read(value.as_mut_ptr().cast(), value.len())?;
        if count < 0 {
            return Err(Self::errno());
        }
        value.truncate(count as usize);
        Ok(value)
    }

    fn list(&self) -> Result<Vec<u8>, Errno> {
        let read = |output: *mut libc::c_char, size| unsafe {
            match self {
                Self::Path(path) => {
                    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| Errno::EINVAL)?;
                    Ok(libc::llistxattr(path.as_ptr(), output, size))
                }
                Self::File(file) => Ok(libc::flistxattr(file.as_raw_fd(), output, size)),
            }
        };
        let size = read(std::ptr::null_mut(), 0)?;
        if size < 0 {
            return Err(Self::errno());
        }
        let mut value = vec![0; size as usize];
        let count = read(value.as_mut_ptr().cast(), value.len())?;
        if count < 0 {
            return Err(Self::errno());
        }
        value.truncate(count as usize);
        Ok(value)
    }

    fn set(&self, name: &XattrName, value: &[u8], flags: i32) -> Result<(), Errno> {
        let name = Self::name(name)?;
        let result = unsafe {
            match self {
                Self::Path(path) => {
                    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| Errno::EINVAL)?;
                    libc::lsetxattr(path.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), flags)
                }
                Self::File(file) => libc::fsetxattr(
                    file.as_raw_fd(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    flags,
                ),
            }
        };
        if result < 0 { Err(Self::errno()) } else { Ok(()) }
    }

    fn remove(&self, name: &XattrName) -> Result<(), Errno> {
        let name = Self::name(name)?;
        let result = unsafe {
            match self {
                Self::Path(path) => {
                    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| Errno::EINVAL)?;
                    libc::lremovexattr(path.as_ptr(), name.as_ptr())
                }
                Self::File(file) => libc::fremovexattr(file.as_raw_fd(), name.as_ptr()),
            }
        };
        if result < 0 { Err(Self::errno()) } else { Ok(()) }
    }
}

#[derive(Debug)]
struct XattrTransaction {
    target: XattrTarget,
    mutation: RuntimeXattrMutation,
    previous: Option<Vec<u8>>,
    committed: bool,
}

impl XattrTransaction {
    fn prepare(target: XattrTarget, mutation: RuntimeXattrMutation) -> Result<Box<dyn PreparedXattrMutation>, Errno> {
        let name = match &mutation {
            RuntimeXattrMutation::Set { name, .. } | RuntimeXattrMutation::Remove { name } => name,
        };
        let previous = match target.get(name) {
            Ok(value) => Some(value),
            Err(error) if error == Errno::ENODATA => None,
            Err(error) => return Err(error),
        };
        match &mutation {
            RuntimeXattrMutation::Set {
                flags: XattrFlags::Create,
                ..
            } if previous.is_some() => return Err(Errno::EEXIST),
            RuntimeXattrMutation::Set {
                flags: XattrFlags::Replace,
                ..
            }
            | RuntimeXattrMutation::Remove { .. }
                if previous.is_none() =>
            {
                return Err(Errno::ENODATA);
            }
            _ => {}
        }
        Ok(Box::new(Self {
            target,
            mutation,
            previous,
            committed: false,
        }))
    }
}

impl PreparedXattrMutation for XattrTransaction {
    fn commit(&mut self) -> Result<(), Errno> {
        match &self.mutation {
            RuntimeXattrMutation::Set { name, value, flags } => {
                let flags = match flags {
                    XattrFlags::Upsert => 0,
                    XattrFlags::Create => libc::XATTR_CREATE,
                    XattrFlags::Replace => libc::XATTR_REPLACE,
                };
                self.target.set(name, value, flags)?;
            }
            RuntimeXattrMutation::Remove { name } => self.target.remove(name)?,
        }
        self.committed = true;
        Ok(())
    }

    fn rollback(self: Box<Self>) {
        if !self.committed {
            return;
        }
        let name = match &self.mutation {
            RuntimeXattrMutation::Set { name, .. } | RuntimeXattrMutation::Remove { name } => name,
        };
        match &self.previous {
            Some(value) => {
                let _ = self.target.set(name, value, 0);
            }
            None => {
                let _ = self.target.remove(name);
            }
        }
    }
}

pub(super) struct HostMetadata;

#[derive(Debug, Default)]
pub(super) struct Registry(Mutex<BTreeMap<(u64, u64), Ownership>>);

#[derive(Debug)]
struct Ownership {
    _pin: File,
    user: u32,
    group: u32,
}

impl Registry {
    pub(super) fn set(&self, descriptor: RawFd, user: u32, group: u32) -> Result<(), RuntimePathError> {
        // SAFETY: fcntl duplicates the live inode capability and returns unique ownership.
        let duplicated = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicated < 0 {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful fcntl returned one descriptor not owned elsewhere.
        let pin = unsafe { File::from_raw_fd(duplicated) };
        let value = pin.metadata().map_err(HostError::map)?;
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((value.dev(), value.ino()), Ownership { _pin: pin, user, group });
        Ok(())
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

    pub(super) fn file(file: &File) -> Result<FileMetadata, RuntimePathError> {
        let value = file.metadata().map_err(HostError::map)?;
        Self::value(&value)
    }

    fn path_resolved(path: &Path) -> Result<ResolvedMetadata, RuntimePathError> {
        let value = std::fs::symlink_metadata(path).map_err(HostError::map)?;
        let file = Self::value(&value)?;
        let (birth, mount) = Self::path_extensions(path, &value);
        Ok(ResolvedMetadata { birth, mount, file })
    }

    fn file_resolved(file: &File, metadata: FileMetadata) -> Result<ResolvedMetadata, RuntimePathError> {
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

    fn value(value: &std::fs::Metadata) -> Result<FileMetadata, RuntimePathError> {
        let kind = match value.mode() & native::mode::TYPE_MASK {
            native::mode::FIFO => FileKind::Fifo,
            native::mode::CHARACTER => FileKind::Character,
            native::mode::DIRECTORY => FileKind::Directory,
            native::mode::BLOCK => FileKind::Block,
            native::mode::REGULAR => FileKind::Regular,
            native::mode::SYMLINK => FileKind::Symlink,
            native::mode::SOCKET => FileKind::Socket,
            _ => return Err(RuntimePathError::Invalid),
        };
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

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    use super::{HostMetadata, Node, Registry};
    use hl_linux::{AccessPlan, PathOperand};
    use hl_runtime::{Access, GuestPath, GuestPathBytes, OpenDirectory, ResolvedPathLease};

    #[test]
    fn nofollow_access_checks_the_symlink_node() {
        let root = std::env::temp_dir().join(format!("hl-access-link-{}", std::process::id()));
        let link = root.join("dangling");
        std::fs::create_dir_all(&root).unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("missing", &link).unwrap();
        let node = Node::new(
            link.clone(),
            GuestPath::new("/dangling").unwrap(),
            std::sync::Arc::new(Registry::default()),
        );
        let plan = |nofollow| AccessPlan {
            operand: PathOperand {
                directory: OpenDirectory::default(),
                path: GuestPathBytes::new(b"/dangling").unwrap(),
                allow_empty: false,
                nofollow,
            },
            access: Access::from_bits(0).unwrap(),
            effective_ids: false,
        };
        assert!(node.access(&plan(false)).is_err());
        assert_eq!(node.access(&plan(true)), Ok(()));
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn resolved_metadata_extensions() {
        let path = std::env::temp_dir().join(format!("hl-statx-meta-{}", std::process::id()));
        let file = std::fs::File::create(&path).unwrap();
        let path_metadata = HostMetadata::path_resolved(&path).unwrap();
        let file_metadata = HostMetadata::file(&file).unwrap();
        let descriptor_metadata = HostMetadata::file_resolved(&file, file_metadata).unwrap();
        assert!(path_metadata.birth.is_some());
        assert_eq!(path_metadata.birth, descriptor_metadata.birth);
        assert!(path_metadata.mount.is_some());
        assert_eq!(path_metadata.mount, descriptor_metadata.mount);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ownership_hardlink() {
        let root = std::env::temp_dir().join(format!("hl-owner-{}", std::process::id()));
        let source = root.join("source");
        let alias = root.join("alias");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&source, b"data").unwrap();
        std::fs::hard_link(&source, &alias).unwrap();
        let file = std::fs::File::open(&source).unwrap();
        let host = file.metadata().unwrap();
        let user = host.uid().wrapping_add(10_000);
        let group = host.gid().wrapping_add(20_000);
        let registry = Registry::default();
        registry.set(file.as_raw_fd(), user, group).unwrap();

        for path in [&source, &alias] {
            let mut projected = HostMetadata::path(path).unwrap();
            registry.project(&mut projected);
            assert_eq!((projected.user, projected.group), (user, group));
            let physical = std::fs::metadata(path).unwrap();
            assert_eq!((physical.uid(), physical.gid()), (host.uid(), host.gid()));
        }

        drop(file);
        std::fs::remove_dir_all(root).unwrap();
    }
}
