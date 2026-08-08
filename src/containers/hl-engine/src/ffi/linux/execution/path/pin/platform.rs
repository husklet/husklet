//! Platform descriptor, node-kind, and open-flag helpers for the pin host.

use std::ffi::CStr;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

use hl_runtime::{NodeHandle, NodeKind, OpenIntent, ResolveHostError, RuntimePathError};

use super::{Host, LAYERED_HANDLE_BIT};

impl Host {
    pub(super) fn is_layered(handle: NodeHandle) -> bool {
        handle.raw() & LAYERED_HANDLE_BIT != 0
    }

    pub(super) fn direct_handle(descriptor: OwnedFd) -> NodeHandle {
        use std::os::fd::IntoRawFd;
        let descriptor = descriptor.into_raw_fd();
        NodeHandle::from_raw(u64::try_from(descriptor).expect("descriptor is nonnegative") + 1)
    }

    pub(super) fn direct_descriptor(handle: NodeHandle) -> RawFd {
        debug_assert!(!Self::is_layered(handle));
        i32::try_from(handle.raw() - 1).expect("native descriptor handle")
    }

    pub(super) fn selected_descriptor(&self, handle: NodeHandle) -> Result<RawFd, ResolveHostError> {
        if Self::is_layered(handle) {
            self.with_entry(handle, |entry| entry.candidates[0].descriptor.as_raw_fd())
        } else {
            Ok(Self::direct_descriptor(handle))
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn pin_flags(_: NodeKind) -> i32 {
        libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC
    }

    #[cfg(target_os = "macos")]
    pub(super) fn pin_flags(kind: NodeKind) -> i32 {
        libc::O_CLOEXEC
            | if kind == NodeKind::Symlink {
                libc::O_SYMLINK
            } else {
                libc::O_EVTONLY | libc::O_NOFOLLOW
            }
    }

    pub(super) fn child_kind(directory: RawFd, name: &CStr) -> Result<NodeKind, ResolveHostError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: name is terminated, directory is pinned, and fstatat initializes
        // status on success without retaining either pointer.
        if unsafe { libc::fstatat(directory, name.as_ptr(), status.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: successful fstatat initialized every field.
        Ok(Self::kind_from_mode(unsafe { status.assume_init() }.st_mode))
    }

    pub(super) fn metadata_kind(descriptor: RawFd) -> Result<NodeKind, ResolveHostError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes status on success and retains no pointer.
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: the successful fstat initialized every field.
        let mode = unsafe { status.assume_init() }.st_mode;
        Ok(Self::kind_from_mode(mode))
    }

    pub(super) fn device(descriptor: RawFd) -> Result<libc::dev_t, ResolveHostError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: fstat initializes status on success and retains no pointer.
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } != 0 {
            return Err(Self::error());
        }
        // SAFETY: successful fstat initialized every field.
        Ok(unsafe { status.assume_init() }.st_dev)
    }

    pub(super) fn kind_from_mode(mode: libc::mode_t) -> NodeKind {
        match mode & libc::S_IFMT {
            libc::S_IFDIR => NodeKind::Directory,
            libc::S_IFREG => NodeKind::File,
            libc::S_IFLNK => NodeKind::Symlink,
            _ => NodeKind::Other,
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn read_link(descriptor: RawFd, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        let empty = c"";
        // SAFETY: output is writable for its length and empty requests the pinned
        // O_PATH link itself through Linux AT_EMPTY_PATH semantics.
        let count = unsafe { libc::readlinkat(descriptor, empty.as_ptr(), output.as_mut_ptr().cast(), output.len()) };
        usize::try_from(count).map_err(|_| Self::error())
    }

    #[cfg(target_os = "macos")]
    pub(super) fn read_link(descriptor: RawFd, output: &mut [u8]) -> Result<usize, ResolveHostError> {
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

    pub(super) fn error() -> ResolveHostError {
        Self::map(std::io::Error::last_os_error())
    }

    pub(super) fn map(error: std::io::Error) -> ResolveHostError {
        match error.kind() {
            std::io::ErrorKind::NotFound => ResolveHostError::NotFound,
            std::io::ErrorKind::NotADirectory => ResolveHostError::NotDirectory,
            std::io::ErrorKind::PermissionDenied => ResolveHostError::PermissionDenied,
            _ => ResolveHostError::Io,
        }
    }

    pub(super) fn open_flags(intent: OpenIntent) -> Result<i32, RuntimePathError> {
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
    pub(in crate::ffi::linux::execution::path) fn descriptor_path(descriptor: RawFd) -> std::io::Result<PathBuf> {
        std::fs::read_link(format!("/proc/self/fd/{descriptor}"))
    }

    #[cfg(target_os = "macos")]
    pub(in crate::ffi::linux::execution::path) fn descriptor_path(descriptor: RawFd) -> std::io::Result<PathBuf> {
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
    pub(super) fn platform_open_flags(mut flags: i32, bits: u32) -> Result<i32, RuntimePathError> {
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
    pub(super) fn platform_open_flags(mut flags: i32, bits: u32) -> Result<i32, RuntimePathError> {
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
