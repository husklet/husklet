use super::{
    Disk, Entry, Error, File, JOURNAL_HEADER, JournalId, OpenOptions, Path, PathBuf, RECORD_LIMIT, Read, Result, Seek,
    Stream, Write, fs, now_ms,
};

impl Disk {
    pub(super) fn journal_slot(id: &JournalId) -> usize {
        let tag = match id {
            JournalId::Container(_) => 0_u8,
            JournalId::Exec(_) => 1_u8,
        };
        let mut value = 0xcbf2_9ce4_8422_2325_u64;
        for byte in std::iter::once(tag).chain(id.as_str().bytes()) {
            value ^= u64::from(byte);
            value = value.wrapping_mul(0x0000_0100_0000_01b3);
        }
        usize::try_from(value % u64::try_from(super::JOURNAL_STRIPES).expect("stripe count fits u64"))
            .expect("stripe index fits usize")
    }

    pub(super) fn journal_lock(&self, id: &JournalId) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.journal_stripes[Self::journal_slot(id)]
            .lock()
            .map_err(|_| Error::Corrupt("journal stripe lock poisoned".into()))
    }

    pub(super) fn log_path(&self, id: &JournalId) -> PathBuf {
        let directory = match id {
            JournalId::Container(_) => &self.directory,
            JournalId::Exec(_) => &self.execs,
        };
        directory.join(format!("{}.journal", id.as_str()))
    }

    pub(super) fn append_sync(&self, id: &JournalId, stream: Stream, bytes: Vec<u8>) -> Result<Entry> {
        let length = u64::try_from(bytes.len()).map_err(|_| Error::Corrupt("log record length exceeds u64".into()))?;
        if length > RECORD_LIMIT {
            return Err(Error::Corrupt(format!("log record exceeds {RECORD_LIMIT} bytes")));
        }
        let _stripe = self.journal_lock(id)?;
        let sequence = {
            let indexes = self
                .indexes
                .lock()
                .map_err(|_| Error::Corrupt("log cursor lock poisoned".into()))?;
            u64::try_from(indexes.get(id).map_or(0, Vec::len))
                .map_err(|_| Error::Corrupt("log index exceeds u64".into()))?
                .checked_add(1)
                .ok_or_else(|| Error::Corrupt("log sequence exhausted".into()))?
        };
        let mut file = OpenOptions::new().create(true).append(true).open(self.log_path(id))?;
        let offset = file.metadata()?.len();
        let timestamp_ms = now_ms();
        let mut header = [0_u8; JOURNAL_HEADER];
        header[..8].copy_from_slice(&sequence.to_le_bytes());
        header[8] = match stream {
            Stream::Stdout => 1,
            Stream::Stderr => 2,
        };
        header[9..17].copy_from_slice(&timestamp_ms.to_le_bytes());
        header[17..].copy_from_slice(&length.to_le_bytes());
        file.write_all(&header)?;
        file.write_all(&bytes)?;
        self.indexes
            .lock()
            .map_err(|_| Error::Corrupt("log cursor lock poisoned".into()))?
            .entry(id.clone())
            .or_default()
            .push(offset);
        Ok(Entry {
            sequence,
            timestamp_ms,
            stream,
            bytes,
        })
    }

    pub(super) fn entries_sync(&self, id: &JournalId) -> Result<Vec<Entry>> {
        let _stripe = self.journal_lock(id)?;
        Self::read_journal(&self.log_path(id))
    }

    pub(super) fn after_sync(&self, id: &JournalId, sequence: u64, limit: usize) -> Result<Vec<Entry>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let _stripe = self.journal_lock(id)?;
        let offset = {
            let indexes = self
                .indexes
                .lock()
                .map_err(|_| Error::Corrupt("log cursor lock poisoned".into()))?;
            let index = indexes.get(id).map(Vec::as_slice).unwrap_or_default();
            let position =
                usize::try_from(sequence).map_err(|_| Error::Corrupt("log cursor exceeds host range".into()))?;
            if position > index.len() {
                return Err(Error::Corrupt(format!(
                    "log cursor {sequence} exceeds journal length {}",
                    index.len()
                )));
            }
            let Some(offset) = index.get(position).copied() else {
                return Ok(Vec::new());
            };
            offset
        };
        Self::read_journal_at(
            &self.log_path(id),
            offset,
            sequence
                .checked_add(1)
                .ok_or_else(|| Error::Corrupt("log sequence exhausted".into()))?,
            limit,
        )
    }

    pub(super) fn cursor_sync(&self, id: &JournalId) -> Result<u64> {
        let _stripe = self.journal_lock(id)?;
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| Error::Corrupt("log cursor lock poisoned".into()))?;
        u64::try_from(indexes.get(id).map_or(0, Vec::len)).map_err(|_| Error::Corrupt("log index exceeds u64".into()))
    }

    pub(super) fn remove_journal_sync(&self, id: &JournalId) -> Result<()> {
        let _stripe = self.journal_lock(id)?;
        if let Err(error) = fs::remove_file(self.log_path(id)) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
        self.indexes
            .lock()
            .map_err(|_| Error::Corrupt("log cursor lock poisoned".into()))?
            .remove(id);
        Ok(())
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

    pub(super) fn read_journal_at(path: &Path, offset: u64, mut expected: u64, limit: usize) -> Result<Vec<Entry>> {
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
                    return Err(Error::Corrupt(format!("truncated log journal {}", path.display())));
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
                return Err(Error::Corrupt(format!("oversized log record in {}", path.display())));
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
            JournalId::exec(
                value
                    .parse()
                    .map_err(|error| Error::Corrupt(format!("invalid exec journal identity: {error}")))?,
            )
        } else {
            JournalId::container(
                value
                    .parse()
                    .map_err(|error| Error::Corrupt(format!("invalid container journal identity: {error}")))?,
            )
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
