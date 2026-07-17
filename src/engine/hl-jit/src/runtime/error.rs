//! The runtime error type shared across the API surface.

use hl_jit_darwin::Guest;
use std::fmt;

/// An error configuring or running a container.
#[derive(Debug)]
pub enum Error {
    /// No engine backend is available for the requested guest (the JIT binary was not built).
    NoBackend(Guest),
    /// The container spec is incomplete (e.g. no image).
    Invalid(&'static str),
    /// The underlying OS failed to launch the container.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoBackend(g) => write!(f, "no hl-jit backend available for {}", g.target()),
            Error::Invalid(m) => write!(f, "invalid container config: {m}"),
            Error::Io(e) => write!(f, "container launch failed: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
