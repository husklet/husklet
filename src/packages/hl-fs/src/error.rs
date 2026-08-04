use std::{
    error::Error,
    fmt, io,
    path::{Path, PathBuf},
};

/// Filesystem operation failure with its path and semantic boundary preserved.
#[derive(Debug)]
pub enum FsError {
    /// A host filesystem operation failed.
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Path on which the operation was attempted.
        path: PathBuf,
        /// Host error.
        source: io::Error,
    },
    /// A bounded read contained more bytes than permitted.
    LimitExceeded {
        /// Path being read.
        path: PathBuf,
        /// Maximum accepted byte count.
        limit: usize,
    },
    /// A root path does not name a directory.
    NotDirectory(PathBuf),
    /// A relative path was absolute or contained a parent traversal.
    PathEscape(PathBuf),
    /// Canonical resolution escaped through an existing symlink.
    SymlinkEscape(PathBuf),
    /// Stable file identity is unavailable from this host object.
    IdentityUnavailable(PathBuf),
    /// Safe standard-library APIs cannot provide the requested durability.
    DirectoryDurabilityUnsupported(PathBuf),
    /// A unique sibling could not be allocated within the bounded retry count.
    TemporarySiblingExhausted(PathBuf),
}

impl FsError {
    pub(crate) fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_owned(),
            source,
        }
    }
}

impl fmt::Display for FsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "failed to {operation} {}: {source}", path.display()),
            Self::LimitExceeded { path, limit } => {
                write!(formatter, "{} exceeds the {limit}-byte limit", path.display())
            }
            Self::NotDirectory(path) => write!(formatter, "{} is not a directory", path.display()),
            Self::PathEscape(path) => {
                write!(formatter, "{} is not a rooted relative path", path.display())
            }
            Self::SymlinkEscape(path) => {
                write!(formatter, "{} resolves outside its root", path.display())
            }
            Self::IdentityUnavailable(path) => {
                write!(formatter, "stable identity is unavailable for {}", path.display())
            }
            Self::DirectoryDurabilityUnsupported(path) => write!(
                formatter,
                "safe parent-directory synchronization is unavailable for {}",
                path.display()
            ),
            Self::TemporarySiblingExhausted(path) => {
                write!(
                    formatter,
                    "could not allocate a temporary sibling for {}",
                    path.display()
                )
            }
        }
    }
}

impl Error for FsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Result returned by host-filesystem mechanisms.
pub type Result<T> = std::result::Result<T, FsError>;
