use std::{error, fmt, io, path::PathBuf, process::ExitStatus};

#[derive(Debug)]
pub enum Error {
    Discovery {
        operation: &'static str,
        source: cc::Error,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    ToolFailed {
        operation: &'static str,
        path: PathBuf,
        status: ExitStatus,
    },
    MissingArtifact {
        operation: &'static str,
        path: PathBuf,
    },
    InvalidPlan {
        operation: &'static str,
        path: PathBuf,
        message: String,
    },
}

impl Error {
    pub(crate) fn discovery(operation: &'static str, source: cc::Error) -> Self {
        Self::Discovery { operation, source }
    }

    pub(crate) fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery { operation, source } => write!(formatter, "failed to {operation}: {source}"),
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "failed to {operation} {}: {source}", path.display()),
            Self::ToolFailed {
                operation,
                path,
                status,
            } => write!(formatter, "failed to {operation} {}: {status}", path.display()),
            Self::MissingArtifact { operation, path } => {
                write!(formatter, "failed to {operation}: missing artifact {}", path.display())
            }
            Self::InvalidPlan {
                operation,
                path,
                message,
            } => write!(formatter, "failed to {operation} {}: {message}", path.display()),
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Discovery { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::ToolFailed { .. } | Self::MissingArtifact { .. } | Self::InvalidPlan { .. } => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::Error;
    use std::{error::Error as _, io, path::PathBuf};

    #[test]
    fn io_errors_preserve_operation_path_and_source() {
        let error = Error::io(
            "write response file",
            PathBuf::from("out/objects.rsp"),
            io::Error::other("full"),
        );
        assert!(error.to_string().contains("write response file out/objects.rsp"));
        assert_eq!(error.source().unwrap().to_string(), "full");
    }
}
