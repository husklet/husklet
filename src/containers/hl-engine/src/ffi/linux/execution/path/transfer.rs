use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::{
    DescriptionIdentity, ObjectError, OfdMetadata, OfdTimestamp, OpenFileDescription, PreparedSpliceRead, SeekPosition,
    StatusFlags,
};
use hl_runtime::{ImportedDescription, ImportedTransfer, RuntimeNetworkError, TransferPublication};

use super::NativeFile;
use super::splice::CursorGate;

trait FileCapability: Send + Sync {
    fn duplicate(&self) -> Result<OwnedFd, RuntimeNetworkError>;
    fn mapping(&self) -> Result<File, RuntimeNetworkError>;
    fn operate(
        &self,
        request: FileOperation,
        terminal: &mut dyn FnMut(RawFd) -> Result<usize, hl_runtime::VectorError>,
    ) -> Result<usize, hl_runtime::VectorError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::ffi::linux::execution) enum FileIntent {
    Read,
    Write,
    Probe,
}

#[derive(Clone, Copy)]
pub(in crate::ffi::linux::execution) struct FileOperation {
    pub intent: FileIntent,
    pub position: Option<u64>,
    pub append: bool,
    pub total: u64,
}

impl FileOperation {
    fn target(self, file: &File) -> Result<u64, hl_descriptor::ObjectError> {
        let start = if self.append {
            file.metadata().map_err(NativeFile::object)?.len()
        } else if let Some(position) = self.position {
            position
        } else {
            let mut cursor = file;
            cursor.stream_position().map_err(NativeFile::object)?
        };
        start.checked_add(self.total).ok_or(hl_descriptor::ObjectError::NoSpace)
    }
}

#[derive(Default)]
pub(in crate::ffi::linux::execution) struct FileTransferRegistry {
    files: Mutex<BTreeMap<DescriptionIdentity, Weak<dyn FileCapability>>>,
}

impl fmt::Debug for FileTransferRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let files = self.files.lock().map_err(|_| fmt::Error)?;
        formatter
            .debug_struct("FileTransferRegistry")
            .field("files", &files.len())
            .finish()
    }
}

