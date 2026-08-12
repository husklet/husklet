use super::Networks;
use crate::{Error, Network, NetworkDriver, Result};

#[derive(Clone, Copy)]
enum Removal {
    Unused,
    Force,
}

impl Networks {
    /// Removes an unused network.
    ///
    /// # Errors
    /// Returns lookup, in-use, or persistence failures.
    pub async fn remove(&self, reference: &str) -> Result<Network> {
        self.remove_with(reference, Removal::Unused).await
    }

    /// Removes a network and all of its durable endpoint attachments.
    ///
    /// Existing processes retain any already-open sockets, but future process
    /// launches no longer receive the removed network configuration.
    ///
    /// # Errors
    /// Returns lookup, predefined-network, runtime, or persistence failures.
    pub async fn force_remove(&self, reference: &str) -> Result<Network> {
        self.remove_with(reference, Removal::Force).await
    }

    async fn remove_with(&self, reference: &str, removal: Removal) -> Result<Network> {
        let _guard = self.operation.lock().await;
        let network = self.resolve_network(reference).await?;
        if network.predefined() {
            return Err(Error::InvalidNetwork("predefined networks cannot be removed".into()));
        }
        if !network.endpoints.is_empty() && matches!(removal, Removal::Unused) {
            return Err(Error::NetworkInUse(network.name));
        }
        if network.driver == NetworkDriver::Bridge {
            let path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{}", network.id));
            match tokio::fs::remove_dir_all(path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(Error::Runtime(error.to_string())),
            }
        }
        self.storage.remove(&network.name).await?;
        Ok(network)
    }

    /// Removes every unused network accepted by the domain policy.
    ///
    /// # Errors
    /// Returns persistence or record-decoding failures.
    pub async fn prune(&self) -> Result<Vec<Network>> {
        let candidates = self
            .list()
            .await?
            .into_iter()
            .filter(|network| !network.predefined() && network.endpoints.is_empty())
            .map(|network| network.name)
            .collect::<Vec<_>>();
        let mut removed = Vec::new();
        for name in candidates {
            match self.remove(&name).await {
                Ok(network) => removed.push(network),
                Err(Error::NetworkInUse(_) | Error::NetworkNotFound(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(removed)
    }
}
