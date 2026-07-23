use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{Error, Result};

/// Unix transport and Docker protocol policy.
#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) socket: PathBuf,
    pub(crate) api_version: String,
    pub(crate) timeout: Duration,
    pub(crate) response_limit: usize,
}

impl Config {
    /// Defaults to Docker API v1.43, a 30-second request timeout, and 16 MiB responses.
    pub fn unix(socket: impl AsRef<Path>) -> Self {
        Self {
            socket: socket.as_ref().to_path_buf(),
            api_version: "v1.43".into(),
            timeout: Duration::from_secs(30),
            response_limit: 16 * 1024 * 1024,
        }
    }

    #[must_use]
    pub fn api_version(mut self, version: impl Into<String>) -> Self {
        self.api_version = version.into();
        self
    }
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    #[must_use]
    pub fn response_limit(mut self, bytes: usize) -> Self {
        self.response_limit = bytes;
        self
    }
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }
    #[must_use]
    pub fn version(&self) -> &str {
        &self.api_version
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.socket.as_os_str().is_empty() {
            return Err(Error::Config("socket path is empty".into()));
        }
        let version = self
            .api_version
            .strip_prefix('v')
            .unwrap_or(&self.api_version);
        let (major, minor) = version
            .split_once('.')
            .ok_or_else(|| Error::Config("API version must be vMAJOR.MINOR".into()))?;
        if major.parse::<u16>().is_err() || minor.parse::<u16>().is_err() {
            return Err(Error::Config("API version must be vMAJOR.MINOR".into()));
        }
        if self.timeout.is_zero() {
            return Err(Error::Config("timeout must be non-zero".into()));
        }
        if self.response_limit == 0 {
            return Err(Error::Config("response limit must be non-zero".into()));
        }
        Ok(())
    }
}
