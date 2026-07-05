//! Network endpoints.

use bollard::models::NetworkCreateRequest;
use bollard::query_parameters::ListNetworksOptions;

use crate::{Client, Network, Result};

impl Client {
    /// `GET /networks`.
    pub async fn list_networks(&self) -> Result<Vec<Network>> {
        let ns = self
            .docker()?
            .list_networks(None::<ListNetworksOptions>)
            .await?;
        Ok(ns.into_iter().map(Network::from).collect())
    }

    /// `POST /networks/create` — returns the new network id.
    pub async fn create_network(&self, name: &str) -> Result<String> {
        let body = NetworkCreateRequest {
            name: name.to_string(),
            ..Default::default()
        };
        Ok(self.docker()?.create_network(body).await?.id)
    }

    /// `DELETE /networks/{id}`.
    pub async fn remove_network(&self, id: &str) -> Result<()> {
        self.docker()?.remove_network(id).await
    }
}
