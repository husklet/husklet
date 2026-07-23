//! Typed Docker-compatible Unix-socket client.
#![forbid(unsafe_code)]

mod config;
mod error;
pub mod model;
mod transport;
mod uri;

pub mod api;

pub use config::Config;
pub use error::{Error, Result};
pub use transport::{Stream, Upgrade};

use std::path::Path;
use std::sync::Arc;

use api::{Containers, Events, Executions, Images, Networks, System, Volumes};
use model::Version;
use transport::Transport;

/// A cheap-to-clone client for a Docker-compatible Unix socket.
#[derive(Clone, Debug)]
pub struct Client {
    endpoint: Arc<Endpoint>,
}

#[derive(Debug)]
struct Endpoint {
    config: Config,
    transport: Transport,
}

impl Client {
    /// Connect lazily to `socket` with the default Docker API version.
    ///
    /// # Errors
    /// Returns an error when the resulting client configuration is invalid.
    pub fn unix(socket: impl AsRef<Path>) -> Result<Self> {
        Self::with_config(Config::unix(socket))
    }

    /// Construct a client from explicit transport policy without performing I/O.
    ///
    /// # Errors
    /// Returns an error when `config` is invalid.
    pub fn with_config(config: Config) -> Result<Self> {
        config.validate()?;
        let transport = Transport::new(config.clone());
        Ok(Self {
            endpoint: Arc::new(Endpoint { config, transport }),
        })
    }

    /// Configuration used by this client.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.endpoint.config
    }

    /// Container operations.
    #[must_use]
    pub fn containers(&self) -> Containers<'_> {
        Containers::new(&self.endpoint.transport)
    }

    /// Lifecycle event subscriptions.
    #[must_use]
    pub fn events(&self) -> Events<'_> {
        Events::new(&self.endpoint.transport)
    }

    /// Processes executed in existing containers.
    #[must_use]
    pub fn executions(&self) -> Executions<'_> {
        Executions::new(&self.endpoint.transport)
    }

    /// Image metadata and archive operations.
    #[must_use]
    pub fn images(&self) -> Images<'_> {
        Images::new(&self.endpoint.transport)
    }

    /// Local persistent-volume operations.
    #[must_use]
    pub fn volumes(&self) -> Volumes<'_> {
        Volumes::new(&self.endpoint.transport)
    }

    /// Host-neutral network topology operations.
    #[must_use]
    pub fn networks(&self) -> Networks<'_> {
        Networks::new(&self.endpoint.transport)
    }

    /// Daemon-wide information and disk usage.
    #[must_use]
    pub fn system(&self) -> System<'_> {
        System::new(&self.endpoint.transport)
    }

    /// Check server reachability through Docker's unversioned ping endpoint.
    ///
    /// # Errors
    /// Returns transport, timeout, HTTP, or invalid-ping-response failures.
    pub async fn ping(&self) -> Result<()> {
        let body = self.endpoint.transport.get_unversioned("/_ping").await?;
        if body.as_ref() == b"OK" {
            Ok(())
        } else {
            Err(Error::Protocol("/_ping did not return OK".into()))
        }
    }

    /// Read server and runtime version information.
    ///
    /// # Errors
    /// Returns transport, timeout, HTTP, or response-decoding failures.
    pub async fn version(&self) -> Result<Version> {
        self.endpoint
            .transport
            .get_json_unversioned("/version")
            .await
    }
}
