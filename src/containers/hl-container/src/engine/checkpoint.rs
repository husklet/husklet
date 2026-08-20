//! The engine's checkpoint image, carried over the container's own checkpoint storage.
//!
//! The engine asks for a single opaque object per domain; the container keeps a transactional,
//! deadline-bounded store. This adapter is the whole of the translation between them, including
//! which storage failures the engine is entitled to distinguish.

use super::{CHECKPOINT_MANIFEST_MAGIC, CHECKPOINT_OBJECT};
use std::sync::Arc;

pub(super) struct CheckpointTransport {
    image: Arc<dyn crate::CheckpointImage>,
}

impl CheckpointTransport {
    pub(super) fn new(image: Arc<dyn crate::CheckpointImage>) -> Self {
        Self { image }
    }

    fn storage_error(error: &crate::CheckpointError) -> hl_engine::composition::CompositionError {
        if error.is_deadline() {
            hl_engine::composition::CompositionError::DeadlineExceeded
        } else if error.is_busy() {
            hl_engine::composition::CompositionError::TransactionBusy
        } else if error.publication_occurred() {
            hl_engine::composition::CompositionError::PublishedNotDurable
        } else {
            hl_engine::composition::CompositionError::RuntimeConstruction
        }
    }
}

impl hl_engine::composition::CheckpointSink for CheckpointTransport {
    fn replace(&self, bytes: &[u8]) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        let deadline = std::time::Instant::now() + hl_engine::composition::DEFAULT_CHECKPOINT_TIMEOUT;
        let transaction = self
            .image
            .begin_until(deadline)
            .map_err(|error| Self::storage_error(&error))?;
        self.image
            .put_until(transaction, CHECKPOINT_OBJECT, bytes, deadline)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?;
        let mut manifest = Vec::with_capacity(16);
        manifest.extend_from_slice(CHECKPOINT_MANIFEST_MAGIC);
        manifest.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.image
            .commit_until(transaction, &manifest, deadline)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn begin_until(
        &self,
        deadline: std::time::Instant,
    ) -> std::result::Result<std::num::NonZeroU64, hl_engine::composition::CompositionError> {
        self.image
            .begin_until(deadline)
            .map_err(|error| Self::storage_error(&error))
    }

    fn put_until(
        &self,
        transaction: std::num::NonZeroU64,
        name: &str,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        self.image
            .put_until(transaction, name, bytes, deadline)
            .map_err(|error| Self::storage_error(&error))
    }

    fn abort_until(
        &self,
        transaction: std::num::NonZeroU64,
        deadline: std::time::Instant,
    ) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        self.image
            .abort_until(transaction, deadline)
            .map_err(|error| Self::storage_error(&error))
    }

    fn commit_until(
        &self,
        transaction: std::num::NonZeroU64,
        manifest: &[u8],
        deadline: std::time::Instant,
    ) -> std::result::Result<(), hl_engine::composition::CompositionError> {
        self.image
            .commit_until(transaction, manifest, deadline)
            .map_err(|error| Self::storage_error(&error))
    }
}

impl hl_engine::composition::CheckpointSource for CheckpointTransport {
    fn read(&self, maximum: usize) -> std::result::Result<Vec<u8>, hl_engine::composition::CompositionError> {
        let manifest = self
            .image
            .get("MANIFEST")
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?;
        if manifest.len() != 16 || &manifest[..8] != CHECKPOINT_MANIFEST_MAGIC {
            return Err(hl_engine::composition::CompositionError::RuntimeConstruction);
        }
        let length = u64::from_le_bytes(
            manifest[8..]
                .try_into()
                .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?,
        );
        let length =
            usize::try_from(length).map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?;
        if length > maximum {
            return Err(hl_engine::composition::CompositionError::RuntimeConstruction);
        }
        let bytes = self
            .image
            .get(CHECKPOINT_OBJECT)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)?;
        (bytes.len() == length)
            .then_some(bytes)
            .ok_or(hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn get(&self, name: &str) -> std::result::Result<Vec<u8>, hl_engine::composition::CompositionError> {
        self.image
            .get(name)
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn list(&self) -> std::result::Result<Vec<String>, hl_engine::composition::CompositionError> {
        self.image
            .list()
            .map_err(|_| hl_engine::composition::CompositionError::RuntimeConstruction)
    }

    fn get_until(
        &self,
        name: &str,
        deadline: std::time::Instant,
    ) -> std::result::Result<Vec<u8>, hl_engine::composition::CompositionError> {
        self.image
            .get_until(name, deadline)
            .map_err(|error| Self::storage_error(&error))
    }

    fn list_until(
        &self,
        deadline: std::time::Instant,
    ) -> std::result::Result<Vec<String>, hl_engine::composition::CompositionError> {
        self.image
            .list_until(deadline)
            .map_err(|error| Self::storage_error(&error))
    }
}
