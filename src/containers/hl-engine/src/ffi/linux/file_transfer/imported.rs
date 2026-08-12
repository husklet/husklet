//! Descriptor adapter for files imported over a unix-socket transfer.

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    ObjectError, OfdMetadata, OfdTimestamp, OpenFileDescription, PreparedSpliceRead, SeekPosition, StatusFlags,
};
use hl_runtime::RuntimeNetworkError;

use super::cursor::CursorGate;
use super::{FileCapability, FileOperation};

pub(super) struct ImportedFile {
    file: Mutex<File>,
    pub(super) status: StatusFlags,
    splice_gate: Arc<CursorGate>,
}

impl ImportedFile {
    pub(super) fn new(descriptor: OwnedFd) -> Result<Self, RuntimeNetworkError> {
        let file = File::from(descriptor);
        let metadata = file.metadata().map_err(|_| RuntimeNetworkError::Failed)?;
        if !metadata.file_type().is_file() {
            return Err(RuntimeNetworkError::Unsupported);
        }
        let flags = Self::flags(&file)?;
        Ok(Self {
            file: Mutex::new(file),
            status: Self::status(flags),
            splice_gate: Arc::new(CursorGate::default()),
        })
    }

    fn flags(file: &File) -> Result<i32, RuntimeNetworkError> {
        // SAFETY: F_GETFL observes the live owned file and retains no pointer.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0 {
            Err(RuntimeNetworkError::Failed)
        } else {
            Ok(flags & (libc::O_ACCMODE | libc::O_APPEND | libc::O_NONBLOCK))
        }
    }

    fn status(flags: i32) -> StatusFlags {
        let access = match flags & libc::O_ACCMODE {
            libc::O_WRONLY => 1,
            libc::O_RDWR => 2,
            _ => 0,
        };
        let append = if flags & libc::O_APPEND != 0 {
            StatusFlags::APPEND
        } else {
            0
        };
        let nonblocking = if flags & libc::O_NONBLOCK != 0 {
            StatusFlags::NONBLOCKING
        } else {
            0
        };
        StatusFlags::from_bits(access | append | nonblocking)
    }

    fn object(error: std::io::Error) -> ObjectError {
        match error.kind() {
            std::io::ErrorKind::WouldBlock => ObjectError::WouldBlock,
            std::io::ErrorKind::Interrupted => ObjectError::Interrupted,
            std::io::ErrorKind::PermissionDenied => ObjectError::PermissionDenied,
            _ => ObjectError::Io,
        }
    }
}

impl fmt::Debug for ImportedFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ImportedFile")
    }
}

impl FileCapability for ImportedFile {
    fn duplicate(&self) -> Result<OwnedFd, RuntimeNetworkError> {
        self.mapping().map(OwnedFd::from)
    }

    fn mapping(&self) -> Result<File, RuntimeNetworkError> {
        self.file
            .lock()
            .map_err(|_| RuntimeNetworkError::Failed)?
            .try_clone()
            .map_err(|_| RuntimeNetworkError::Failed)
    }

    fn operate(
        &self,
        _request: FileOperation,
        terminal: &mut dyn FnMut(RawFd) -> Result<usize, hl_runtime::VectorError>,
    ) -> Result<usize, hl_runtime::VectorError> {
        let file = self
            .file
            .lock()
            .map_err(|_| hl_runtime::VectorError::Object(hl_descriptor::ObjectError::Io))?;
        terminal(file.as_raw_fd())
    }
}

