use std::fmt;
use std::sync::Mutex;

use hl_descriptor::{
    DirectoryBatch, DirectoryBatchToken, ObjectError, ObjectKind, OfdDirectoryEntry, OfdMetadata, OpenFileDescription,
    SeekPosition,
};

pub(super) struct SnapshotFile {
    bytes: Vec<u8>,
    cursor: Mutex<usize>,
    metadata: OfdMetadata,
}

pub(super) struct UtsFile {
    source: std::sync::Arc<dyn super::Source>,
    namespace: u64,
    domain: bool,
    cursor: Mutex<usize>,
    metadata: OfdMetadata,
}

pub(super) struct CommFile {
    source: std::sync::Arc<dyn super::Source>,
    process: u32,
    thread: Option<u32>,
    cursor: Mutex<usize>,
    metadata: OfdMetadata,
}

pub(super) struct OomFile {
    source: std::sync::Arc<dyn super::Source>,
    process: u32,
    cursor: Mutex<usize>,
    metadata: OfdMetadata,
}

impl OomFile {
    pub(super) const fn new(source: std::sync::Arc<dyn super::Source>, process: u32, metadata: OfdMetadata) -> Self {
        Self {
            source,
            process,
            cursor: Mutex::new(0),
            metadata,
        }
    }

    fn bytes(&self) -> Result<Vec<u8>, ObjectError> {
        self.source
            .oom_score_adj(self.process)
            .map(|value| format!("{value}\n").into_bytes())
            .map_err(|_| ObjectError::Retired)
    }
}

impl fmt::Debug for OomFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcfsOomFile")
    }
}

impl OpenFileDescription for OomFile {
    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let mut metadata = self.metadata.clone();
        metadata.size = self.bytes()?.len() as u64;
        Ok(metadata)
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        let bytes = self.bytes()?;
        let mut cursor = self.cursor.lock().unwrap_or_else(|error| error.into_inner());
        let count = output.len().min(bytes.len().saturating_sub(*cursor));
        output[..count].copy_from_slice(&bytes[*cursor..*cursor + count]);
        *cursor += count;
        Ok(count)
    }

    fn write_context(&self, input: &[u8], context: hl_descriptor::OperationContext<'_>) -> Result<usize, ObjectError> {
        let actor = context.actor.ok_or(ObjectError::PermissionDenied)?;
        let value = input.strip_suffix(b"\n").unwrap_or(input);
        let value = std::str::from_utf8(value)
            .ok()
            .and_then(|value| value.parse::<i16>().ok())
            .filter(|value| (-1000..=1000).contains(value))
            .ok_or(ObjectError::InvalidArgument)?;
        self.source.write_oom_score_adj(self.process, actor, value)?;
        *self.cursor.lock().unwrap_or_else(|error| error.into_inner()) += input.len();
        Ok(input.len())
    }

    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        let length = self.bytes()?.len();
        let mut cursor = self.cursor.lock().unwrap_or_else(|error| error.into_inner());
        let next = match position {
            SeekPosition::Start(value) => i128::from(value),
            SeekPosition::Current(value) => *cursor as i128 + i128::from(value),
            SeekPosition::End(value) => length as i128 + i128::from(value),
            SeekPosition::Data(_) | SeekPosition::Hole(_) => return Err(ObjectError::InvalidArgument),
        };
        *cursor = usize::try_from(next).map_err(|_| ObjectError::InvalidArgument)?;
        Ok(*cursor as u64)
    }
}

impl CommFile {
    pub(super) fn new(
        source: std::sync::Arc<dyn super::Source>,
        process: u32,
        thread: Option<u32>,
        metadata: OfdMetadata,
    ) -> Self {
        Self {
            source,
            process,
            thread,
            cursor: Mutex::new(0),
            metadata,
        }
    }

    fn bytes(&self) -> Result<Vec<u8>, ObjectError> {
        self.source
            .comm(self.process, self.thread)
            .map_err(|_| ObjectError::Retired)
    }
}

impl fmt::Debug for CommFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcfsCommFile")
    }
}

