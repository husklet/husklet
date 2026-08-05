use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStringExt;
use std::sync::{Arc, Mutex};

use hl_loader::{ImageRole, ImageSource, ImageSourceError};
use hl_runtime::{RuntimeExecError, SourceFactory};
use hl_task::ProcessId;

pub(super) struct FileSource {
    rootfs: Option<Vec<u8>>,
    root_primary: bool,
}

impl FileSource {
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_IN_ROOT: u64 = 0x10;
    const SYS_OPENAT2: i64 = 437;

    pub(super) fn new(rootfs: Option<&[u8]>) -> Self {
        Self {
            rootfs: rootfs.map(<[u8]>::to_vec),
            root_primary: false,
        }
    }

    pub(super) fn rooted(rootfs: Option<&[u8]>) -> Self {
        Self {
            rootfs: rootfs.map(<[u8]>::to_vec),
            root_primary: true,
        }
    }

    fn open(&self, role: ImageRole, path: &[u8]) -> Result<File, ImageSourceError> {
        if let Some(rootfs) = self.rootfs.as_deref() {
            if role == ImageRole::Interpreter || self.root_primary {
                return Self::open_rooted(rootfs, path);
            }
            if role == ImageRole::Main
                && let Some(guest) = Self::inside_root(rootfs, path)
            {
                return Self::open_rooted(rootfs, guest);
            }
        }
        File::open(OsString::from_vec(path.to_vec())).map_err(Self::map_io)
    }

    fn inside_root<'a>(rootfs: &[u8], path: &'a [u8]) -> Option<&'a [u8]> {
        let root = rootfs.strip_suffix(b"/").unwrap_or(rootfs);
        let suffix = path.strip_prefix(root)?;
        (suffix.first() == Some(&b'/')).then_some(suffix)
    }

    fn open_rooted(rootfs: &[u8], path: &[u8]) -> Result<File, ImageSourceError> {
        let root = File::open(OsString::from_vec(rootfs.to_vec())).map_err(Self::map_io)?;
        if !root.metadata().map_err(Self::map_io)?.is_dir() {
            return Err(ImageSourceError::AccessDenied);
        }
        let path = CString::new(path).map_err(|_| ImageSourceError::AccessDenied)?;
        let how = super::super::OpenHow {
            flags: super::super::abi::O_RDONLY as u64 | super::super::abi::O_CLOEXEC as u64,
            mode: 0,
            resolve: Self::RESOLVE_IN_ROOT | Self::RESOLVE_NO_MAGICLINKS,
        };
        // SAFETY: all pointers remain live for this non-retaining syscall and
        // RESOLVE_IN_ROOT confines traversal, including absolute symlinks.
        let descriptor = unsafe {
            super::super::abi::syscall(
                Self::SYS_OPENAT2,
                root.as_raw_fd(),
                path.as_ptr(),
                &how as *const super::super::OpenHow,
                std::mem::size_of::<super::super::OpenHow>(),
            )
        };
        if descriptor < 0 {
            return Err(Self::map_io(std::io::Error::last_os_error()));
        }
        let descriptor = i32::try_from(descriptor).map_err(|_| ImageSourceError::Io)?;
        // SAFETY: openat2 returned a new descriptor with no other owner.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    fn read(mut file: File, maximum: usize) -> Result<Vec<u8>, ImageSourceError> {
        let metadata = file.metadata().map_err(Self::map_io)?;
        if !metadata.is_file() {
            return Err(ImageSourceError::Io);
        }
        if metadata.len() > maximum as u64 {
            return Err(ImageSourceError::TooLarge);
        }
        let limit = u64::try_from(maximum)
            .map_err(|_| ImageSourceError::TooLarge)?
            .saturating_add(1);
        let capacity = usize::try_from(metadata.len()).map_err(|_| ImageSourceError::TooLarge)?;
        let mut bytes = Vec::with_capacity(capacity);
        file.by_ref()
            .take(limit)
            .read_to_end(&mut bytes)
            .map_err(Self::map_io)?;
        if bytes.len() > maximum {
            return Err(ImageSourceError::TooLarge);
        }
        Ok(bytes)
    }

    fn map_io(error: std::io::Error) -> ImageSourceError {
        match error.kind() {
            std::io::ErrorKind::NotFound => ImageSourceError::NotFound,
            std::io::ErrorKind::PermissionDenied => ImageSourceError::AccessDenied,
            _ => ImageSourceError::Io,
        }
    }
}

impl ImageSource for FileSource {
    fn read_image(&mut self, role: ImageRole, path: &[u8], maximum: usize) -> Result<Vec<u8>, ImageSourceError> {
        Self::read(self.open(role, path)?, maximum)
    }
}

pub(super) struct ProjectedSource {
    authority: Arc<Mutex<crate::native::AuthorityWorker>>,
}

