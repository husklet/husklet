#[cfg(target_os = "macos")]
use std::ffi::CStr;
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use hl_runtime::{
    GuestName, MountSourceId, NodeHandle, NodeKind, OpenIntent, ResolveHostError, RuntimePathError, VfsHost,
};

#[derive(Clone)]
pub(super) struct Host {
    root: Arc<File>,
    mounts: Arc<super::source::MountPaths>,
}

impl Host {
    pub(super) fn new(root: Arc<File>, mounts: Arc<super::source::MountPaths>) -> Self {
        Self { root, mounts }
    }

    fn duplicate(node: NodeHandle) -> Result<OwnedFd, ResolveHostError> {
        // SAFETY: fcntl observes a live resolver-owned descriptor and returns
        // an independently owned descriptor without retaining pointers.
        let descriptor = unsafe { libc::fcntl(Self::descriptor(node), libc::F_DUPFD_CLOEXEC, 0) };
        if descriptor < 0 {
            return Err(Self::error());
        }
        // SAFETY: successful F_DUPFD_CLOEXEC created one unowned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }

    pub(super) fn open(
        parent: &OwnedFd,
        name: &CString,
        intent: OpenIntent,
        mode: u32,
    ) -> Result<File, RuntimePathError> {
        let flags = Self::open_flags(intent)?;
        // SAFETY: parent and name remain live for the non-retaining openat;
        // success returns one new descriptor with unique ownership.
        let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags, mode as libc::mode_t) };
        if descriptor < 0 {
            return Err(super::HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful openat returned one descriptor not owned elsewhere.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    pub(super) fn path(parent: &OwnedFd, name: &CString) -> Result<PathBuf, RuntimePathError> {
        let directory = Self::descriptor_path(parent.as_raw_fd()).map_err(super::HostError::map)?;
        Ok(directory.join(OsStr::from_bytes(name.as_bytes())))
    }
}

impl VfsHost for Host {
    type ParentLease = OwnedFd;

    fn pin_root(&self) -> Result<NodeHandle, ResolveHostError> {
        let descriptor = self.root.as_raw_fd();
        // SAFETY: fcntl duplicates the live root descriptor and retains no pointer.
        let pin = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 0) };
        if pin < 0 {
            Err(Self::error())
        } else {
            Ok(Self::handle(pin))
        }
    }

    fn pin_mount(&self, source: MountSourceId) -> Result<NodeHandle, ResolveHostError> {
        let root = self.mounts.root(source).map_err(|error| match error {
            RuntimePathError::NotFound => ResolveHostError::NotFound,
            _ => ResolveHostError::Io,
        })?;
        let descriptor = root.as_raw_fd();
        // SAFETY: fcntl duplicates the live mount-root descriptor and retains no pointer.
        let pin = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 0) };
        if pin < 0 {
            Err(Self::error())
        } else {
            Ok(Self::handle(pin))
        }
    }

    fn inspect_child(
        &self,
        directory: NodeHandle,
        component: &GuestName,
    ) -> Result<(NodeHandle, NodeKind), ResolveHostError> {
        let name = CString::new(component.as_bytes()).map_err(|_| ResolveHostError::Io)?;
        let observed = Self::child_kind(Self::descriptor(directory), &name)?;
        let flags = Self::pin_flags(observed);
        // SAFETY: name is terminated and the pinned directory remains live for
        // this non-retaining relative open.
        let child = unsafe { libc::openat(Self::descriptor(directory), name.as_ptr(), flags) };
        if child < 0 {
            return Err(Self::error());
        }
        let kind = Self::metadata_kind(child);
        match kind {
            Ok(kind) => Ok((Self::handle(child), kind)),
            Err(error) => {
                // SAFETY: child is uniquely owned after successful openat.
                unsafe { libc::close(child) };
                Err(error)
            }
        }
    }

    fn read_link(&self, link: NodeHandle, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        Self::read_link(Self::descriptor(link), output)
    }

    fn crosses_mount(&self, directory: NodeHandle, child: NodeHandle) -> Result<bool, ResolveHostError> {
        Ok(Self::device(Self::descriptor(directory))? != Self::device(Self::descriptor(child))?)
    }

    fn duplicate_parent(&self, parent: NodeHandle) -> Result<Self::ParentLease, ResolveHostError> {
        Self::duplicate(parent)
    }

    fn close(&self, node: NodeHandle) {
        // SAFETY: each resolver pin is transferred once and closed once.
        unsafe { libc::close(Self::descriptor(node)) };
    }
}

