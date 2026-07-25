use super::{
    fs, now_ms, Disk, Entry, Error, File, JournalId, OpenOptions, Path, PathBuf, Read, Result,
    Seek, Stream, Write, JOURNAL_HEADER, RECORD_LIMIT,
};

impl Disk {
    pub(super) fn log_path(&self, id: &JournalId) -> PathBuf {
        let directory = match id {
            JournalId::Container(_) => &self.directory,
            JournalId::Exec(_) => &self.execs,
        };
        directory.join(format!("{}.journal", id.as_str()))
    }

    pub(super) fn append_sync(
        &self,
        id: &JournalId,
        stream: Stream,
        bytes: &[u8],
    ) -> Result<Entry> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| Error::Corrupt("log record length exceeds u64".into()))?;
        if length > RECORD_LIMIT {
            return Err(Error::Corrupt(format!(
                "log record exceeds {RECORD_LIMIT} bytes"
            )));
        }
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| Error::Corrupt("log cursor lock poisoned".into()))?;
        let index = indexes.entry(id.clone()).or_default();
        let sequence = u64::try_from(index.len())
            .map_err(|_| Error::Corrupt("log index exceeds u64".into()))?
            .checked_add(1)
            .ok_or_else(|| Error::Corrupt("log sequence exhausted".into()))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path(id))?;
        let offset = file.metadata()?.len();
        let timestamp_ms = now_ms();
        file.write_all(&sequence.to_le_bytes())?;
        file.write_all(&[match stream {
            Stream::Stdout => 1,
            Stream::Stderr => 2,
        }])?;
        file.write_all(&timestamp_ms.to_le_bytes())?;
        file.write_all(&length.to_le_bytes())?;
        file.write_all(bytes)?;
        index.push(offset);
        Ok(Entry {
            sequence,
            timestamp_ms,
            stream,
            bytes: bytes.to_vec(),
        })
    }

    pub(super) fn entries_sync(&self, id: &JournalId) -> Result<Vec<Entry>> {
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        Self::read_journal(&self.log_path(id))
    }

    pub(super) fn after_sync(
        &self,
        id: &JournalId,
        sequence: u64,
        limit: usize,
    ) -> Result<Vec<Entry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let _guard = self
            .transaction
            .lock()
            .map_err(|_| Error::Corrupt("repository lock poisoned".into()))?;
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| Error::Corrupt("log cursor lock poisoned".into()))?;
        let index = indexes.get(id).map(Vec::as_slice).unwrap_or_default();
        let position = usize::try_from(sequence)
            .map_err(|_| Error::Corrupt("log cursor exceeds host range".into()))?;
        if position > index.len() {
            return Err(Error::Corrupt(format!(
                "log cursor {sequence} exceeds journal length {}",
                index.len()
            )));
        }
        let Some(offset) = index.get(position).copied() else {
            return Ok(Vec::new());
        };
        drop(indexes);
        Self::read_journal_at(
            &self.log_path(id),
            offset,
            sequence
                .checked_add(1)
                .ok_or_else(|| Error::Corrupt("log sequence exhausted".into()))?,
            limit,
        )
    }

    pub(super) async fn blocking<T: Send + 'static>(
        operation: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        tokio::task::spawn_blocking(operation)
            .await
            .map_err(|error| Error::Io(std::io::Error::other(error)))?
    }

    pub(super) fn remove_temporary(directory: &Path) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension() != Some(std::ffi::OsStr::new("tmp")) {
                continue;
            }
            fs::remove_file(path)?;
        }
        Self::sync_directory(directory)
    }

    pub(super) fn read_journal(path: &Path) -> Result<Vec<Entry>> {
        Self::read_journal_at(path, 0, 1, usize::MAX)
    }

    pub(super) fn read_journal_at(
        path: &Path,
        offset: u64,
        mut expected: u64,
        limit: usize,
    ) -> Result<Vec<Entry>> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        file.seek(std::io::SeekFrom::Start(offset))?;
        let mut entries = Vec::new();
        loop {
            if entries.len() == limit {
                return Ok(entries);
            }
            let mut header = [0_u8; JOURNAL_HEADER];
            let start = file.stream_position()?;
            match file.read_exact(&mut header) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    if start == file.metadata()?.len() {
                        return Ok(entries);
                    }
                    return Err(Error::Corrupt(format!(
                        "truncated log journal {}",
                        path.display()
                    )));
                }
                Err(error) => return Err(error.into()),
            }
            let sequence = u64::from_le_bytes(header[..8].try_into().expect("fixed header"));
            if sequence != expected {
                return Err(Error::Corrupt(format!(
                    "non-contiguous log sequence in {}",
                    path.display()
                )));
            }
            expected = expected
                .checked_add(1)
                .ok_or_else(|| Error::Corrupt("log sequence exhausted".into()))?;
            let stream = match header[8] {
                1 => Stream::Stdout,
                2 => Stream::Stderr,
                value => {
                    return Err(Error::Corrupt(format!(
                        "invalid log stream {value} in {}",
                        path.display()
                    )));
                }
            };
            let timestamp_ms = u64::from_le_bytes(header[9..17].try_into().expect("fixed header"));
            let length = u64::from_le_bytes(header[17..].try_into().expect("fixed header"));
            if length > RECORD_LIMIT {
                return Err(Error::Corrupt(format!(
                    "oversized log record in {}",
                    path.display()
                )));
            }
            let mut bytes = vec![0; usize::try_from(length).expect("record limit fits usize")];
            file.read_exact(&mut bytes).map_err(|error| {
                if error.kind() == std::io::ErrorKind::UnexpectedEof {
                    Error::Corrupt(format!("truncated log journal {}", path.display()))
                } else {
                    error.into()
                }
            })?;
            entries.push(Entry {
                sequence,
                timestamp_ms,
                stream,
                bytes,
            });
        }
    }

    pub(super) fn sync_directory(directory: &Path) -> Result<()> {
        File::open(directory)?.sync_all().map_err(Error::Io)
    }
}

