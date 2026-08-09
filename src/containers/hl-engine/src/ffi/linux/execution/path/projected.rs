use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    DirectoryBatch, DirectoryBatchToken, ObjectError, OfdDirectoryEntry, OfdMetadata, OfdTimestamp,
    OpenFileDescription, PreparedSpliceRead, SeekPosition,
};
use hl_linux::OpenAbiPlan;
use hl_runtime::{DirectoryBaseLease, GuestPath, OpenIntent, PreparedPathOpen, RuntimePathError};

pub(super) use super::registry::Registry;
use super::registry::SlotReservation;
use super::source::ProjectedContext;
use super::splice::CursorGate;

mod error;
mod node;

use error::Error;
pub(super) use node::Node;

pub(super) struct Open;

impl Open {
    pub(super) fn prepare(
        context: &ProjectedContext,
        base: &DirectoryBaseLease,
        plan: &OpenAbiPlan,
        files: Registry,
    ) -> Result<Box<dyn PreparedPathOpen>, RuntimePathError> {
        if plan.intent.bits() & OpenIntent::TEMPORARY != 0 {
            return Err(RuntimePathError::Unsupported);
        }
        let registry = files.reserve()?;
        let path = Path::join(context.root(), base, plan.operand.path.as_bytes())?;
        let tree = Arc::clone(context.tree()?);
        let directory = plan.intent.bits() & OpenIntent::DIRECTORY != 0;
        let relative = plan.operand.path.as_bytes().first() != Some(&b'/');
        let base_handle = base
            .descriptor_lease()
            .and_then(|lease| lease.metadata().ok())
            .and_then(|metadata| files.get(&(metadata.device, metadata.inode)))
            .map(|file| file.handle);
        let nofollow = plan.intent.bits() & OpenIntent::NOFOLLOW != 0;
        let bits = plan.intent.bits();
        let options = hl_provider::TreeOpen {
            kind: if nofollow {
                hl_provider::TreeKind::Link
            } else if directory {
                hl_provider::TreeKind::Directory
            } else {
                hl_provider::TreeKind::File
            },
            read: bits & OpenIntent::READ != 0 || bits & OpenIntent::WRITE == 0,
            write: bits & OpenIntent::WRITE != 0,
            create: bits & OpenIntent::CREATE != 0,
            truncate: bits & OpenIntent::TRUNCATE != 0,
            append: bits & OpenIntent::APPEND != 0,
            exclusive: bits & OpenIntent::EXCLUSIVE != 0,
            mode: plan.mode,
        };
        let request_path = if relative && base_handle.is_some() {
            plan.operand.path.as_bytes()
        } else {
            path.as_slice()
        };
        let handle = tree
            .lock()
            .map_err(|_| RuntimePathError::Io)?
            .tree_open_options(base_handle.filter(|_| relative).unwrap_or(0), request_path, options)
            .map_err(Error::path)?;
        let stat = match tree
            .lock()
            .map_err(|_| RuntimePathError::Io)?
            .tree_stat(handle)
            .map_err(Error::path)
        {
            Ok(stat) => stat,
            Err(error) => {
                let _ = tree.lock().map(|mut worker| worker.tree_close(handle));
                return Err(error);
            }
        };
        if stat.mode & 0o170_000 == 0o120_000 && plan.intent.bits() & OpenIntent::PATH_ONLY == 0 {
            let _ = tree.lock().map(|mut worker| worker.tree_close(handle));
            return Err(RuntimePathError::Loop);
        }
        let file = Arc::new(File {
            tree,
            handle,
            stat,
            guest: path,
            cursor: Arc::new(Mutex::new(0)),
            splice_gate: Arc::new(CursorGate::default()),
            mapping: Mutex::new(None),
            directory: Mutex::new(directory.then(|| Directory::new(handle))),
            readable: options.read,
            writable: options.write,
            append: options.append,
            closed: AtomicBool::new(false),
        });
        Ok(Box::new(OpenTransaction {
            file,
            registry: Some(registry),
        }))
    }
}

pub(super) struct Path;

impl Path {
    pub(super) fn join(
        root: &GuestPath,
        base: &DirectoryBaseLease,
        operand: &[u8],
    ) -> Result<Vec<u8>, RuntimePathError> {
        if operand.is_empty() || operand.contains(&0) {
            return Err(RuntimePathError::Invalid);
        }
        let base_path = base.path().as_str().as_bytes();
        let mut value = if operand[0] == b'/' && !base.confines_root() {
            operand.to_vec()
        } else {
            let mut value = base_path.to_vec();
            if !value.ends_with(b"/") {
                value.push(b'/');
            }
            value.extend_from_slice(operand.strip_prefix(b"/").unwrap_or(operand));
            value
        };
        if root.as_str() != "/" {
            let mut rooted = root.as_str().trim_end_matches('/').as_bytes().to_vec();
            rooted.extend_from_slice(&value);
            value = rooted;
        }
        Ok(value)
    }
}