impl FileTransferRegistry {
    pub(super) fn bind(
        &self,
        identity: DescriptionIdentity,
        file: Arc<NativeFile>,
    ) -> Result<(), hl_runtime::RuntimePathError> {
        let capability: Arc<dyn FileCapability> = file;
        let mut files = self.files.lock().map_err(|_| hl_runtime::RuntimePathError::Io)?;
        files.retain(|_, file| file.strong_count() != 0);
        if files.len() >= 4096 {
            return Err(hl_runtime::RuntimePathError::TooLarge);
        }
        match files.entry(identity) {
            Entry::Vacant(slot) => {
                slot.insert(Arc::downgrade(&capability));
            }
            Entry::Occupied(_) => return Err(hl_runtime::RuntimePathError::TooLarge),
        }
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn duplicate(
        &self,
        identity: DescriptionIdentity,
    ) -> Result<OwnedFd, RuntimeNetworkError> {
        self.capability(identity)?.duplicate()
    }

    pub(super) fn mapping(&self, identity: DescriptionIdentity) -> Option<File> {
        self.capability(identity).ok()?.mapping().ok()
    }

    pub(in crate::ffi::linux::execution) fn supports(&self, identity: DescriptionIdentity) -> bool {
        self.capability(identity).is_ok()
    }

    pub(in crate::ffi::linux::execution) fn operate(
        &self,
        identity: DescriptionIdentity,
        request: FileOperation,
        mut terminal: impl FnMut(RawFd) -> Result<usize, hl_runtime::VectorError>,
    ) -> Result<usize, hl_runtime::VectorError> {
        let capability = self.capability(identity).map_err(|error| match error {
            RuntimeNetworkError::Unsupported => hl_runtime::VectorError::Unsupported,
            RuntimeNetworkError::Invalid => hl_runtime::VectorError::Object(hl_descriptor::ObjectError::BadDescriptor),
            RuntimeNetworkError::NoMemory => hl_runtime::VectorError::Object(hl_descriptor::ObjectError::ResourceLimit),
            RuntimeNetworkError::Failed => hl_runtime::VectorError::Object(hl_descriptor::ObjectError::Io),
            _ => hl_runtime::VectorError::Unsupported,
        })?;
        capability.operate(request, &mut terminal)
    }

    pub(in crate::ffi::linux::execution) fn import(
        self: &Arc<Self>,
        descriptor: OwnedFd,
    ) -> Result<ImportedTransfer, RuntimeNetworkError> {
        let file = Arc::new(ImportedFile::new(descriptor)?);
        let description = ImportedDescription {
            object: file.clone(),
            status: file.status,
        };
        Ok(ImportedTransfer::new(
            vec![description],
            Box::new(FilePublication {
                registry: Arc::clone(self),
                file,
                identity: None,
            }),
        ))
    }

    fn capability(&self, identity: DescriptionIdentity) -> Result<Arc<dyn FileCapability>, RuntimeNetworkError> {
        self.files
            .lock()
            .map_err(|_| RuntimeNetworkError::Failed)?
            .get(&identity)
            .and_then(Weak::upgrade)
            .ok_or(RuntimeNetworkError::Unsupported)
    }

    fn bind_import(&self, identity: DescriptionIdentity, file: Arc<ImportedFile>) -> Result<(), RuntimeNetworkError> {
        let capability: Arc<dyn FileCapability> = file;
        let mut files = self.files.lock().map_err(|_| RuntimeNetworkError::Failed)?;
        files.retain(|_, file| file.strong_count() != 0);
        if files.len() >= 4096 {
            return Err(RuntimeNetworkError::NoMemory);
        }
        match files.entry(identity) {
            Entry::Vacant(slot) => {
                slot.insert(Arc::downgrade(&capability));
            }
            Entry::Occupied(_) => return Err(RuntimeNetworkError::Invalid),
        }
        Ok(())
    }

    fn remove(&self, identity: DescriptionIdentity) {
        if let Ok(mut files) = self.files.lock() {
            files.remove(&identity);
        }
    }
}

impl FileCapability for NativeFile {
    fn duplicate(&self) -> Result<OwnedFd, RuntimeNetworkError> {
        let file = self.file.lock().map_err(|_| RuntimeNetworkError::Failed)?;
        let file = file.as_ref().ok_or(RuntimeNetworkError::Unsupported)?;
        file.try_clone()
            .map(OwnedFd::from)
            .map_err(|_| RuntimeNetworkError::Failed)
    }

    fn mapping(&self) -> Result<File, RuntimeNetworkError> {
        self.file
            .lock()
            .map_err(|_| RuntimeNetworkError::Failed)?
            .as_ref()
            .ok_or(RuntimeNetworkError::Unsupported)?
            .try_clone()
            .map_err(|_| RuntimeNetworkError::Failed)
    }

    fn operate(
        &self,
        request: FileOperation,
        terminal: &mut dyn FnMut(RawFd) -> Result<usize, hl_runtime::VectorError>,
    ) -> Result<usize, hl_runtime::VectorError> {
        self.io().map_err(hl_runtime::VectorError::Object)?;
        let opened = self
            .file
            .lock()
            .map_err(|_| hl_runtime::VectorError::Object(hl_descriptor::ObjectError::Io))?;
        let file = opened.as_ref().ok_or(hl_runtime::VectorError::Object(
            hl_descriptor::ObjectError::BadDescriptor,
        ))?;
        let result = if request.intent == FileIntent::Write {
            let lease = self
                .shm_lease
                .lock()
                .map_err(|_| hl_runtime::VectorError::Object(hl_descriptor::ObjectError::Io))?;
            if let Some(lease) = lease.as_ref() {
                let target = request.target(file).map_err(hl_runtime::VectorError::Object)?;
                lease
                    .external(file, target, |file| terminal(file.as_raw_fd()))
                    .map_err(hl_runtime::VectorError::Object)?
            } else {
                terminal(file.as_raw_fd())
            }
        } else {
            terminal(file.as_raw_fd())
        };
        if let Ok(count) = result {
            if request.intent == FileIntent::Write {
                self.publish_modified(count);
            }
            Ok(count)
        } else {
            result
        }
    }
}

struct FilePublication {
    registry: Arc<FileTransferRegistry>,
    file: Arc<ImportedFile>,
    identity: Option<DescriptionIdentity>,
}

impl TransferPublication for FilePublication {
    fn bind(&mut self, identities: &[DescriptionIdentity]) -> Result<(), RuntimeNetworkError> {
        let [identity] = identities else {
            return Err(RuntimeNetworkError::Invalid);
        };
        self.registry.bind_import(*identity, Arc::clone(&self.file))?;
        self.identity = Some(*identity);
        Ok(())
    }

