use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hl_runtime::{
    Access, AccessIdentity, Identity as FileIdentity, Kind, Metadata, Permissions, PreparedPathMutation,
    RuntimePathError, Timestamp,
};

use super::{HostError, NativePath, mutation::NativeLink};

pub(super) struct Descriptor(RawFd);

impl Descriptor {
    pub(super) const fn new(descriptor: RawFd) -> Self {
        Self(descriptor)
    }

    pub(super) fn metadata(&self) -> Result<Metadata, RuntimePathError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: descriptor is retained by the caller, status is writable, and
        // fstat retains neither descriptor nor output pointer.
        if unsafe { libc::fstat(self.0, status.as_mut_ptr()) } != 0 {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful fstat initialized the complete stat value.
        let value = unsafe { status.assume_init() };
        Ok(Metadata {
            identity: FileIdentity {
                device: value.st_dev,
                inode: value.st_ino,
            },
            kind: Kind::Directory,
            permissions: Permissions::from_bits((value.st_mode & 0o7777) as u16),
            links: u64::from(value.st_nlink),
            user: value.st_uid,
            group: value.st_gid,
            special_device: value.st_rdev,
            size: value.st_size as u64,
            blocks_512: value.st_blocks as u64,
            accessed: Timestamp {
                seconds: value.st_atime,
                nanoseconds: value.st_atime_nsec as u32,
            },
            modified: Timestamp {
                seconds: value.st_mtime,
                nanoseconds: value.st_mtime_nsec as u32,
            },
            changed: Timestamp {
                seconds: value.st_ctime,
                nanoseconds: value.st_ctime_nsec as u32,
            },
        })
    }

    /// Metadata carrying the guest owner rather than the host inode's. The host path is rendered
    /// only when the registry has no settled answer for this inode, so a recorded owner is free.
    pub(super) fn projected<F: FnOnce() -> Option<PathBuf>>(
        &self,
        ownership: &super::metadata::Registry,
        path: F,
    ) -> Result<Metadata, RuntimePathError> {
        let mut value = self.metadata()?;
        let path = (!ownership.knows(value.identity.device, value.identity.inode))
            .then(path)
            .flatten();
        ownership.project_at(path.as_deref(), &mut value);
        Ok(value)
    }
}

pub(super) fn authorize_chmod(
    descriptor: RawFd,
    mode: &mut u32,
    ownership: &super::metadata::Registry,
    path: Option<&std::path::Path>,
    identity: &AccessIdentity,
) -> Result<(), RuntimePathError> {
    let current = Descriptor::new(descriptor).projected(ownership, || path.map(Path::to_path_buf))?;
    // Linux `chmod` on a file another uid owns is EPERM, matching `prepare_chmod`, not EACCES.
    if identity.user != current.user && !identity.capabilities.owner_override {
        return Err(RuntimePathError::OperationNotPermitted);
    }
    if *mode & u32::from(Permissions::SET_GROUP_ID) != 0
        && !identity.belongs_to_group(current.group)
        && !identity.capabilities.preserve_set_id
    {
        *mode &= !u32::from(Permissions::SET_GROUP_ID);
    }
    Ok(())
}

/// Linux permission policy for `open`. Creating a name that does not exist is a write to the
/// parent directory; opening one that does needs read for `O_RDONLY` and write for `O_WRONLY`,
/// `O_RDWR`, `O_APPEND`, and `O_TRUNC` -- `O_TRUNC` included, because it shortens the file whatever
/// the access mode asks for, so `O_RDONLY|O_TRUNC` on a file the task may only read is EACCES.
/// `O_PATH` describes a name rather than opening it and is exempt, as it is on Linux.
pub(super) fn authorize_open(
    parent: &super::overlay_lease::ParentLease,
    name: &std::ffi::CString,
    path: &Path,
    intent: hl_runtime::OpenIntent,
    ownership: &super::metadata::Registry,
    identity: &AccessIdentity,
) -> Result<(), RuntimePathError> {
    let bits = intent.bits();
    if bits & hl_runtime::OpenIntent::PATH_ONLY != 0 {
        return Ok(());
    }
    let granted = |metadata: &Metadata, wanted: u8| -> Result<(), RuntimePathError> {
        let access = Access::from_bits(wanted).map_err(|_| RuntimePathError::Invalid)?;
        identity
            .check_access(metadata, access)
            .map_err(|_| RuntimePathError::Access)
    };
    // Existence is the visible-layer answer, not an `fstatat` of the pinned parent: image content
    // lives in a lower layer whose name the upper parent does not carry, and treating that as a
    // creation would demand write on a read-only image directory for an ordinary read.
    let current = match super::metadata::HostMetadata::anchored(parent, name) {
        Ok(mut current) => {
            ownership.project_at(Some(path), &mut current);
            Some(current)
        }
        Err(RuntimePathError::NotFound) => None,
        Err(error) => return Err(error),
    };
    let Some(current) = current else {
        // A name with no inode is about to be created, so the parent must be writable and
        // searchable. The file itself has no mode to check yet.
        let selected = parent.selected().as_raw_fd();
        let directory =
            Descriptor::new(selected).projected(ownership, || super::pin::Host::descriptor_path(selected).ok())?;
        return granted(&directory, Access::WRITE | Access::EXECUTE);
    };
    let mut wanted = 0;
    if bits & hl_runtime::OpenIntent::READ != 0 {
        wanted |= Access::READ;
    }
    if bits & (hl_runtime::OpenIntent::WRITE | hl_runtime::OpenIntent::TRUNCATE | hl_runtime::OpenIntent::APPEND) != 0 {
        wanted |= Access::WRITE;
    }
    granted(&current, wanted)
}