impl OpenFileDescription for ImportedFile {
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.splice_gate.enter();
        self.file
            .lock()
            .map_err(|_| ObjectError::Io)?
            .read(output)
            .map_err(Self::object)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.splice_gate.enter();
        self.file
            .lock()
            .map_err(|_| ObjectError::Io)?
            .write(input)
            .map_err(Self::object)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.file
            .lock()
            .map_err(|_| ObjectError::Io)?
            .read_at(output, offset)
            .map_err(Self::object)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.file
            .lock()
            .map_err(|_| ObjectError::Io)?
            .write_at(input, offset)
            .map_err(Self::object)
    }

    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        if matches!(position, SeekPosition::Data(_) | SeekPosition::Hole(_)) {
            return Err(ObjectError::NotSupported);
        }
        self.splice_gate.enter();
        let position = match position {
            SeekPosition::Start(value) => SeekFrom::Start(value),
            SeekPosition::Current(value) => SeekFrom::Current(value),
            SeekPosition::End(value) => SeekFrom::End(value),
            SeekPosition::Data(_) | SeekPosition::Hole(_) => unreachable!("returned above"),
        };
        self.file
            .lock()
            .map_err(|_| ObjectError::Io)?
            .seek(position)
            .map_err(Self::object)
    }

    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        let implicit = offset.is_none();
        let mut cursor = self
            .file
            .lock()
            .map_err(|_| ObjectError::Io)?
            .try_clone()
            .map_err(Self::object)?;
        let commit_cursor = implicit.then(|| cursor.try_clone()).transpose().map_err(Self::object)?;
        let start = Arc::new(Mutex::new(None));
        let prepared_start = Arc::clone(&start);
        let prepared = self.splice_gate.prepare(
            implicit,
            nonblocking,
            cancellation,
            || {
                let value = offset.map_or_else(|| cursor.stream_position().map_err(Self::object), Ok)?;
                *prepared_start.lock().map_err(|_| ObjectError::Io)? = Some(value);
                let mut bytes = vec![0; maximum.min(65_536)];
                let count = cursor.read_at(&mut bytes, value).map_err(Self::object)?;
                bytes.truncate(count);
                Ok(bytes)
            },
            move |count| {
                if let Some(mut cursor) = commit_cursor {
                    let value = start
                        .lock()
                        .map_err(|_| ObjectError::Io)?
                        .ok_or(ObjectError::Interrupted)?;
                    let end = value.checked_add(count as u64).ok_or(ObjectError::InvalidArgument)?;
                    cursor.seek(SeekFrom::Start(end)).map_err(Self::object)?;
                }
                Ok(())
            },
        )?;
        Ok(Some(prepared))
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let value = self
            .file
            .lock()
            .map_err(|_| ObjectError::Io)?
            .metadata()
            .map_err(Self::object)?;
        let timestamp = |seconds, nanoseconds: i64| OfdTimestamp {
            seconds,
            nanoseconds: u32::try_from(nanoseconds).unwrap_or(0),
        };
        Ok(OfdMetadata {
            device: value.dev(),
            inode: value.ino(),
            kind: 8,
            permissions: (value.mode() & 0o7777) as u16,
            links: value.nlink(),
            user: value.uid(),
            group: value.gid(),
            special_device: value.rdev(),
            size: value.size(),
            blocks_512: value.blocks(),
            block_size: 4096,
            accessed: timestamp(value.atime(), value.atime_nsec()),
            modified: timestamp(value.mtime(), value.mtime_nsec()),
            changed: timestamp(value.ctime(), value.ctime_nsec()),
        })
    }

    fn truncate(&self, size: u64) -> Result<(), ObjectError> {
        self.file
            .lock()
            .map_err(|_| ObjectError::Io)?
            .set_len(size)
            .map_err(Self::object)
    }

    fn synchronize(&self, data_only: bool) -> Result<(), ObjectError> {
        let file = self.file.lock().map_err(|_| ObjectError::Io)?;
        if data_only { file.sync_data() } else { file.sync_all() }.map_err(Self::object)
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        let file = self.file.lock().map_err(|_| ObjectError::Io)?;
        // SAFETY: both fcntl operations observe or update flags on this live owned descriptor.
        let current = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if current < 0 {
            return Err(Self::object(std::io::Error::last_os_error()));
        }
        let mut updated = current & !(libc::O_APPEND | libc::O_NONBLOCK);
        if flags.bits() & StatusFlags::APPEND != 0 {
            updated |= libc::O_APPEND;
        }
        if flags.bits() & StatusFlags::NONBLOCKING != 0 {
            updated |= libc::O_NONBLOCK;
        }
        // SAFETY: F_SETFL updates status flags and retains no pointer.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, updated) } < 0 {
            Err(Self::object(std::io::Error::last_os_error()))
        } else {
            Ok(())
        }
    }

    fn add_seals(&self, seals: u8) -> Result<u8, ObjectError> {
        let file = self.file.lock().map_err(|_| ObjectError::Io)?;
        // SAFETY: F_ADD_SEALS updates the live imported open file description.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, i32::from(seals)) } < 0 {
            Err(Self::object(std::io::Error::last_os_error()))
        } else {
            drop(file);
            self.seals()
        }
    }

    fn seals(&self) -> Result<u8, ObjectError> {
        let file = self.file.lock().map_err(|_| ObjectError::Io)?;
        // SAFETY: F_GET_SEALS observes the live imported open file description.
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 {
            Err(Self::object(std::io::Error::last_os_error()))
        } else {
            u8::try_from(seals).map_err(|_| ObjectError::Io)
        }
    }
}