impl ProjectedSource {
    pub(super) fn new(authority: Arc<Mutex<crate::native::AuthorityWorker>>) -> Self {
        Self { authority }
    }

    fn map(error: crate::native::ProjectionError) -> ImageSourceError {
        match error {
            crate::native::ProjectionError::Linux(libc::ENOENT) => ImageSourceError::NotFound,
            crate::native::ProjectionError::Linux(libc::EACCES) => ImageSourceError::AccessDenied,
            crate::native::ProjectionError::Linux(libc::EMSGSIZE) => ImageSourceError::TooLarge,
            crate::native::ProjectionError::Linux(_) | crate::native::ProjectionError::Session => ImageSourceError::Io,
        }
    }

    fn read(
        authority: &mut crate::native::AuthorityWorker,
        handle: u64,
        maximum: usize,
    ) -> Result<Vec<u8>, ImageSourceError> {
        let mut bytes = Vec::new();
        while bytes.len() < maximum {
            let remaining = maximum - bytes.len();
            let count = remaining.min(hl_provider::FileWire::MAX_READ_DATA);
            let chunk = authority
                .read_file(handle, bytes.len() as u64, count)
                .map_err(Self::map)?;
            let complete = chunk.len() < count;
            bytes.extend_from_slice(&chunk);
            if complete {
                return Ok(bytes);
            }
        }
        if authority
            .read_file(handle, bytes.len() as u64, 1)
            .map_err(Self::map)?
            .is_empty()
        {
            Ok(bytes)
        } else {
            Err(ImageSourceError::TooLarge)
        }
    }

    fn read_tree(
        authority: &mut crate::native::AuthorityWorker,
        path: &[u8],
        maximum: usize,
    ) -> Result<Vec<u8>, ImageSourceError> {
        let handle = authority.tree_open(path, false).map_err(Self::map)?;
        let result = Self::read_tree_handle(authority, handle, maximum);
        let closed = authority.tree_close(handle).map_err(Self::map);
        match (result, closed) {
            (Ok(bytes), Ok(())) => Ok(bytes),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    fn read_tree_handle(
        authority: &mut crate::native::AuthorityWorker,
        handle: u64,
        maximum: usize,
    ) -> Result<Vec<u8>, ImageSourceError> {
        let stat = authority.tree_stat(handle).map_err(Self::map)?;
        if stat.mode & 0o170000 != 0o100000 {
            return Err(ImageSourceError::AccessDenied);
        }
        if stat.size > maximum as u64 {
            return Err(ImageSourceError::TooLarge);
        }
        let mut bytes = Vec::with_capacity(stat.size as usize);
        while bytes.len() < stat.size as usize {
            let count = (stat.size as usize - bytes.len()).min(hl_provider::TreeWire::MAX_DATA);
            let chunk = authority
                .tree_read(handle, bytes.len() as u64, count)
                .map_err(Self::map)?;
            if chunk.is_empty() {
                return Err(ImageSourceError::Io);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

impl ImageSource for ProjectedSource {
    fn read_image(&mut self, role: ImageRole, path: &[u8], maximum: usize) -> Result<Vec<u8>, ImageSourceError> {
        let mut authority = self.authority.lock().map_err(|_| ImageSourceError::Io)?;
        if role == ImageRole::Interpreter {
            return Self::read_tree(&mut authority, path, maximum);
        }
        let handle = authority.open_file(1).map_err(Self::map)?;
        let result = Self::read(&mut authority, handle, maximum);
        let closed = authority.close_file(handle).map_err(Self::map);
        match (result, closed) {
            (Ok(bytes), Ok(())) => Ok(bytes),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }
}

pub(super) enum Source {
    File(FileSource),
    Projected(ProjectedSource),
}

pub(super) struct Sources {
    rootfs: Option<Vec<u8>>,
    authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
}

impl Sources {
    pub(super) fn new(rootfs: Option<&[u8]>, authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>) -> Self {
        Self {
            rootfs: rootfs.map(<[u8]>::to_vec),
            authority,
        }
    }
}

impl SourceFactory for Sources {
    type Source = Source;

    fn open(&self, _: ProcessId, plan: &hl_linux::ExecPlan) -> Result<Source, RuntimeExecError> {
        if plan.directory.is_some_and(|directory| directory != -100) {
            return Err(RuntimeExecError::BadDescriptor);
        }
        Ok(match &self.authority {
            Some(value) => Source::Projected(ProjectedSource::new(Arc::clone(value))),
            None => Source::File(FileSource::rooted(self.rootfs.as_deref())),
        })
    }
}

impl ImageSource for Source {
    fn read_image(&mut self, role: ImageRole, path: &[u8], maximum: usize) -> Result<Vec<u8>, ImageSourceError> {
        match self {
            Self::File(source) => source.read_image(role, path, maximum),
            Self::Projected(source) => source.read_image(role, path, maximum),
        }
    }
}