pub(super) fn authorize_chown(
    descriptor: RawFd,
    user: Option<u32>,
    group: Option<u32>,
    ownership: &super::metadata::Registry,
    path: Option<&Path>,
    identity: &AccessIdentity,
) -> Result<(), RuntimePathError> {
    let current = Descriptor::new(descriptor).projected(ownership, || path.map(Path::to_path_buf))?;
    identity
        .chown(&current, user, group)
        .map(|_| ())
        .map_err(|_| RuntimePathError::OperationNotPermitted)
}

pub(super) fn authorize_times(
    descriptor: RawFd,
    times: &[hl_linux::TimestampChange; 2],
    ownership: &super::metadata::Registry,
    path: Option<&Path>,
    identity: &AccessIdentity,
) -> Result<(), RuntimePathError> {
    let current = Descriptor::new(descriptor).projected(ownership, || path.map(Path::to_path_buf))?;
    if identity.user == current.user || identity.capabilities.owner_override {
        return Ok(());
    }
    // A non-owner setting explicit times is EPERM; only the now-or-omit form falls back to needing
    // write access, which is the EACCES case.
    if times
        .iter()
        .any(|change| matches!(change, hl_linux::TimestampChange::Value { .. }))
    {
        return Err(RuntimePathError::OperationNotPermitted);
    }
    let write = Access::from_bits(Access::WRITE).map_err(|_| RuntimePathError::Invalid)?;
    identity
        .check_access(&current, write)
        .map_err(|_| RuntimePathError::Access)
}

pub(super) fn prepare_chmod(
    host: &NativePath,
    source: hl_descriptor::OperationLease,
    mode: u32,
    identity: &AccessIdentity,
) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
    let file = NativeLink::acquire(source)?;
    // A descriptor names no path, so only a recorded creation or chown can answer here.
    let current = Descriptor::new(file.as_raw_fd()).projected(&host.ownership, || None)?;
    if identity.user != current.user && !identity.capabilities.owner_override {
        return Err(RuntimePathError::OperationNotPermitted);
    }
    Ok(Box::new(PendingChmod {
        file,
        mode: mode & 0o7777,
    }))
}

pub(super) fn prepare_chown(
    host: &NativePath,
    source: hl_descriptor::OperationLease,
    user: Option<u32>,
    group: Option<u32>,
    identity: &AccessIdentity,
) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
    let file = NativeLink::acquire(source)?;
    let current = Descriptor::new(file.as_raw_fd()).projected(&host.ownership, || None)?;
    let changed = identity
        .chown(&current, user, group)
        .map_err(|_| RuntimePathError::OperationNotPermitted)?;
    Ok(Box::new(PendingChown {
        file,
        user: changed.user,
        group: changed.group,
        ownership: Arc::clone(&host.ownership),
    }))
}

pub(super) fn prepare_times(
    host: &NativePath,
    source: hl_descriptor::OperationLease,
    times: [hl_linux::TimestampChange; 2],
    identity: &AccessIdentity,
) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
    let file = NativeLink::acquire(source)?;
    authorize_times(file.as_raw_fd(), &times, &host.ownership, None, identity)?;
    Ok(Box::new(PendingTimes { file, times }))
}

#[derive(Debug)]
struct PendingTimes {
    file: std::fs::File,
    times: [hl_linux::TimestampChange; 2],
}

impl PreparedPathMutation for PendingTimes {
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        set_times_fd(self.file.as_raw_fd(), self.times, false)
    }

    fn rollback(self: Box<Self>) {}
}

#[derive(Debug)]
struct PendingChown {
    file: std::fs::File,
    user: u32,
    group: u32,
    ownership: Arc<super::metadata::Registry>,
}

impl PreparedPathMutation for PendingChown {
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        self.ownership.set(self.file.as_raw_fd(), self.user, self.group)
    }

    fn rollback(self: Box<Self>) {}
}

#[derive(Debug)]
struct PendingChmod {
    file: std::fs::File,
    mode: u32,
}

impl PreparedPathMutation for PendingChmod {
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        // SAFETY: fchmod observes a retained descriptor and retains no state.
        // SAFETY: the retained descriptor is live and fchmod retains no state.
        let status = unsafe { libc::fchmod(self.file.as_raw_fd(), self.mode) };
        if status == 0 {
            Ok(())
        } else {
            Err(HostError::map(std::io::Error::last_os_error()))
        }
    }

    fn rollback(self: Box<Self>) {}
}

pub(super) fn set_times_fd(
    descriptor: RawFd,
    changes: [hl_linux::TimestampChange; 2],
    nofollow: bool,
) -> Result<(), RuntimePathError> {
    let convert = |change| match change {
        hl_linux::TimestampChange::Omit => libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        hl_linux::TimestampChange::Now => libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
        hl_linux::TimestampChange::Value { seconds, nanoseconds } => libc::timespec {
            tv_sec: seconds,
            tv_nsec: i64::from(nanoseconds),
        },
    };
    let times = [convert(changes[0]), convert(changes[1])];
    let flags = libc::AT_EMPTY_PATH | if nofollow { libc::AT_SYMLINK_NOFOLLOW } else { 0 };
    // SAFETY: the retained inode descriptor and fixed-size timespec array
    // remain live; utimensat retains neither.
    let status = unsafe { libc::utimensat(descriptor, c"".as_ptr(), times.as_ptr(), flags) };
    if status == 0 {
        Ok(())
    } else {
        Err(HostError::map(std::io::Error::last_os_error()))
    }
}
