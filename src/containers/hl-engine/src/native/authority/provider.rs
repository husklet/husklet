#![allow(unsafe_code)]

use crate::engine::EngineError;

pub(super) struct ProjectedBackend {
    file: Option<std::fs::File>,
    root: Option<std::fs::File>,
}

impl ProjectedBackend {
    pub(super) fn new(descriptor: Option<i32>, root: Option<i32>) -> Result<Self, EngineError> {
        let file = descriptor
            .map(crate::ffi::linux::InheritedFile::adopt)
            .transpose()
            .map_err(|()| EngineError::AuthorityFailed)?;
        let root = root
            .map(crate::ffi::linux::InheritedFile::adopt)
            .transpose()
            .map_err(|()| EngineError::AuthorityFailed)?;
        Ok(Self { file, root })
    }

    pub(super) fn tree(&self, writable: bool) -> Result<ProjectedTree, EngineError> {
        let root = self
            .root
            .as_ref()
            .ok_or(EngineError::AuthorityFailed)?
            .try_clone()
            .map_err(|_| EngineError::AuthorityFailed)?;
        Ok(ProjectedTree { root, writable })
    }
}

pub(super) struct ProjectedTree {
    root: std::fs::File,
    writable: bool,
}

impl hl_provider::TreeRoot for ProjectedTree {
    fn open_in_root(
        &mut self,
        path: &[u8],
        options: hl_provider::TreeOpen,
    ) -> Result<Box<dyn hl_provider::TreeObject>, i32> {
        let file = crate::ffi::linux::PinnedRoot::open(&self.root, path, options, self.writable)?;
        let guest = if options.kind == hl_provider::TreeKind::Link {
            None
        } else {
            Some(crate::ffi::linux::PinnedRoot::guest(&self.root, &file)?)
        };
        let root = self.root.try_clone().map_err(ProjectedNode::errno)?;
        Ok(Box::new(ProjectedNode {
            file,
            root,
            guest,
            writable: self.writable,
        }))
    }
}

struct ProjectedNode {
    file: std::fs::File,
    root: std::fs::File,
    guest: Option<Vec<u8>>,
    writable: bool,
}

impl hl_provider::TreeObject for ProjectedNode {
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, i32> {
        use std::os::unix::fs::FileExt;
        self.file.read_at(output, offset).map_err(Self::errno)
    }
    fn stat(&self) -> Result<hl_provider::TreeStat, i32> {
        use std::os::unix::fs::MetadataExt;
        let value = self.file.metadata().map_err(Self::errno)?;
        Ok(hl_provider::TreeStat {
            size: value.len(),
            mode: value.mode(),
            device: value.dev(),
            inode: value.ino(),
        })
    }
    fn read_link(&self, maximum: usize) -> Result<Vec<u8>, i32> {
        crate::ffi::linux::PinnedRoot::read_link(&self.file, maximum)
    }
    fn entries(&mut self, maximum: usize) -> Result<Vec<u8>, i32> {
        crate::ffi::linux::PinnedRoot::entries(&self.file, maximum)
    }
    fn write_at(&mut self, offset: u64, input: &[u8]) -> Result<usize, i32> {
        use std::os::unix::fs::FileExt;
        if !self.writable {
            return Err(libc::EROFS);
        }
        self.file.write_at(input, offset).map_err(Self::errno)
    }
    fn append(&mut self, input: &[u8]) -> Result<(usize, u64), i32> {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        if !self.writable {
            return Err(libc::EROFS);
        }
        let descriptor = self.file.as_raw_fd();
        // SAFETY: fcntl retains no pointer. TreeAuthority serializes every
        // operation on this OFD while append is temporarily enabled, and the
        // original status flags are restored before returning.
        let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        if flags < 0 {
            return Err(Self::last_errno());
        }
        // SAFETY: this only changes status flags on the live authority-owned OFD.
        if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_APPEND) } < 0 {
            return Err(Self::last_errno());
        }
        let result = self.file.write(input).map_err(Self::errno);
        // SAFETY: this restores the flags read from the same live OFD above.
        let restore = unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags) };
        if restore < 0 {
            return Err(Self::last_errno());
        }
        let count = result?;
        let end = self.file.metadata().map_err(Self::errno)?.size();
        Ok((count, end))
    }
    fn truncate(&mut self, size: u64) -> Result<(), i32> {
        if !self.writable {
            return Err(libc::EROFS);
        }
        self.file.set_len(size).map_err(Self::errno)
    }
    fn open_in_root(
        &self,
        path: &[u8],
        options: hl_provider::TreeOpen,
    ) -> Result<Box<dyn hl_provider::TreeObject>, i32> {
        if path.is_empty() || path[0] == b'/' || path.contains(&0) {
            return Err(libc::EINVAL);
        }
        let mut absolute = self.guest.clone().ok_or(libc::ENOTDIR)?;
        if !absolute.ends_with(b"/") {
            absolute.push(b'/');
        }
        absolute.extend_from_slice(path);
        let file = crate::ffi::linux::PinnedRoot::open(&self.root, &absolute, options, self.writable)?;
        let guest = if options.kind == hl_provider::TreeKind::Link {
            None
        } else {
            Some(crate::ffi::linux::PinnedRoot::guest(&self.root, &file)?)
        };
        let root = self.root.try_clone().map_err(Self::errno)?;
        Ok(Box::new(Self {
            file,
            root,
            guest,
            writable: self.writable,
        }))
    }
}

impl ProjectedNode {
    fn errno(error: std::io::Error) -> i32 {
        error.raw_os_error().unwrap_or(libc::EIO)
    }
    fn last_errno() -> i32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
    }
}

impl hl_provider::FileBackend for ProjectedBackend {
    fn open(&mut self, service: u64, access: u8) -> Result<Box<dyn hl_provider::FileObject>, i32> {
        if service != 1 {
            return Err(libc::ENOENT);
        }
        if access != 1 {
            return Err(libc::EACCES);
        }
        let file = self
            .file
            .as_ref()
            .ok_or(libc::ENOENT)?
            .try_clone()
            .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))?;
        Ok(Box::new(ProjectedObject(file)))
    }
}

struct ProjectedObject(std::fs::File);

impl hl_provider::FileObject for ProjectedObject {
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, i32> {
        use std::os::unix::fs::FileExt;
        self.0
            .read_at(output, offset)
            .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))
    }

    fn info(&self) -> Result<hl_provider::FileInfo, i32> {
        use std::os::unix::fs::MetadataExt;
        let value = self
            .0
            .metadata()
            .map_err(|error| error.raw_os_error().unwrap_or(libc::EIO))?;
        Ok(hl_provider::FileInfo {
            size: value.len(),
            mode: value.mode(),
            device: value.dev(),
            inode: value.ino(),
        })
    }
}