pub(super) fn initialize(
    directory: &Path,
    execs: &Path,
    volumes: &Path,
    networks: &Path,
) -> Result<std::collections::BTreeMap<JournalId, Vec<u64>>> {
    fs::create_dir_all(directory)?;
    fs::create_dir_all(execs)?;
    fs::create_dir_all(volumes)?;
    fs::create_dir_all(networks)?;
    Disk::remove_temporary(execs)?;
    Disk::remove_temporary(volumes)?;
    Disk::remove_temporary(networks)?;
    let mut indexes = std::collections::BTreeMap::new();
    index_journals(directory, false, &mut indexes)?;
    index_journals(execs, true, &mut indexes)?;
    Disk::sync_directory(directory)?;
    Disk::sync_directory(execs)?;
    Disk::sync_directory(volumes)?;
    Disk::sync_directory(networks)?;
    Ok(indexes)
}

pub(super) fn index_journals(
    directory: &Path,
    execution: bool,
    indexes: &mut std::collections::BTreeMap<JournalId, Vec<u64>>,
) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension() == Some(std::ffi::OsStr::new("tmp")) {
            fs::remove_file(path)?;
            continue;
        }
        if path.extension() != Some(std::ffi::OsStr::new("journal")) {
            continue;
        }
        let value = path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| Error::Corrupt(format!("invalid journal path {}", path.display())))?;
        let id = if execution {
            JournalId::exec(value.parse().map_err(|error| {
                Error::Corrupt(format!("invalid exec journal identity: {error}"))
            })?)
        } else {
            JournalId::container(value.parse().map_err(|error| {
                Error::Corrupt(format!("invalid container journal identity: {error}"))
            })?)
        };
        let entries = Disk::read_journal(&path)?;
        let mut offset = 0_u64;
        let mut index = Vec::with_capacity(entries.len());
        for entry in entries {
            index.push(offset);
            offset = offset
                .checked_add(JOURNAL_HEADER as u64)
                .and_then(|value| value.checked_add(entry.bytes.len() as u64))
                .ok_or_else(|| Error::Corrupt("log journal size overflow".into()))?;
        }
        indexes.insert(id, index);
    }
    Ok(())
}
