use http::Method;

use crate::Result;
use crate::model::{Network, NetworkConnect, NetworkCreate, NetworkCreated, NetworkDisconnect, NetworkPrune};
use crate::transport::Transport;
use crate::uri::Component;

/// Typed Docker network operations.
#[derive(Clone, Copy, Debug)]
pub struct Networks<'a> {
    transport: &'a Transport,
}

impl<'a> Networks<'a> {
    pub(crate) fn new(transport: &'a Transport) -> Self {
        Self { transport }
    }

    /// List every network visible to the daemon.
    ///
    /// # Errors
    /// Returns transport, Docker API, or response-decoding failures.
    pub async fn list(&self) -> Result<Vec<Network>> {
        self.transport.get_json("/networks").await
    }

    /// Create a host-neutral network topology.
    ///
    /// # Errors
    /// Returns transport, validation, capability, storage, or response-decoding failures.
    pub async fn create(&self, request: &NetworkCreate) -> Result<NetworkCreated> {
        self.transport
            .json(Method::POST, "/networks/create", Some(request))
            .await
    }

    /// Inspect a network by name, full ID, or unambiguous ID prefix.
    ///
    /// # Errors
    /// Returns transport, not-found, ambiguous-reference, or response-decoding failures.
    pub async fn inspect(&self, reference: &str) -> Result<Network> {
        self.transport
            .get_json(&format!("/networks/{}", Component::opaque(reference)))
            .await
    }

    /// Attach a stopped container to a network.
    ///
    /// # Errors
    /// Returns transport, validation, ownership, IPAM, or Docker API failures.
    pub async fn connect(&self, reference: &str, request: &NetworkConnect) -> Result<()> {
        self.transport
            .empty_json(
                Method::POST,
                &format!("/networks/{}/connect", Component::opaque(reference)),
                request,
            )
            .await
    }

    /// Detach a stopped container from a network.
    ///
    /// # Errors
    /// Returns transport, lifecycle, ownership, or Docker API failures.
    pub async fn disconnect(&self, reference: &str, request: &NetworkDisconnect) -> Result<()> {
        self.transport
            .empty_json(
                Method::POST,
                &format!("/networks/{}/disconnect", Component::opaque(reference)),
                request,
            )
            .await
    }

    /// Remove an unused network.
    ///
    /// # Errors
    /// Returns transport, not-found, in-use, storage, or Docker API failures.
    pub async fn remove(&self, reference: &str, force: bool) -> Result<()> {
        self.transport
            .empty(
                Method::DELETE,
                &format!("/networks/{}?force={force}", Component::opaque(reference)),
            )
            .await
    }

    /// Remove every unused network.
    ///
    /// # Errors
    /// Returns transport, storage, Docker API, or response-decoding failures.
    pub async fn prune(&self) -> Result<NetworkPrune> {
        self.transport
            .json::<(), NetworkPrune>(Method::POST, "/networks/prune", None)
            .await
    }
}
