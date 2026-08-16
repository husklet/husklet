use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;

/// Failure from durable checkpoint object storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointError {
    message: String,
    deadline: bool,
    busy: bool,
    published: bool,
}

impl CheckpointError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            deadline: false,
            busy: false,
            published: false,
        }
    }

    #[must_use]
    pub fn deadline() -> Self {
        Self {
            message: "checkpoint storage deadline exceeded".into(),
            deadline: true,
            busy: false,
            published: false,
        }
    }

    #[must_use]
    pub fn busy() -> Self {
        Self {
            message: "checkpoint transaction is busy".into(),
            deadline: false,
            busy: true,
            published: false,
        }
    }

    #[must_use]
    pub(crate) fn published(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            deadline: false,
            busy: false,
            published: true,
        }
    }

    #[must_use]
    pub const fn is_deadline(&self) -> bool {
        self.deadline
    }

    #[must_use]
    pub(crate) const fn is_busy(&self) -> bool {
        self.busy
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
    /// Acquires exclusive ownership of one unpublished generation.
    fn begin_until(&self, deadline: std::time::Instant) -> Result<NonZeroU64, CheckpointError>;

    /// Stores one object in the unpublished checkpoint generation.
    ///
    /// # Errors
    /// Returns a storage or object-name failure.
    /// Cooperatively stores an object before an absolute monotonic deadline.
    /// Implementations must bound waits under their control. A blocking kernel
    /// filesystem call may still outlive the deadline and return an error after it
    /// completes; this API does not create or abandon background work.
    fn put_until(
        &self,
        transaction: NonZeroU64,
        name: &str,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CheckpointError>;

    /// Discards the unpublished generation without changing the generation
    /// visible through `get` and `list`.
    ///
    /// # Errors
    /// Returns a storage failure when the unpublished generation cannot be
    /// discarded completely.
    fn abort_until(&self, transaction: NonZeroU64, deadline: std::time::Instant) -> Result<(), CheckpointError>;

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

    /// Publishes only if the deadline is live immediately before the atomic
    /// pointer replacement. Once replacement starts, its result is authoritative:
    /// successful publication is never reported as a timeout.
    fn commit_until(
        &self,
        transaction: NonZeroU64,
        manifest: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CheckpointError>;
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