impl OpenFileDescription for CommFile {
    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let mut metadata = self.metadata.clone();
        metadata.size = self.bytes()?.len() as u64;
        Ok(metadata)
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        let bytes = self.bytes()?;
        let mut cursor = self.cursor.lock().unwrap_or_else(|error| error.into_inner());
        let count = output.len().min(bytes.len().saturating_sub(*cursor));
        output[..count].copy_from_slice(&bytes[*cursor..*cursor + count]);
        *cursor += count;
        Ok(count)
    }

    fn write_context(&self, input: &[u8], context: hl_descriptor::OperationContext<'_>) -> Result<usize, ObjectError> {
        if input.is_empty() {
            return Ok(0);
        }
        let actor = context.actor.ok_or(ObjectError::PermissionDenied)?;
        let limit = input.len().min(15);
        let end = input[..limit]
            .iter()
            .position(|byte| matches!(byte, 0 | b'\n'))
            .unwrap_or(limit);
        self.source
            .write_comm(self.process, self.thread, actor, &input[..end])?;
        *self.cursor.lock().unwrap_or_else(|error| error.into_inner()) = 0;
        Ok(input.len())
    }

    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        let length = self.bytes()?.len();
        let mut cursor = self.cursor.lock().unwrap_or_else(|error| error.into_inner());
        let next = match position {
            SeekPosition::Start(value) => i128::from(value),
            SeekPosition::Current(value) => *cursor as i128 + i128::from(value),
            SeekPosition::End(value) => length as i128 + i128::from(value),
            SeekPosition::Data(_) | SeekPosition::Hole(_) => return Err(ObjectError::InvalidArgument),
        };
        *cursor = usize::try_from(next).map_err(|_| ObjectError::InvalidArgument)?;
        Ok(*cursor as u64)
    }
}

impl UtsFile {
    pub(super) fn new(
        source: std::sync::Arc<dyn super::Source>,
        namespace: u64,
        domain: bool,
        metadata: OfdMetadata,
    ) -> Self {
        Self {
            source,
            namespace,
            domain,
            cursor: Mutex::new(0),
            metadata,
        }
    }

    fn bytes(&self) -> Result<Vec<u8>, ObjectError> {
        let uts = self
            .source
            .uts_namespace(self.namespace)
            .map_err(|_| ObjectError::Retired)?;
        let mut bytes = if self.domain { uts.domainname } else { uts.hostname };
        bytes.push(b'\n');
        Ok(bytes)
    }
}

impl fmt::Debug for UtsFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcfsUtsFile")
    }
}

impl OpenFileDescription for UtsFile {
    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        Ok(self.metadata.clone())
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        let bytes = self.bytes()?;
        let mut cursor = self.cursor.lock().unwrap_or_else(|error| error.into_inner());
        let count = output.len().min(bytes.len().saturating_sub(*cursor));
        output[..count].copy_from_slice(&bytes[*cursor..*cursor + count]);
        *cursor += count;
        Ok(count)
    }

    fn write_context(&self, input: &[u8], context: hl_descriptor::OperationContext<'_>) -> Result<usize, ObjectError> {
        let actor = context.actor.ok_or(ObjectError::PermissionDenied)?;
        let mut cursor = self.cursor.lock().unwrap_or_else(|error| error.into_inner());
        if *cursor != 0 {
            return Err(ObjectError::InvalidArgument);
        }
        let value = input.strip_suffix(b"\n").unwrap_or(input);
        if value.len() > 64 {
            return Err(ObjectError::InvalidArgument);
        }
        self.source.write_uts(self.namespace, self.domain, actor, value)?;
        *cursor = input.len();
        Ok(input.len())
    }
}

pub(super) struct SnapshotDirectory {
    entries: Vec<OfdDirectoryEntry>,
    state: Mutex<(u64, usize)>,
    metadata: OfdMetadata,
}

impl SnapshotDirectory {
    pub(super) fn entries(source: impl IntoIterator<Item = (Vec<u8>, u8)>, metadata: OfdMetadata) -> Self {
        let mut entries = vec![
            OfdDirectoryEntry {
                inode: metadata.inode,
                cookie: 1,
                file_type: 4,
                name: b".".to_vec(),
            },
            OfdDirectoryEntry {
                inode: metadata.inode,
                cookie: 2,
                file_type: 4,
                name: b"..".to_vec(),
            },
        ];
        entries.extend(
            source
                .into_iter()
                .enumerate()
                .map(|(index, (name, file_type))| OfdDirectoryEntry {
                    inode: u64::try_from(index + 3).unwrap_or(u64::MAX),
                    cookie: i64::try_from(index + 3).unwrap_or(i64::MAX),
                    file_type,
                    name,
                }),
        );
        Self {
            entries,
            state: Mutex::new((1, 0)),
            metadata,
        }
    }

