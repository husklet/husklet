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
    /// A registry refused an operation, carrying the registry's own words about why.
    ///
    /// Those words are the whole diagnosis, because the status line is not: Docker Hub answers a
    /// refused pull with HTTP 401 whose body reads `{"errors":[{"code":"UNAUTHORIZED",...}]}` for a
    /// credential failure and `{"errors":[{"code":"TOOMANYREQUESTS",...}]}` for an exhausted
    /// anonymous quota. Those need opposite responses -- configure credentials, or wait -- and only
    /// `body` separates them. It is carried verbatim rather than classified here: every registry
    /// words it differently, and text an operator has to read beats a guess that is confidently
    /// wrong. Construct with [`Error::registry`], which bounds it.
    #[error("registry operation failed: {message}{}", body.as_ref().map_or_else(String::new, |body| format!("; registry said: {body}")))]
    Registry { message: String, body: Option<String> },
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
    #[error("{}{source}", path.as_ref().map_or_else(String::new, |path| format!("{}: ", path.display())))]
    Io {
        path: Option<PathBuf>,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// The most of a registry response body an error will carry.
    ///
    /// A response body is remote-controlled and unbounded on the wire, and this one reaches logs,
    /// the daemon's HTTP error surface, and pull-progress records, so it is cut here rather than at
    /// any of them. 2 KiB is thirteen times the 157-byte envelope `index.docker.io` returned for
    /// `library/ubuntu:24.04` when measured on 2026-08-20, which leaves room for a multi-error OCI
    /// envelope with its `detail` objects intact while keeping one failure to about one screen.
    /// A registry with more than that to say is no longer diagnosing anything.
    const REGISTRY_BODY_BYTES: usize = 2048;

    /// Records a registry failure together with the registry's own response body.
    ///
    /// The body is bounded and its control characters are escaped, so a hostile or merely verbose
    /// registry can neither flood a log nor forge a line in one. Everything printable survives
    /// byte for byte.
    #[must_use]
    pub fn registry(message: impl Into<String>, body: Option<String>) -> Self {
        let body = body
            .filter(|body| !body.trim().is_empty())
            .map(|body| Self::readable(&body));
        Self::Registry {
            message: message.into(),
            body,
        }
    }

    fn readable(body: &str) -> String {
        use std::fmt::Write as _;

        let mut readable = String::with_capacity(body.len());
        for character in body.chars() {
            if character.is_control() {
                readable.extend(character.escape_debug());
            } else {
                readable.push(character);
            }
        }
        if readable.len() > Self::REGISTRY_BODY_BYTES {
            let mut end = Self::REGISTRY_BODY_BYTES;
            while !readable.is_char_boundary(end) {
                end -= 1;
            }
            readable.truncate(end);
            // Name the length the registry sent, not the escaped length, so the number an operator
            // reads is the one their own capture of the response would show.
            let _ = write!(readable, "... ({} bytes, truncated)", body.len());
        }
        readable
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io { path: None, source }
    }
}

/// Name the file an io failure refers to; the caller cannot reconstruct it from an `errno`.
pub(crate) trait At<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> At<T> for std::result::Result<T, std::io::Error> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| Error::Io {
            path: Some(path.into()),
            source,
        })
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn typed_error_categories_preserve_human_context() {
        let message = "registry refused manifest: permission denied";
        assert_eq!(
            Error::registry(message, None).to_string(),
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

    /// The two Docker Hub answers an operator must tell apart share a status line and differ only
    /// in the body, so the body has to reach the message or the message cannot be acted on.
    #[test]
    fn a_registry_failure_reports_the_body_that_separates_credentials_from_quota() {
        let credentials = Error::registry(
            "HTTP 401 from https://index.docker.io/v2/library/ubuntu/manifests/24.04",
            Some(r#"{"errors":[{"code":"UNAUTHORIZED","message":"authentication required"}]}"#.into()),
        );
        let quota = Error::registry(
            "HTTP 401 from https://index.docker.io/v2/library/ubuntu/manifests/24.04",
            Some(r#"{"errors":[{"code":"TOOMANYREQUESTS","message":"pull request limit exceeded"}]}"#.into()),
        );
        assert!(credentials.to_string().contains("UNAUTHORIZED"), "{credentials}");
        assert!(quota.to_string().contains("TOOMANYREQUESTS"), "{quota}");
        assert_ne!(credentials.to_string(), quota.to_string());
    }

    /// A registry that sent nothing must not grow an empty `registry said:` clause, and one that
    /// sent a novel must not reach a log intact.
    #[test]
    fn a_registry_body_is_optional_bounded_and_cannot_forge_a_log_line() {
        assert_eq!(
            Error::registry("HTTP 401 from mock", Some(String::new())).to_string(),
            "registry operation failed: HTTP 401 from mock"
        );

        let flood = "x".repeat(64 * 1024);
        let bounded = Error::registry("HTTP 500 from mock", Some(flood.clone())).to_string();
        assert!(bounded.len() < flood.len(), "{}", bounded.len());
        assert!(bounded.contains("(65536 bytes, truncated)"), "{bounded}");

        let forged = Error::registry(
            "HTTP 401 from mock",
            Some("first\nregistry operation failed: fine".into()),
        );
        assert_eq!(forged.to_string().lines().count(), 1);
        assert!(forged.to_string().contains("first\\nregistry"), "{forged}");
    }

    #[test]
    fn io_from_and_display_match_inner() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let expected = source.to_string();
        let error: Error = source.into();
        assert_eq!(error.to_string(), expected);
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn io_names_the_path_it_failed_on() {
        use super::At as _;

        let error =
            std::result::Result::<(), _>::Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))
                .at("/store/snapshots/committed/chain-abc")
                .unwrap_err();
        assert_eq!(error.to_string(), "/store/snapshots/committed/chain-abc: no such file");
    }
}