impl Host {
    fn handle(descriptor: RawFd) -> NodeHandle {
        NodeHandle::from_raw(u64::try_from(descriptor).expect("descriptor is nonnegative") + 1)
    }

    fn descriptor(handle: NodeHandle) -> RawFd {
        i32::try_from(handle.raw() - 1).expect("native descriptor handle")
    }

    #[cfg(target_os = "linux")]
    fn pin_flags(_: NodeKind) -> i32 {
        libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC
    }

    #[cfg(target_os = "macos")]
    fn pin_flags(kind: NodeKind) -> i32 {
        libc::O_CLOEXEC
            | if kind == NodeKind::Symlink {
                libc::O_SYMLINK
            } else {
                libc::O_EVTONLY | libc::O_NOFOLLOW
            }
    }

    fn child_kind(directory: RawFd, name: &CString) -> Result<NodeKind, ResolveHostError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: name is terminated, directory is pinned, and fstatat initializes
        // status on success without retaining either pointer.
        if unsafe { libc::fstatat(directory, name.as_ptr(), status.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: successful fstatat initialized every field.
        Ok(Self::kind_from_mode(unsafe { status.assume_init() }.st_mode))
    }

    fn metadata_kind(descriptor: RawFd) -> Result<NodeKind, ResolveHostError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes status on success and retains no pointer.
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: the successful fstat initialized every field.
        let mode = unsafe { status.assume_init() }.st_mode;
        Ok(Self::kind_from_mode(mode))
    }