pub(super) struct File {
    tree: Arc<Mutex<crate::native::AuthorityWorker>>,
    handle: u64,
    stat: hl_provider::TreeStat,
    guest: Vec<u8>,
    cursor: Arc<Mutex<u64>>,
    splice_gate: Arc<CursorGate>,
    mapping: Mutex<Option<std::fs::File>>,
    directory: Mutex<Option<Directory>>,
    readable: bool,
    writable: bool,
    append: bool,
    closed: AtomicBool,
}

impl File {
    pub(super) const fn identity(&self) -> (u64, u64) {
        (self.stat.device, self.stat.inode)
    }

    pub(super) fn guest(&self) -> Result<GuestPath, RuntimePathError> {
        let path = std::str::from_utf8(&self.guest).map_err(|_| RuntimePathError::Invalid)?;
        GuestPath::new(path).map_err(|_| RuntimePathError::Invalid)
    }

    fn read_from(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        if !self.readable {
            return Err(ObjectError::BadDescriptor);
        }
        let bytes = self
            .tree
            .lock()
            .map_err(|_| ObjectError::Io)?
            .tree_read(self.handle, offset, output.len())
            .map_err(Error::object)?;
        output[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    fn write_from(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        if !self.writable {
            return Err(ObjectError::BadDescriptor);
        }
        if input.is_empty() {
            return Ok(0);
        }
        let input = &input[..input.len().min(hl_provider::TreeWire::MAX_WRITE_DATA)];
        let count = self
            .tree
            .lock()
            .map_err(|_| ObjectError::Io)?
            .tree_write(self.handle, offset, input)
            .map_err(Error::object)?;
        self.invalidate(count)?;
        Ok(count)
    }

    fn append_input(&self, input: &[u8]) -> Result<(usize, u64), ObjectError> {
        if !self.writable {
            return Err(ObjectError::BadDescriptor);
        }
        if input.is_empty() {
            let end = self
                .tree
                .lock()
                .map_err(|_| ObjectError::Io)?
                .tree_stat(self.handle)
                .map_err(Error::object)?
                .size;
            return Ok((0, end));
        }
        let input = &input[..input.len().min(hl_provider::TreeWire::MAX_APPEND_DATA)];
        let result = self
            .tree
            .lock()
            .map_err(|_| ObjectError::Io)?
            .tree_append(self.handle, input)
            .map_err(Error::object)?;
        self.invalidate(result.0)?;
        Ok(result)
    }

    fn invalidate(&self, count: usize) -> Result<(), ObjectError> {
        if count != 0 {
            *self.mapping.lock().map_err(|_| ObjectError::Io)? = None;
        }
        Ok(())
    }

    pub(super) fn mapping(&self) -> Result<std::fs::File, ObjectError> {
        let mut mapping = self.mapping.lock().map_err(|_| ObjectError::Io)?;
        if let Some(file) = mapping.as_ref() {
            return file.try_clone().map_err(|_| ObjectError::Io);
        }
        let size = self
            .tree
            .lock()
            .map_err(|_| ObjectError::Io)?
            .tree_stat(self.handle)
            .map_err(Error::object)?
            .size;
        let file =
            super::materialization::Materialization::copy(size, |offset, output| self.read_from(offset, output))?;
        *mapping = Some(file.try_clone().map_err(|_| ObjectError::Io)?);
        Ok(file)
    }
}

impl fmt::Debug for File {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProjectedFile")
    }
}

impl OpenFileDescription for File {
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.splice_gate.enter();
        let mut cursor = self.cursor.lock().map_err(|_| ObjectError::Io)?;
        let count = self.read_from(*cursor, output)?;
        *cursor = cursor.checked_add(count as u64).ok_or(ObjectError::InvalidArgument)?;
        Ok(count)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.read_from(offset, output)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.splice_gate.enter();
        if !self.writable {
            return Err(ObjectError::BadDescriptor);
        }
        let mut cursor = self.cursor.lock().map_err(|_| ObjectError::Io)?;
        let count = if self.append {
            let (count, end) = self.append_input(input)?;
            *cursor = end;
            count
        } else {
            let count = self.write_from(*cursor, input)?;
            *cursor = cursor.checked_add(count as u64).ok_or(ObjectError::InvalidArgument)?;
            count
        };
        Ok(count)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        if self.append {
            self.append_input(input).map(|(count, _)| count)
        } else {
            self.write_from(offset, input)
        }
    }

    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        self.splice_gate.enter();
        let mut cursor = self.cursor.lock().map_err(|_| ObjectError::Io)?;
        let next = match position {
            SeekPosition::Start(value) => i128::from(value),
            SeekPosition::Current(value) => i128::from(*cursor) + i128::from(value),
            SeekPosition::End(value) => {
                let size = self
                    .tree
                    .lock()
                    .map_err(|_| ObjectError::Io)?
                    .tree_stat(self.handle)
                    .map_err(Error::object)?
                    .size;
                i128::from(size) + i128::from(value)
            }
            SeekPosition::Data(_) | SeekPosition::Hole(_) => return Err(ObjectError::NotSupported),
        };
        *cursor = u64::try_from(next).map_err(|_| ObjectError::InvalidArgument)?;
        Ok(*cursor)
    }

    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        let implicit = offset.is_none();
        let mut bytes = vec![0; maximum.min(hl_provider::TreeWire::MAX_DATA)];
        let cursor = Arc::clone(&self.cursor);
        let start = Arc::new(Mutex::new(None));
        let prepared_start = Arc::clone(&start);
        let prepared = self.splice_gate.prepare(
            implicit,
            nonblocking,
            cancellation,
            || {
                let value =
                    offset.unwrap_or_else(|| *self.cursor.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                *prepared_start.lock().map_err(|_| ObjectError::Io)? = Some(value);
                let count = self.read_from(value, &mut bytes)?;
                bytes.truncate(count);
                Ok(bytes)
            },
            move |count| {
                if implicit {
                    let value = start
                        .lock()
                        .map_err(|_| ObjectError::Io)?
                        .ok_or(ObjectError::Interrupted)?;
                    *cursor.lock().map_err(|_| ObjectError::Io)? =
                        value.checked_add(count as u64).ok_or(ObjectError::InvalidArgument)?;
                }
                Ok(())
            },
        )?;
        Ok(Some(prepared))
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let stat = self
            .tree
            .lock()
            .map_err(|_| ObjectError::Io)?
            .tree_stat(self.handle)
            .map_err(Error::object)?;
        let zero = OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(OfdMetadata {
            device: stat.device,
            inode: stat.inode,
            kind: if stat.mode & 0o170_000 == 0o040_000 { 4 } else { 8 },
            permissions: (stat.mode & 0o7777) as u16,
            links: 1,
            user: 0,
            group: 0,
            special_device: 0,
            size: stat.size,
            blocks_512: stat.size.div_ceil(512),
            block_size: 4096,
            accessed: zero,
            modified: zero,
            changed: zero,
        })
    }

    fn read_directory(&self, maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        let mut directory = self.directory.lock().map_err(|_| ObjectError::Io)?;
        directory
            .as_mut()
            .ok_or(ObjectError::NotSupported)?
            .read(&self.tree, maximum)
    }

    fn commit_directory(&self, token: DirectoryBatchToken, count: usize) -> Result<(), ObjectError> {
        let mut directory = self.directory.lock().map_err(|_| ObjectError::Io)?;
        directory
            .as_mut()
            .ok_or(ObjectError::NotSupported)?
            .commit(token, count)
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let _ = self.tree.lock().map(|mut worker| worker.tree_close(self.handle));
        }
    }

    fn truncate(&self, size: u64) -> Result<(), ObjectError> {
        if !self.writable {
            return Err(ObjectError::BadDescriptor);
        }
        self.tree
            .lock()
            .map_err(|_| ObjectError::Io)?
            .tree_truncate(self.handle, size)
            .map_err(Error::object)?;
        *self.mapping.lock().map_err(|_| ObjectError::Io)? = None;
        Ok(())
    }
}

