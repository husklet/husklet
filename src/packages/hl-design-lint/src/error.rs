use std::{io, path::PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
/// Failure while loading sources, parsing Rust, or emitting a report.
pub enum LintError {
    /// Filesystem operation failed.
    #[error("failed to {action} {path}: {source}")]
    Io {
        /// Attempted operation.
        action: &'static str,
        /// Affected path.
        path: PathBuf,
        #[source]
        /// Underlying I/O failure.
        source: io::Error,
    },

    #[error("failed to parse {path}: {source}")]
    /// Rust source could not be parsed.
    Parse {
        /// Invalid source path.
        path: PathBuf,
        #[source]
        /// Parser failure.
        source: syn::Error,
    },

    #[error("failed to write {target}: {source}")]
    /// Reporter output failed.
    Report {
        /// Output being written.
        target: &'static str,
        #[source]
        /// Underlying output failure.
        source: io::Error,
    },

    #[error("{0}")]
    /// Command-line arguments are invalid.
    Argument(&'static str),
}

impl LintError {
    pub(crate) fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }

    pub(crate) fn report(target: &'static str, source: io::Error) -> Self {
        Self::Report { target, source }
    }
}

/// Result returned by lint operations.
pub type Result<T> = std::result::Result<T, LintError>;
