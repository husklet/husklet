//! Directory enumeration and subtree removal for overlay marker publication.

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use super::OPAQUE_NAME;

/// Removes an upper entry, descending through a directory first: an upper
/// directory being whited out still holds its own child markers, which would
/// otherwise fail the `rmdir` with ENOTEMPTY and leave the name resolving.
pub(super) fn remove_tree(parent: &impl AsRawFd, name: &CStr) -> io::Result<()> {
    if let Some(directory) = open_directory(parent, name)? {
        for child in read_children(&directory)?.0 {
            remove_tree(&directory, &child)?;
        }
        drop(directory);
        // SAFETY: parent and name remain live and unlinkat retains neither.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } == 0 {
            return Ok(());
        }
        return Err(io::Error::last_os_error());
    }
    // SAFETY: parent and name remain live and unlinkat retains neither.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(())
    } else {
        Err(error)
    }
}

/// Opens one nofollow child directory, reporting an absent or non-directory
/// name as `None` rather than an error.
pub(super) fn open_directory(parent: &impl AsRawFd, name: &CStr) -> io::Result<Option<OwnedFd>> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: parent and name remain live and success returns a new descriptor.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            // ELOOP is how O_NOFOLLOW reports a symlink named where a directory
            // was expected, which is a non-directory like any other here.
            Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP) => Ok(None),
            _ => Err(error),
        };
    }
    // SAFETY: successful openat returned one unowned descriptor.
    Ok(Some(unsafe { OwnedFd::from_raw_fd(descriptor) }))
}

/// Child names of one open directory and whether it carries an opaque marker.
pub(super) fn read_children(directory: &OwnedFd) -> io::Result<(Vec<CString>, bool)> {
    // fdopendir consumes the descriptor it is handed, so it gets a duplicate
    // and the caller's own capability stays usable afterwards.
    let duplicate = directory.try_clone()?.into_raw_fd();
    // SAFETY: fdopendir takes ownership of the freshly duplicated descriptor.
    let handle = unsafe { libc::fdopendir(duplicate) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so the duplicate is still ours to close.
        unsafe { libc::close(duplicate) };
        return Err(error);
    }
    let mut names = Vec::new();
    let mut opaque = false;
    loop {
        // SAFETY: handle is a live stream and a null return ends enumeration.
        let entry = unsafe { libc::readdir(handle) };
        if entry.is_null() {
            break;
        }
        // SAFETY: readdir returned a live entry owned by the open stream, whose
        // name is terminated and stays valid until the next readdir call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name == c"." || name == c".." {
            continue;
        }
        if name == OPAQUE_NAME {
            opaque = true;
        }
        names.push(name.to_owned());
    }
    // SAFETY: closedir consumes the stream and its descriptor exactly once.
    unsafe { libc::closedir(handle) };
    Ok((names, opaque))
}