struct Directory {
    handle: u64,
    entries: Vec<OfdDirectoryEntry>,
    index: usize,
    cursor: i64,
    eof: bool,
}

impl Directory {
    fn new(handle: u64) -> Self {
        Self {
            handle,
            entries: Vec::new(),
            index: 0,
            cursor: 0,
            eof: false,
        }
    }

    fn read(
        &mut self,
        tree: &Arc<Mutex<crate::native::AuthorityWorker>>,
        maximum: usize,
    ) -> Result<DirectoryBatch, ObjectError> {
        if self.index == self.entries.len() && !self.eof {
            let bytes = tree
                .lock()
                .map_err(|_| ObjectError::Io)?
                .tree_entries(self.handle, hl_provider::TreeWire::MAX_DATA)
                .map_err(Error::object)?;
            self.eof = bytes.is_empty();
            self.entries = super::entries::Entries::parse(&bytes)?;
            self.index = 0;
        }
        Ok(DirectoryBatch {
            token: DirectoryBatchToken {
                generation: self.handle,
                cookie: self.cursor,
            },
            entries: self.entries[self.index..].iter().take(maximum).cloned().collect(),
        })
    }

    fn commit(&mut self, token: DirectoryBatchToken, count: usize) -> Result<(), ObjectError> {
        if token.generation != self.handle || token.cookie != self.cursor {
            return Err(ObjectError::InvalidArgument);
        }
        self.index = self
            .index
            .checked_add(count)
            .filter(|index| *index <= self.entries.len())
            .ok_or(ObjectError::InvalidArgument)?;
        self.cursor = self
            .cursor
            .checked_add(count as i64)
            .ok_or(ObjectError::ResourceLimit)?;
        Ok(())
    }
}

struct OpenTransaction {
    file: Arc<File>,
    registry: Option<SlotReservation>,
}

impl fmt::Debug for OpenTransaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProjectedOpen")
    }
}

impl PreparedPathOpen for OpenTransaction {
    fn object(&self) -> Arc<dyn OpenFileDescription> {
        self.file.clone()
    }
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        self.registry.take().ok_or(RuntimePathError::Io)?.commit(&self.file)
    }
    fn rollback(self: Box<Self>) {
        self.file.close();
    }
}
