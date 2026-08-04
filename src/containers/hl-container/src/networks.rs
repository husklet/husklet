use crate::model::now_ms;
use crate::storage::{Containers as ContainerStore, NetworkStore};
use crate::{
    Container, ContainerId, Endpoint, EndpointSpec, Error, Isolation, Network, NetworkDriver, NetworkSpec, Result,
};
use std::sync::Arc;
use tokio::sync::Mutex;

mod pool;
use pool::Pool;
mod removal;

/// Durable host-neutral network topology and address ownership.
#[derive(Clone)]
pub struct Networks {
    storage: Arc<dyn NetworkStore>,
    containers: Arc<dyn ContainerStore>,
    operation: Arc<Mutex<()>>,
    identity: crate::identity::Identity,
}

impl Networks {
    pub(crate) fn new(
        storage: Arc<dyn NetworkStore>,
        containers: Arc<dyn ContainerStore>,
        operation: Arc<Mutex<()>>,
        runtime_root: std::path::PathBuf,
    ) -> Self {
        Self {
            storage,
            containers,
            operation,
            identity: crate::identity::Identity::new(runtime_root),
        }
    }

    pub(crate) async fn ensure_predefined(&self, spec: NetworkSpec) -> Result<Network> {
        spec.validate()?;
        if !matches!(
            (spec.name.as_str(), spec.driver),
            ("bridge", NetworkDriver::Bridge) | ("none", NetworkDriver::None)
        ) {
            return Err(Error::InvalidNetwork(
                "predefined network name and driver disagree".into(),
            ));
        }
        if let Some(existing) = self.storage.get(&spec.name).await? {
            if existing.driver != spec.driver || existing.driver == NetworkDriver::Bridge && existing.subnet.is_none() {
                return Err(Error::NetworkConflict(spec.name));
            }
            existing.validate()?;
            return Ok(existing);
        }
        self.create(spec).await
    }

    /// Creates and durably records a virtual network.
    ///
    /// # Errors
    /// Returns validation, overlap, persistence, or naming conflicts.
    pub async fn create(&self, mut spec: NetworkSpec) -> Result<Network> {
        spec.validate()?;
        let _guard = self.operation.lock().await;
        if let Some(existing) = self.storage.get(&spec.name).await? {
            if existing.compatible(&spec) {
                return Ok(existing);
            }
            return Err(Error::NetworkConflict(spec.name));
        }
        let existing = self.storage.list().await?;
        let pool = Pool::from(existing.as_slice());
        if spec.driver == NetworkDriver::Bridge && spec.subnet.is_none() {
            spec.subnet = Some(pool.allocate()?);
        }
        if let Some(subnet) = spec.subnet {
            pool.validate(subnet)?;
        }
        let network = Network::from_spec(spec, now_ms());
        self.storage.insert(&network).await?;
        Ok(network)
    }

    pub(crate) async fn reconcile(&self) -> Result<()> {
        let containers = self
            .containers
            .list()
            .await?
            .into_iter()
            .map(|container| container.id)
            .collect::<std::collections::BTreeSet<_>>();
        let mut ids = std::collections::BTreeSet::new();
        for network in self.storage.list().await? {
            network.validate()?;
            if !ids.insert(network.id.clone()) {
                return Err(Error::Corrupt(format!("duplicate network ID {}", network.id)));
            }
            if network
                .endpoints
                .keys()
                .any(|container| !containers.contains(container))
            {
                return Err(Error::Corrupt(format!(
                    "network {:?} references a missing container",
                    network.name
                )));
            }
        }
        Ok(())
    }

    /// Lists every durable network in name and ID order.
    ///
    /// # Errors
    /// Returns persistence or record-decoding failures.
    pub async fn list(&self) -> Result<Vec<Network>> {
        let mut values = self.storage.list().await?;
        values.sort_by(|left, right| left.name.cmp(&right.name).then_with(|| left.id.cmp(&right.id)));
        Ok(values)
    }

    /// Resolves a network by ID, prefix, or name.
    ///
    /// # Errors
    /// Returns lookup, ambiguity, persistence, or record-decoding failures.
    pub async fn inspect(&self, reference: &str) -> Result<Network> {
        self.resolve_network(reference).await
    }

    /// Attaches a stopped container and assigns its durable address.
    ///
    /// # Errors
    /// Returns lookup, validation, conflict, topology, or persistence failures.
    pub async fn connect(&self, network: &str, container: &str, spec: EndpointSpec) -> Result<Endpoint> {
        let mut endpoints = self.connect_many(container, [(network.to_owned(), spec)]).await?;
        endpoints
            .pop()
            .ok_or_else(|| Error::Corrupt("network attachment produced no endpoint".into()))
    }