    pub(super) fn new(numbers: impl IntoIterator<Item = i32>, file_type: u8, metadata: OfdMetadata) -> Self {
        let mut entries = vec![
            OfdDirectoryEntry {
                inode: metadata.inode,
                cookie: 1,
                file_type: 4,
                name: b".".to_vec(),
            },
            OfdDirectoryEntry {
                inode: metadata.inode,
                cookie: 2,
                file_type: 4,
                name: b"..".to_vec(),
            },
        ];
        entries.extend(
            numbers
                .into_iter()
                .enumerate()
                .map(|(index, number)| OfdDirectoryEntry {
                    inode: u64::try_from(number).unwrap_or(0),
                    cookie: i64::try_from(index + 3).unwrap_or(i64::MAX),
                    file_type,
                    name: number.to_string().into_bytes(),
                }),
        );
        Self {
            entries,
            state: Mutex::new((1, 0)),
            metadata,
        }
    }

    pub(super) fn names(names: impl IntoIterator<Item = Vec<u8>>, metadata: OfdMetadata) -> Self {
        Self::entries(names.into_iter().map(|name| (name, 4)), metadata)
    }
}

impl fmt::Debug for SnapshotDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcfsSnapshotDirectory")
    }
}

impl OpenFileDescription for SnapshotDirectory {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Directory
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        Ok(self.metadata.clone())
    }

    fn read_directory(&self, maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        Ok(DirectoryBatch {
            token: DirectoryBatchToken {
                generation: state.0,
                cookie: i64::try_from(state.1).map_err(|_| ObjectError::InvalidArgument)?,
            },
            entries: self.entries.iter().skip(state.1).take(maximum).cloned().collect(),
        })
    }

    fn commit_directory(&self, token: DirectoryBatchToken, count: usize) -> Result<(), ObjectError> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if token.generation != state.0 || token.cookie != state.1 as i64 {
            return Err(ObjectError::InvalidArgument);
        }
        state.1 = state
            .1
            .checked_add(count)
            .filter(|position| *position <= self.entries.len())
            .ok_or(ObjectError::InvalidArgument)?;
        Ok(())
    }
}

impl SnapshotFile {
    pub(super) const fn new(bytes: Vec<u8>, metadata: OfdMetadata) -> Self {
        Self {
            bytes,
            cursor: Mutex::new(0),
            metadata,
        }
    }
}

impl fmt::Debug for SnapshotFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcfsSnapshotFile")
    }
}

impl OpenFileDescription for SnapshotFile {
    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        Ok(self.metadata.clone())
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        let mut cursor = self.cursor.lock().unwrap_or_else(|error| error.into_inner());
        let count = output.len().min(self.bytes.len().saturating_sub(*cursor));
        output[..count].copy_from_slice(&self.bytes[*cursor..*cursor + count]);
        *cursor += count;
        Ok(count)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        let offset = usize::try_from(offset).map_err(|_| ObjectError::InvalidArgument)?;
        let count = output.len().min(self.bytes.len().saturating_sub(offset));
        output[..count].copy_from_slice(&self.bytes[offset..offset + count]);
        Ok(count)
    }

    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        let mut cursor = self.cursor.lock().unwrap_or_else(|error| error.into_inner());
        let next = match position {
            SeekPosition::Start(value) => i128::from(value),
            SeekPosition::Current(value) => *cursor as i128 + i128::from(value),
            SeekPosition::End(value) => self.bytes.len() as i128 + i128::from(value),
            SeekPosition::Data(_) | SeekPosition::Hole(_) => return Err(ObjectError::InvalidArgument),
        };
        *cursor = usize::try_from(next).map_err(|_| ObjectError::InvalidArgument)?;
        Ok(*cursor as u64)
    }
}
