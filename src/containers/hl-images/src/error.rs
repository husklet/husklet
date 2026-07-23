use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid image reference: {0}")]
    InvalidReference(String),
    #[error("invalid digest: {0}")]
    InvalidDigest(String),
    #[error("content digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("content size mismatch: expected {expected}, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("content not found: {0}")]
    ContentNotFound(String),
    #[error("unsafe archive entry {path:?}: {reason}")]
    UnsafeArchive { path: PathBuf, reason: &'static str },
    #[error("lease {lease} does not own {resource}")]
    NotOwned { lease: String, resource: String },
    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),
    #[error("registry operation failed: {0}")]
    Registry(String),
    #[error("no manifest for platform {os}/{architecture}{variant}")]
    UnsupportedPlatform {
        os: String,
        architecture: String,
        variant: String,
    },
    #[error("malformed OCI document: {0}")]
    MalformedOci(String),
    #[error("layer DiffID mismatch: expected {expected}, got {actual}")]
    DiffIdMismatch { expected: String, actual: String },
    #[error("cannot {operation} for OCI layer entry {path:?}: {source}")]
    LayerFilesystem {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    Io(
        #[from]
        #[source]
        std::io::Error,
    ),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn typed_error_categories_preserve_human_context() {
        let message = "registry refused manifest: permission denied";
        assert_eq!(
            Error::Registry(message.into()).to_string(),
            format!("registry operation failed: {message}")
        );
        assert_eq!(
            Error::InvalidMetadata(message.into()).to_string(),
            format!("invalid metadata: {message}")
        );
        assert_eq!(
            Error::InvalidReference(message.into()).to_string(),
            format!("invalid image reference: {message}")
        );
    }

    #[test]
    fn io_from_and_display_match_inner() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let expected = source.to_string();
        let error: Error = source.into();
        assert_eq!(error.to_string(), expected);
        assert!(std::error::Error::source(&error).is_some());
    }
}
