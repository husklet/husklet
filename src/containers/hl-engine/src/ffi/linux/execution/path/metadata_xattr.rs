//! Extended-attribute targets and staged xattr mutations.

use std::ffi::CString;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use hl_linux::Errno;
use hl_runtime::{PreparedXattrMutation, RuntimeXattrMutation, XattrFlags, XattrName};

#[derive(Debug)]
pub(super) enum XattrTarget {
    Path(PathBuf),
    File(File),
}

impl XattrTarget {
    pub(super) fn name(name: &XattrName) -> Result<CString, Errno> {
        CString::new(name.as_bytes()).map_err(|_| Errno::EINVAL)
    }

    fn errno() -> Errno {
        Errno::from_host(Errno::from_raw(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO),
        ))
    }

    pub(super) fn get(&self, name: &XattrName) -> Result<Vec<u8>, Errno> {
        let name = Self::name(name)?;
        // SAFETY: both callers below pass either (null, 0) or a pointer into `value` with
        // its exact length, and `name`/`path` CStrings outlive each call; a racing xattr
        // growth yields ERANGE rather than an overrun.
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

    pub(super) fn list(&self) -> Result<Vec<u8>, Errno> {
        // SAFETY: both callers below pass either (null, 0) or `value`'s pointer with its
        // exact length, and the `path` CString outlives each call; a racing xattr growth
        // yields ERANGE rather than an overrun.
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
        // SAFETY: `value` is a live borrowed slice passed with its own length, and the
        // `name`/`path` CStrings are NUL-terminated and outlive the call; the kernel only
        // reads from these buffers.
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
        // SAFETY: the `name`/`path` CStrings are NUL-terminated and live for the whole
        // call, and the File variant's descriptor is kept open by the borrowed `File`.
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
pub(super) struct XattrTransaction {
    target: XattrTarget,
    mutation: RuntimeXattrMutation,
    previous: Option<Vec<u8>>,
    committed: bool,
}

impl XattrTransaction {
    pub(super) fn prepare(
        target: XattrTarget,
        mutation: RuntimeXattrMutation,
    ) -> Result<Box<dyn PreparedXattrMutation>, Errno> {
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