    fn commit(mut self: Box<Self>) {
        self.identity = None;
    }

    fn rollback(mut self: Box<Self>) {
        self.unbind();
    }
}

impl FilePublication {
    fn unbind(&mut self) {
        if let Some(identity) = self.identity.take() {
            self.registry.remove(identity);
        }
    }
}

impl Drop for FilePublication {
    fn drop(&mut self) {
        self.unbind();
    }
}

struct ImportedFile {
    file: Mutex<File>,
    status: StatusFlags,
    splice_gate: Arc<CursorGate>,
}

impl ImportedFile {
    fn new(descriptor: OwnedFd) -> Result<Self, RuntimeNetworkError> {
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

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::{Read, Seek, Write};
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_descriptor::DescriptorTable;
    use hl_runtime::TransferCommitError;

    use super::FileTransferRegistry;

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    struct TemporaryFile(std::path::PathBuf);

    impl TemporaryFile {
        fn create() -> (Self, std::fs::File) {
            let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("hl-file-transfer-{}-{sequence}", std::process::id()));
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            file.write_all(b"abcdef").unwrap();
            file.rewind().unwrap();
            (Self(path), file)
        }
    }

    impl Drop for TemporaryFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn publish(registry: &Arc<FileTransferRegistry>, receiver: std::fs::File) -> (DescriptorTable, i32) {
        let table = DescriptorTable::new(4).unwrap();
        let transfer = registry.import(OwnedFd::from(receiver)).unwrap();
        let descriptors = transfer
            .prepare(&table, true)
            .unwrap()
            .publish_after(|_| Ok::<_, ()>(()))
            .unwrap();
        (table, descriptors[0])
    }

    #[test]
    fn sender_offset() {
        let (_temporary, sender) = TemporaryFile::create();
        let receiver = sender.try_clone().unwrap();
        drop(sender);
        let registry = Arc::new(FileTransferRegistry::default());
        let (table, number) = publish(&registry, receiver);
        let lease = table.pin(number).unwrap();
        let identity = lease.description_identity();
        let mut first = [0_u8; 2];
        assert_eq!(lease.read(&mut first), Ok(2));
        assert_eq!(&first, b"ab");

        let exported = registry.duplicate(identity).unwrap();
        // SAFETY: F_GETFD only observes the live owned descriptor.
        let flags = unsafe { libc::fcntl(exported.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        let mut duplicate = std::fs::File::from(exported);
        let mut second = [0_u8; 2];
        duplicate.read_exact(&mut second).unwrap();
        assert_eq!(&second, b"cd");
    }

    #[test]
    fn mapping_identity() {
        let (_temporary, sender) = TemporaryFile::create();
        let registry = Arc::new(FileTransferRegistry::default());
        let (table, number) = publish(&registry, sender.try_clone().unwrap());
        drop(sender);
        let lease = table.pin(number).unwrap();
        let expected = lease.metadata().unwrap();
        let mapping = registry.mapping(lease.description_identity()).unwrap();
        let actual = mapping.metadata().unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!((actual.dev(), actual.ino()), (expected.device, expected.inode));
    }

    #[test]
    fn copyout_rollback() {
        let (_temporary, sender) = TemporaryFile::create();
        let registry = Arc::new(FileTransferRegistry::default());
        let table = DescriptorTable::new(4).unwrap();
        let transfer = registry.import(OwnedFd::from(sender.try_clone().unwrap())).unwrap();
        let result = transfer.prepare(&table, false).unwrap().publish_after(|_| Err("fault"));
        assert_eq!(result, Err(TransferCommitError::Copyout("fault")));
        assert!(registry.files.lock().unwrap().is_empty());
        assert!(table.pin(0).is_err());
    }
}