    fn device(descriptor: RawFd) -> Result<libc::dev_t, ResolveHostError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes status on success and retains no pointer.
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: successful fstat initialized every field.
        Ok(unsafe { status.assume_init() }.st_dev)
    }

    fn kind_from_mode(mode: libc::mode_t) -> NodeKind {
        match mode & libc::S_IFMT {
            libc::S_IFDIR => NodeKind::Directory,
            libc::S_IFREG => NodeKind::File,
            libc::S_IFLNK => NodeKind::Symlink,
            _ => NodeKind::Other,
        }
    }

    #[cfg(target_os = "linux")]
    fn read_link(descriptor: RawFd, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        let empty = c"";
        // SAFETY: output is writable for its length and empty requests the pinned
        // O_PATH link itself through Linux AT_EMPTY_PATH semantics.
        let count = unsafe { libc::readlinkat(descriptor, empty.as_ptr(), output.as_mut_ptr().cast(), output.len()) };
        usize::try_from(count).map_err(|_| Self::error())
    }

    #[cfg(target_os = "macos")]
    fn read_link(descriptor: RawFd, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        let mut path = vec![0_i8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes a terminated path into the bounded output.
        if unsafe { libc::fcntl(descriptor, libc::F_GETPATH, path.as_mut_ptr()) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: successful F_GETPATH produced a terminated C string.
        let bytes = unsafe { CStr::from_ptr(path.as_ptr()) }.to_bytes();
        let target = std::fs::read_link(Path::new(OsStr::from_bytes(bytes))).map_err(Self::map)?;
        let target = target.as_os_str().as_bytes();
        if target.len() > output.len() {
            return Err(ResolveHostError::ResourceLimit);
        }
        output[..target.len()].copy_from_slice(target);
        Ok(target.len())
    }

    fn error() -> ResolveHostError {
        Self::map(std::io::Error::last_os_error())
    }

    fn map(error: std::io::Error) -> ResolveHostError {
        match error.kind() {
            std::io::ErrorKind::NotFound => ResolveHostError::NotFound,
            std::io::ErrorKind::NotADirectory => ResolveHostError::NotDirectory,
            std::io::ErrorKind::PermissionDenied => ResolveHostError::PermissionDenied,
            _ => ResolveHostError::Io,
        }
    }

    fn open_flags(intent: OpenIntent) -> Result<i32, RuntimePathError> {
        let bits = intent.bits();
        let access = match (bits & OpenIntent::READ != 0, bits & OpenIntent::WRITE != 0) {
            (true, true) => libc::O_RDWR,
            (false, true) => libc::O_WRONLY,
            _ => libc::O_RDONLY,
        };
        // Host opens must never pin the engine scheduler if a name is swapped
        // to a FIFO after resolution. Guest blocking is implemented by the
        // named-FIFO domain, so the native descriptor is always acquired
        // nonblocking and revalidated after openat.
        let mut flags = access | libc::O_CLOEXEC | libc::O_NONBLOCK;
        if bits & OpenIntent::CREATE != 0 {
            flags |= libc::O_CREAT;
        }
        if bits & OpenIntent::EXCLUSIVE != 0 {
            flags |= libc::O_EXCL;
        }
        if bits & OpenIntent::TRUNCATE != 0 {
            flags |= libc::O_TRUNC;
        }
        if bits & OpenIntent::APPEND != 0 {
            flags |= libc::O_APPEND;
        }
        if bits & OpenIntent::DIRECTORY != 0 {
            flags |= libc::O_DIRECTORY;
        }
        if bits & OpenIntent::NOFOLLOW != 0 {
            flags |= libc::O_NOFOLLOW;
        }
        Self::platform_open_flags(flags, bits)
    }

    #[cfg(target_os = "linux")]
    fn descriptor_path(descriptor: RawFd) -> std::io::Result<PathBuf> {
        std::fs::read_link(format!("/proc/self/fd/{descriptor}"))
    }

    #[cfg(target_os = "macos")]
    fn descriptor_path(descriptor: RawFd) -> std::io::Result<PathBuf> {
        let mut path = vec![0_i8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes a terminated path into the bounded buffer.
        if unsafe { libc::fcntl(descriptor, libc::F_GETPATH, path.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful F_GETPATH produced a terminated C string.
        let bytes = unsafe { CStr::from_ptr(path.as_ptr()) }.to_bytes();
        Ok(PathBuf::from(OsStr::from_bytes(bytes)))
    }

    #[cfg(target_os = "linux")]
    fn platform_open_flags(mut flags: i32, bits: u32) -> Result<i32, RuntimePathError> {
        if bits & OpenIntent::PATH_ONLY != 0 {
            flags &= libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            flags |= libc::O_PATH;
        }
        if bits & OpenIntent::TEMPORARY != 0 && bits & OpenIntent::PATH_ONLY == 0 {
            flags |= libc::O_TMPFILE;
        }
        Ok(flags)
    }

    #[cfg(target_os = "macos")]
    fn platform_open_flags(mut flags: i32, bits: u32) -> Result<i32, RuntimePathError> {
        if bits & OpenIntent::TEMPORARY != 0 {
            return Err(RuntimePathError::Unsupported);
        }
        if bits & OpenIntent::PATH_ONLY != 0 {
            flags &= !(libc::O_RDONLY | libc::O_WRONLY | libc::O_RDWR);
            if bits & OpenIntent::NOFOLLOW != 0 {
                flags &= !libc::O_NOFOLLOW;
                flags |= libc::O_SYMLINK;
            } else {
                flags |= libc::O_EVTONLY;
            }
        }
        Ok(flags)
    }
}
