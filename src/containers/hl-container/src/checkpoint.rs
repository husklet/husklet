use std::fmt;
use std::sync::Arc;

/// Failure from durable checkpoint object storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointError {
    message: String,
}

impl CheckpointError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for CheckpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CheckpointError {}

/// One complete, named process-tree checkpoint image.
pub trait CheckpointImage: Send + Sync {
    /// Stores one object in the unpublished checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage or object-name failure.
    fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CheckpointError>;

    /// Reads one object from the committed checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage, object-name, or missing-object failure.
    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError>;

    /// Lists objects in the committed checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage failure.
    fn list(&self) -> Result<Vec<String>, CheckpointError>;

    /// Publishes a complete generation after its manifest is durable.
    ///
    /// # Errors
    /// Returns a storage failure.
    fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError> {
        self.put("MANIFEST", manifest)
    }
}

/// Opens checkpoint images by stable container generation namespace.
pub trait CheckpointImages: Send + Sync {
    /// Opens one isolated checkpoint generation stream.
    ///
    /// # Errors
    /// Returns an invalid-namespace or storage failure.
    fn open(&self, namespace: &str) -> Result<Arc<dyn CheckpointImage>, CheckpointError>;
}

mod directory;

pub(crate) use directory::DirectoryImages;