    /// Validates a prospective set of attachments without changing durable state.
    ///
    /// # Errors
    /// Returns lookup, endpoint, duplicate-network, or address-allocation failures.
    pub async fn validate_connections(&self, requests: &[(String, EndpointSpec)]) -> Result<Vec<NetworkDriver>> {
        if requests.is_empty() {
            return Err(Error::InvalidNetwork(
                "at least one network attachment is required".into(),
            ));
        }
        let _guard = self.operation.lock().await;
        let mut ids = std::collections::BTreeSet::new();
        let mut drivers = Vec::with_capacity(requests.len());
        for (reference, spec) in requests {
            spec.validate()?;
            let network = self.resolve_network(reference).await?;
            if !ids.insert(network.id.clone()) {
                return Err(Error::InvalidNetwork(format!(
                    "network {:?} was requested more than once",
                    network.name
                )));
            }
            network.allocate(spec.address)?;
            drivers.push(network.driver);
        }
        Ok(drivers)
    }

    /// Atomically attaches a stopped container to several existing networks.
    ///
    /// Every network, endpoint option, address, and duplicate reference is validated before any
    /// durable network record changes. A storage failure restores all records already changed.
    ///
    /// # Errors
    /// Returns lookup, validation, conflict, topology, or persistence failures.
    pub async fn connect_many(
        &self,
        container: &str,
        requests: impl IntoIterator<Item = (String, EndpointSpec)>,
    ) -> Result<Vec<Endpoint>> {
        let requests = requests.into_iter().collect::<Vec<_>>();
        if requests.is_empty() {
            return Err(Error::InvalidNetwork(
                "at least one network attachment is required".into(),
            ));
        }
        for (_, spec) in &requests {
            spec.validate()?;
        }
        let _guard = self.operation.lock().await;
        let container = self.resolve_container(container).await?;
        if container.spec.network_mode == crate::NetworkMode::Host {
            return Err(Error::InvalidNetwork(
                "host network mode cannot be connected to a virtual network".into(),
            ));
        }
        if container.state.is_active() {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "stopped before network mutation",
            });
        }
        let mut planned = Vec::with_capacity(requests.len());
        let mut ids = std::collections::BTreeSet::new();
        for (reference, spec) in requests {
            let mut network = self.resolve_network(&reference).await?;
            if !ids.insert(network.id.clone()) {
                return Err(Error::InvalidNetwork(format!(
                    "network {:?} was requested more than once",
                    network.name
                )));
            }
            if network.endpoints.contains_key(&container.id) {
                return Err(Error::AlreadyConnected {
                    network: network.name,
                    container: container.id,
                });
            }
            let address = network.allocate(spec.address)?;
            let generated_name = spec.name.is_none();
            let endpoint = Endpoint {
                container: container.id.clone(),
                address,
                name: spec
                    .name
                    .or_else(|| container.spec.name.clone())
                    .unwrap_or_else(|| container.id.to_string()),
                generated_name,
                aliases: spec.aliases,
            };
            network.endpoints.insert(container.id.clone(), endpoint.clone());
            planned.push((network, endpoint));
        }
        let mut committed: Vec<Network> = Vec::new();
        for (network, _) in &planned {
            if let Err(error) = self.storage.replace(network).await {
                for mut rollback in committed.into_iter().rev() {
                    rollback.endpoints.remove(&container.id);
                    self.storage.replace(&rollback).await?;
                }
                return Err(error);
            }
            committed.push(network.clone());
        }
        let members = self
            .containers
            .list()
            .await?
            .into_iter()
            .filter(|member| {
                member.id != container.id
                    && member.state.is_active()
                    && planned
                        .iter()
                        .any(|(network, _)| network.endpoints.contains_key(&member.id))
            })
            .collect::<Vec<_>>();
        if !members.is_empty() {
            let inventory = self.storage.list().await?;
            for member in members {
                let configs = Self::runtime_from(&inventory, &member.id);
                self.identity.refresh(&member, &configs)?;
            }
        }
        Ok(planned.into_iter().map(|(_, endpoint)| endpoint).collect())
    }

    pub(crate) async fn rename_generated_endpoint(&self, container: &ContainerId, old: &str, new: &str) -> Result<()> {
        let originals = self.storage.list().await?;
        let mut committed = Vec::new();
        for original in originals {
            let Some(endpoint) = original.endpoints.get(container) else {
                continue;
            };
            if !endpoint.generated_name || endpoint.name != old {
                continue;
            }
            let mut candidate = original.clone();
            let endpoint = candidate
                .endpoints
                .get_mut(container)
                .expect("candidate cloned a present endpoint");
            endpoint.name = new.to_owned();
            if let Err(error) = self.storage.replace(&candidate).await {
                for rollback in committed.into_iter().rev() {
                    self.storage.replace(&rollback).await?;
                }
                return Err(error);
            }
            committed.push(original);
        }
        Ok(())
    }

    /// Detaches a stopped container from a network.
    ///
    /// # Errors
    /// Returns lookup, topology, state, or persistence failures.
    pub async fn disconnect(&self, network: &str, container: &str) -> Result<Endpoint> {
        let _guard = self.operation.lock().await;
        let container = self.resolve_container(container).await?;
        if container.state.is_active() {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "stopped before network mutation",
            });
        }
        let mut network = self.resolve_network(network).await?;
        self.require_mutable(&network).await?;
        let endpoint = network
            .endpoints
            .remove(&container.id)
            .ok_or_else(|| Error::NotConnected {
                network: network.name.clone(),
                container: container.id,
            })?;
        self.storage.replace(&network).await?;
        Ok(endpoint)
    }

    pub(crate) async fn launch(
        &self,
        id: &ContainerId,
        isolation: Isolation,
        mode: crate::NetworkMode,
    ) -> Result<Vec<crate::service::NetworkConfig>> {
        let mut configs = Vec::new();
        for network in self.storage.list().await? {
            if network.endpoints.contains_key(id) {
                if mode == crate::NetworkMode::Host {
                    return Err(Error::InvalidSpec(
                        "host network mode cannot carry network endpoints".into(),
                    ));
                }
                match network.driver {
                    NetworkDriver::None if !isolation.network_isolated => {
                        return Err(Error::InvalidSpec(
                            "none-network attachment requires network isolation".into(),
                        ));
                    }
                    NetworkDriver::Bridge if isolation.network_isolated => {
                        return Err(Error::InvalidSpec(
                            "bridge-network attachment requires networking to be enabled".into(),
                        ));
                    }
                    _ => {}
                }
                configs.push(crate::service::NetworkConfig::from_network(&network, id));
            }
        }
        Ok(configs)
    }

    pub(crate) async fn attach_default_for_publication_locked(&self, container: &Container) -> Result<()> {
        if container.spec.network_mode != crate::NetworkMode::Automatic
            || container.spec.isolation.network_isolated
            || container.spec.publish.is_empty()
        {
            return Ok(());
        }
        let mut networks = self.storage.list().await?;
        if networks
            .iter()
            .any(|network| network.endpoints.contains_key(&container.id))
        {
            return Ok(());
        }
        let bridge = if let Some(index) = networks.iter().position(|network| network.name == "bridge") {
            networks.swap_remove(index)
        } else {
            let mut spec = NetworkSpec::bridge_auto("bridge");
            spec.subnet = Some(Pool::from(networks.as_slice()).allocate()?);
            let network = Network::from_spec(spec, now_ms());
            self.storage.insert(&network).await?;
            network
        };
        if bridge.driver != NetworkDriver::Bridge {
            return Err(Error::InvalidNetwork(
                "predefined bridge name is owned by a non-bridge network".into(),
            ));
        }
        let mut bridge = bridge;
        let endpoint = Endpoint {
            container: container.id.clone(),
            address: bridge.allocate(None)?,
            name: container.spec.name.clone().unwrap_or_else(|| container.id.to_string()),
            generated_name: true,
            aliases: Vec::new(),
        };
        bridge.endpoints.insert(container.id.clone(), endpoint);
        self.storage.replace(&bridge).await
    }

    fn runtime_from(networks: &[Network], id: &ContainerId) -> Vec<crate::service::NetworkConfig> {
        let mut configs = Vec::new();
        for network in networks {
            if network.endpoints.contains_key(id) {
                configs.push(crate::service::NetworkConfig::from_network(network, id));
            }
        }
        configs
    }

    pub(crate) async fn disconnect_container_locked(&self, id: &ContainerId) -> Result<()> {
        for mut network in self.storage.list().await? {
            if network.endpoints.remove(id).is_some() {
                self.storage.replace(&network).await?;
            }
        }
        Ok(())
    }

    async fn resolve_container(&self, reference: &str) -> Result<Container> {
        let matches = self
            .containers
            .list()
            .await?
            .into_iter()
            .filter(|container| {
                container.id.as_str() == reference
                    || container.id.as_str().starts_with(reference)
                    || container.spec.name.as_deref() == Some(reference)
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [container] => Ok(container.clone()),
            [] => Err(Error::NotFound(reference.into())),
            _ => Err(Error::InvalidSpec(format!(
                "container reference {reference:?} is ambiguous"
            ))),
        }
    }

    async fn require_mutable(&self, network: &Network) -> Result<()> {
        if let Some(container) = self
            .containers
            .list()
            .await?
            .into_iter()
            .find(|container| network.endpoints.contains_key(&container.id) && container.state.is_active())
        {
            return Err(Error::InvalidState {
                id: container.id,
                actual: container.state,
                expected: "every attached container stopped before network mutation",
            });
        }
        Ok(())
    }

    async fn resolve_network(&self, reference: &str) -> Result<Network> {
        let networks = self.storage.list().await?;
        if let Some(network) = networks.iter().find(|network| network.id.as_str() == reference) {
            return Ok(network.clone());
        }
        let names = networks
            .iter()
            .filter(|network| network.name == reference)
            .collect::<Vec<_>>();
        match names.as_slice() {
            [network] => return Ok((*network).clone()),
            [] => {}
            _ => {
                return Err(Error::InvalidNetwork(format!(
                    "network reference {reference:?} is ambiguous"
                )));
            }
        }
        let prefixes = networks
            .iter()
            .filter(|network| network.id.as_str().starts_with(reference))
            .collect::<Vec<_>>();
        match prefixes.as_slice() {
            [network] => Ok((*network).clone()),
            [] => Err(Error::NetworkNotFound(reference.into())),
            _ => Err(Error::InvalidNetwork(format!(
                "network reference {reference:?} is ambiguous"
            ))),
        }
    }
}

#[cfg(test)]
mod test;
