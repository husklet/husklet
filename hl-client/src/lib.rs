//! `hl-client` — the shared client both the dd GUI and CLI use to talk to **hl-daemon** over its
//! Unix socket. The wire transport is [`bollard`] (the mature Docker-Engine-API crate); this crate
//! wraps it behind a small façade with hl-specific view models, so the consumers depend on one
//! place and never touch bollard's types directly.
//!
//! ```no_run
//! # async fn ex() -> Result<(), hl_client::Error> {
//! let c = hl_client::Client::new("/Users/me/.hl/run/docker.sock");
//! if c.ping().await.is_ok() {
//!     for ct in c.list_containers().await? { println!("{} {}", ct.id, ct.image); }
//! }
//! # Ok(()) }
//! ```
#![warn(missing_docs)]

use std::path::{Path, PathBuf};

use bollard::{ClientVersion, Docker};

mod client;
mod models;
pub use models::*;

/// Errors surfaced by the client — bollard's, which is `Display` (the GUI shows it, the CLI prints
/// it). Re-exported as `Error` so consumers don't name bollard directly.
pub type Error = bollard::errors::Error;
type Result<T> = std::result::Result<T, Error>;

/// hl-daemon speaks Docker API v1.43.
const VERSION: ClientVersion = ClientVersion {
    major_version: 1,
    minor_version: 43,
};

/// A handle to a hl-daemon reachable at `socket`. Cheap to clone; connects lazily per request
/// (bollard's connector does no I/O until a call is made), matching the old per-request model.
#[derive(Clone)]
pub struct Client {
    socket: PathBuf,
}

impl Client {
    /// Build a client for the daemon listening on `socket`. Does not connect yet.
    pub fn new(socket: impl AsRef<Path>) -> Self {
        Client {
            socket: socket.as_ref().to_path_buf(),
        }
    }

    /// Resolve the default socket: `$HL_DOCKER_SOCK`, else `~/.hl/run/docker.sock`.
    pub fn default_socket() -> PathBuf {
        if let Ok(s) = std::env::var("HL_DOCKER_SOCK") {
            return PathBuf::from(s);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".hl/run/docker.sock")
    }

    /// The socket path this client targets.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    fn docker(&self) -> Result<Docker> {
        Docker::connect_with_unix(&self.socket.to_string_lossy(), 30, &VERSION)
    }
}
