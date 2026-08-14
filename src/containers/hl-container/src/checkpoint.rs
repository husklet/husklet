use std::fmt;
use std::sync::Arc;

/// Failure from durable checkpoint object storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointError {
    message: String,
    deadline: bool,
    published: bool,
}

impl CheckpointError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            deadline: false,
            published: false,
        }
    }

    #[must_use]
    pub fn deadline() -> Self {
        Self {
            message: "checkpoint storage deadline exceeded".into(),
            deadline: true,
            published: false,
        }
    }

    #[must_use]
    pub(crate) fn published(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            deadline: false,
            published: true,
        }
    }

    #[must_use]
    pub const fn is_deadline(&self) -> bool {
        self.deadline
    }

    #[must_use]
    pub(crate) const fn publication_occurred(&self) -> bool {
        self.published
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

    /// Cooperatively stores an object before an absolute monotonic deadline.
    /// Implementations must bound waits under their control. A blocking kernel
    /// filesystem call may still outlive the deadline and return an error after it
    /// completes; this API does not create or abandon background work.
    fn put_until(&self, name: &str, bytes: &[u8], deadline: std::time::Instant) -> Result<(), CheckpointError>;

    /// Reads one object from the committed checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage, object-name, or missing-object failure.
    fn get(&self, name: &str) -> Result<Vec<u8>, CheckpointError>;

    /// Cooperatively reads an object before an absolute monotonic deadline.
    fn get_until(&self, name: &str, deadline: std::time::Instant) -> Result<Vec<u8>, CheckpointError>;

    /// Lists objects in the committed checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage failure.
    fn list(&self) -> Result<Vec<String>, CheckpointError>;

    /// Cooperatively lists objects before an absolute monotonic deadline.
    fn list_until(&self, deadline: std::time::Instant) -> Result<Vec<String>, CheckpointError>;

    /// Publishes a complete generation after its manifest is durable.
    ///
    /// # Errors
    /// Returns a storage failure.
    fn commit(&self, manifest: &[u8]) -> Result<(), CheckpointError> {
        self.put("MANIFEST", manifest)
    }

    /// Publishes only if the deadline is live immediately before the atomic
    /// pointer replacement. Once replacement starts, its result is authoritative:
    /// successful publication is never reported as a timeout.
    fn commit_until(&self, manifest: &[u8], deadline: std::time::Instant) -> Result<(), CheckpointError>;
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
