//! Host path rendering for overlay parent leases.

use std::ffi::{CStr, CString, OsStr};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use hl_runtime::RuntimePathError;

use super::super::overlay_lease::ParentLease;
use super::Host;

impl Host {
    pub(in crate::ffi::linux::execution::path) fn path(
        parent: &ParentLease,
        name: &CString,
    ) -> Result<PathBuf, RuntimePathError> {
        let parent = Self::visible_parent(parent, name)?;
        let directory = Self::descriptor_path(parent.as_raw_fd()).map_err(super::super::HostError::map)?;
        Ok(directory.join(OsStr::from_bytes(name.as_bytes())))
    }

    pub(in crate::ffi::linux::execution::path) fn mutation_path(
        parent: &ParentLease,
        name: &CString,
    ) -> Result<PathBuf, RuntimePathError> {
        let directory = if let Ok(parent) = parent.mutation() {
            Self::descriptor_path(parent.as_raw_fd()).map_err(super::super::HostError::map)?
        } else {
            let root = parent.upper_root().ok_or(RuntimePathError::NotFound)?;
            let root = Self::descriptor_path(root.as_raw_fd()).map_err(super::super::HostError::map)?;
            let guest = parent.guest().ok_or(RuntimePathError::Invalid)?;
            root.join(OsStr::from_bytes(
                guest.as_bytes().strip_prefix(b"/").unwrap_or(guest.as_bytes()),
            ))
        };
        Ok(directory.join(OsStr::from_bytes(name.as_bytes())))
    }

    pub(in crate::ffi::linux::execution::path) fn layer_paths(
        parent: &ParentLease,
        name: &CString,
    ) -> Result<Vec<PathBuf>, RuntimePathError> {
        let mut paths = Vec::new();
        let selected = Self::descriptor_path(parent.selected().as_raw_fd()).map_err(super::super::HostError::map)?;
        paths.push(selected.join(OsStr::from_bytes(name.as_bytes())));
        for lower in parent.lower_parents() {
            let directory = Self::descriptor_path(lower.as_raw_fd()).map_err(super::super::HostError::map)?;
            let path = directory.join(OsStr::from_bytes(name.as_bytes()));
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    pub(super) fn visible_parent<'lease>(
        parent: &'lease ParentLease,
        name: &CStr,
    ) -> Result<&'lease OwnedFd, RuntimePathError> {
        Self::visible_status(parent, name).map(|(parent, _)| parent)
    }

    /// Selects the layer holding `name` and returns its status from the same
    /// `fstatat` that decided the layer, so no caller needs a second walk.
    pub(in crate::ffi::linux::execution::path) fn visible_status<'lease>(
        parent: &'lease ParentLease,
        name: &CStr,
    ) -> Result<(&'lease OwnedFd, libc::stat), RuntimePathError> {
        let mut candidates = Vec::with_capacity(parent.lower_parents().len() + 1);
        candidates.push(parent.selected());
        candidates.extend(parent.lower_parents());
        // This is the final-component scan, where the negative-probe storms
        // land. An indexed lower answers both of its probes from memory.
        let key = parent
            .guest()
            .map(|guest| Self::child_index_key(guest, name.to_bytes()));
        for (position, candidate) in candidates.into_iter().enumerate() {
            // An index answers absence and whiteouts outright; a hit still takes the
            // one fstatat below, since the entry carries no host `stat`.
            let mut indexed = false;
            if let (Some(key), Some(index)) = (key.as_deref(), parent.candidate_index(position)) {
                if index.is_whiteout(key) {
                    return Err(RuntimePathError::NotFound);
                }
                match index.get(key) {
                    Some(_) => indexed = true,
                    None => continue,
                }
            }
            if !indexed && Self::whiteout_at(candidate.as_raw_fd(), name)? {
                return Err(RuntimePathError::NotFound);
            }
            let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: candidate and name remain live and status is writable.
            let result = unsafe {
                libc::fstatat(
                    candidate.as_raw_fd(),
                    name.as_ptr(),
                    status.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result == 0 {
                // SAFETY: the successful fstatat initialized every field.
                return Ok((candidate, unsafe { status.assume_init() }));
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOENT) {
                return Err(super::super::HostError::map(error));
            }
        }
        Err(RuntimePathError::NotFound)
    }

    /// The layer-relative index key for a child of a pinned guest directory.
    fn child_index_key(parent: &hl_runtime::GuestPathBytes, name: &[u8]) -> Vec<u8> {
        let parent = parent.as_bytes();
        let mut key = parent.strip_prefix(b"/".as_slice()).unwrap_or(parent).to_vec();
        if !key.is_empty() {
            key.push(b'/');
        }
        key.extend_from_slice(name);
        key
    }

    pub(super) fn whiteout_at(parent: RawFd, name: &CStr) -> Result<bool, RuntimePathError> {
        let mut marker = b".wh.".to_vec();
        marker.extend_from_slice(name.to_bytes());
        let marker = CString::new(marker).map_err(|_| RuntimePathError::Invalid)?;
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent and marker remain live and status is writable.
        let result = unsafe { libc::fstatat(parent, marker.as_ptr(), status.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) };
        if result == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(false)
        } else {
            Err(super::super::HostError::map(error))
        }
    }
}
