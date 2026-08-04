use crate::{FakeHost, FakeHostError, ResourceKind};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FileToken(pub u64);

pub struct StorageAdapter {
    host: FakeHost,
    files: Mutex<BTreeMap<FileToken, Vec<u8>>>,
    directories: Mutex<BTreeMap<FileToken, Vec<Vec<u8>>>>,
    maximum_transfer: usize,
}

impl StorageAdapter {
    #[must_use]
    pub fn new(host: FakeHost, maximum_transfer: usize) -> Self {
        Self {
            host,
            files: Mutex::new(BTreeMap::new()),
            directories: Mutex::new(BTreeMap::new()),
            maximum_transfer,
        }
    }

    pub fn create_directory(&self, mut entries: Vec<Vec<u8>>) -> Result<FileToken, FakeHostError> {
        entries.sort();
        let token = FileToken(self.host.allocate("directory", ResourceKind::Directory)?);
        self.directories
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(token, entries);
        Ok(token)
    }

    pub fn directory_snapshot(&self, token: FileToken) -> Result<Vec<Vec<u8>>, FakeHostError> {
        let entries = self
            .directories
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&token)
            .cloned()
            .ok_or(FakeHostError::InvalidResource)?;
        self.host
            .record("directory", "snapshot", token.0, entries.len(), entries.len())?;
        Ok(entries)
    }

    pub fn create(&self, bytes: Vec<u8>) -> Result<FileToken, FakeHostError> {
        let token = FileToken(self.host.allocate("file", ResourceKind::File)?);
        self.files
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(token, bytes);
        Ok(token)
    }

    pub fn read(&self, token: FileToken, offset: usize, output: &mut [u8]) -> Result<usize, FakeHostError> {
        let files = self.files.lock().unwrap_or_else(|error| error.into_inner());
        let file = files.get(&token).ok_or(FakeHostError::InvalidResource)?;
        let available = file.len().saturating_sub(offset);
        let count = available.min(output.len()).min(self.maximum_transfer);
        self.host.record("file", "read", token.0, output.len(), count)?;
        output[..count].copy_from_slice(&file[offset..offset + count]);
        Ok(count)
    }

    pub fn write(&self, token: FileToken, offset: usize, input: &[u8]) -> Result<usize, FakeHostError> {
        let count = input.len().min(self.maximum_transfer);
        self.host.record("file", "write", token.0, input.len(), count)?;
        let mut files = self.files.lock().unwrap_or_else(|error| error.into_inner());
        let file = files.get_mut(&token).ok_or(FakeHostError::InvalidResource)?;
        file.resize(file.len().max(offset + count), 0);
        file[offset..offset + count].copy_from_slice(&input[..count]);
        Ok(count)
    }

    pub fn close(&self, token: FileToken) -> Result<(), FakeHostError> {
        self.files
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&token)
            .ok_or(FakeHostError::InvalidResource)?;
        self.host.release("file", ResourceKind::File, token.0)
    }

    pub fn close_directory(&self, token: FileToken) -> Result<(), FakeHostError> {
        self.directories
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&token)
            .ok_or(FakeHostError::InvalidResource)?;
        self.host.release("directory", ResourceKind::Directory, token.0)
    }
}
