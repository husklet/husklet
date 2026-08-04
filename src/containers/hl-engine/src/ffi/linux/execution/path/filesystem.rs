use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use hl_runtime::{FilesystemKind, FilesystemStats, GuestPath, RuntimePathError};

use super::HostError;

pub(super) struct HostFilesystem;

impl HostFilesystem {
    pub(super) fn synthetic(guest: &GuestPath) -> FilesystemStats {
        let (kind, _) = Self::kind(guest);
        FilesystemStats {
            kind,
            block_size: 4096,
            blocks: 0,
            blocks_free: 0,
            blocks_available: 0,
            files: 0,
            files_free: 0,
            filesystem_id: [0, 0],
            name_maximum: 255,
            fragment_size: 4096,
            read_only: false,
            nosuid: kind != FilesystemKind::Overlay,
            nodev: kind != FilesystemKind::Overlay,
            noexec: kind != FilesystemKind::Overlay,
            relatime: true,
        }
    }

    pub(super) fn read(path: &Path, guest: &GuestPath) -> Result<FilesystemStats, RuntimePathError> {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
        let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: the pathname is live and terminated, and status is aligned writable storage.
        let result = unsafe { libc::statvfs(path.as_ptr(), status.as_mut_ptr()) };
        if result != 0 {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful statvfs initialized the complete output structure.
        let status = unsafe { status.assume_init() };
        let (kind, zero) = Self::kind(guest);
        let restricted = kind != FilesystemKind::Overlay;
        let filesystem_id = status.f_fsid as u64;
        FilesystemStats {
            kind,
            block_size: status.f_bsize as u64,
            blocks: if zero { 0 } else { status.f_blocks as u64 },
            blocks_free: if zero { 0 } else { status.f_bfree as u64 },
            blocks_available: if zero { 0 } else { status.f_bavail as u64 },
            files: if zero { 0 } else { status.f_files as u64 },
            files_free: if zero { 0 } else { status.f_ffree as u64 },
            filesystem_id: [filesystem_id as u32, (filesystem_id >> 32) as u32],
            name_maximum: 255,
            fragment_size: status.f_bsize as u64,
            read_only: false,
            nosuid: restricted,
            nodev: restricted,
            noexec: restricted,
            relatime: true,
        }
        .validate()
        .map_err(|_| RuntimePathError::Invalid)
    }

    fn kind(guest: &GuestPath) -> (FilesystemKind, bool) {
        let components = guest
            .as_str()
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        match components.as_slice() {
            ["proc", ..] => (FilesystemKind::Proc, true),
            ["sys", "fs", "cgroup", ..] => (FilesystemKind::Cgroup2, true),
            ["sys", ..] => (FilesystemKind::Sys, true),
            ["dev", "mqueue", ..] => (FilesystemKind::Mqueue, false),
            ["dev", "pts", ..] => (FilesystemKind::Devpts, false),
            ["dev", ..] => (FilesystemKind::Tmpfs, false),
            _ => (FilesystemKind::Overlay, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_mount_kinds_do_not_depend_on_host_paths() {
        for (path, expected, zero) in [
            ("/", FilesystemKind::Overlay, false),
            ("/proc/missing", FilesystemKind::Proc, true),
            ("/sys/missing", FilesystemKind::Sys, true),
            ("/sys/fs/cgroup/missing", FilesystemKind::Cgroup2, true),
            ("/dev/shm/missing", FilesystemKind::Tmpfs, false),
        ] {
            let guest = GuestPath::new(path).unwrap();
            assert_eq!(HostFilesystem::kind(&guest), (expected, zero));
        }
    }

    #[test]
    fn synthetic_geometry_is_valid() {
        for path in ["/proc/self/auxv", "/sys/devices", "/dev/null"] {
            assert!(
                HostFilesystem::synthetic(&GuestPath::new(path).unwrap())
                    .validate()
                    .is_ok()
            );
        }
    }
}
