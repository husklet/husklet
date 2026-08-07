//! Restore transaction that rebinds checkpointed sockets onto a live host.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptionIdentity, DescriptionRef, OpenFileDescription, StatusFlags};
use hl_network::{
    AcceptedSocketCheckpoint, NetworkCatalog, NetworkCatalogRestore, NetworkCheckpointError, NetworkCheckpointImage,
    NetworkResourceKey, NetworkSocketResource, NetworkSocketState, SocketConnectStatus, SocketDescription,
    SocketSnapshot, UnixPairSnapshot, UnixSocketPair,
};

use crate::{RuntimeSocket, RuntimeSocketRegistry};

use super::{CheckpointHost, ObjectBindings, Phase, State};

pub(super) struct HostStage<T> {
    snapshot: SocketSnapshot,
    token: T,
    accepted: Vec<(AcceptedSocketCheckpoint, T, Arc<dyn NetworkSocketResource>)>,
}

pub(super) struct RestoreTransaction<H: CheckpointHost> {
    pub(super) state: Arc<Mutex<State>>,
    pub(super) sockets: Arc<RuntimeSocketRegistry<H>>,
    pub(super) host: Option<Arc<H>>,
    pub(super) descriptors: Arc<hl_descriptor::DescriptorTable>,
    pub(super) image: NetworkCheckpointImage,
    pub(super) registry_generation: u64,
    pub(super) previous_registry: Option<BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>>,
    pub(super) committed_registry: Option<u64>,
    pub(super) hosts: Vec<HostStage<H::Token>>,
    pub(super) pairs: Vec<Arc<UnixSocketPair>>,
    pub(super) replacement_registry: BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>,
    pub(super) unix: Arc<hl_network::UnixNamespace>,
    pub(super) previous_unix: Option<Arc<hl_network::UnixNamespace>>,
    pub(super) rebound: Vec<Arc<RuntimeSocket<H>>>,
    pub(super) catalog_bound: bool,
}

impl<H: CheckpointHost> RestoreTransaction<H> {
    fn host(&self) -> Result<&Arc<H>, NetworkCheckpointError> {
        self.host.as_ref().ok_or(NetworkCheckpointError::InvalidImage)
    }

    fn connect(snapshot: &SocketSnapshot) -> SocketConnectStatus {
        if let Some(error) = snapshot.connect_error {
            return SocketConnectStatus::Failed(error);
        }
        match snapshot.state {
            hl_network::SocketState::Connecting => SocketConnectStatus::Pending,
            hl_network::SocketState::Connected => SocketConnectStatus::Connected,
            _ => SocketConnectStatus::Idle,
        }
    }

    fn close_rebound(&mut self) {
        for object in self.rebound.drain(..) {
            object.retire();
            object.close();
        }
    }

    fn bind_host(
        &mut self,
        catalog: &Arc<NetworkCatalog>,
        snapshot: &SocketSnapshot,
        index: usize,
    ) -> Result<(), NetworkCheckpointError> {
        let staged = self.hosts.get(index).ok_or(NetworkCheckpointError::InvalidImage)?;
        if staged.snapshot != *snapshot {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let token = staged.token;
        let accepted = staged.accepted.clone();
        let flags = StatusFlags::from_bits(if snapshot.nonblocking {
            StatusFlags::NONBLOCKING
        } else {
            0
        });
        let description = Arc::new(SocketDescription::restored(
            self.host()?.clone(),
            token,
            flags,
            Self::connect(snapshot),
        ));
        description.bind_readiness();
        if let hl_network::SocketState::Listening { backlog } = snapshot.state {
            description.listen(backlog as usize);
        }
        for (accepted, token, _binding) in accepted {
            description
                .publish_accepted(token, accepted.local, accepted.peer)
                .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        }
        let object = RuntimeSocket::host(description, token, snapshot.id, snapshot.clone(), catalog.clone());
        ObjectBindings::<H>::bind_object(
            self.state.as_ref(),
            snapshot.id,
            object.clone(),
            &mut self.replacement_registry,
        )?;
        self.rebound.push(object);
        Ok(())
    }

    fn bind_pair(
        &mut self,
        catalog: &Arc<NetworkCatalog>,
        endpoints: &[SocketSnapshot; 2],
        index: usize,
    ) -> Result<(), NetworkCheckpointError> {
        let pair = self
            .pairs
            .get(index)
            .ok_or(NetworkCheckpointError::InvalidImage)?
            .clone();
        let objects = RuntimeSocket::unix_pair(
            pair,
            [endpoints[0].id, endpoints[1].id],
            endpoints.clone(),
            catalog.clone(),
        );
        for endpoint in 0..2 {
            ObjectBindings::<H>::bind_object(
                self.state.as_ref(),
                endpoints[endpoint].id,
                objects[endpoint].clone(),
                &mut self.replacement_registry,
            )?;
            self.rebound.push(objects[endpoint].clone());
        }
        Ok(())
    }

    fn bind_unix(
        &mut self,
        catalog: &Arc<NetworkCatalog>,
        snapshot: &SocketSnapshot,
    ) -> Result<(), NetworkCheckpointError> {
        let object = RuntimeSocket::unix_standalone(snapshot.id, snapshot.clone(), catalog.clone());
        if let Some(hl_network::SocketAddress::Unix(raw)) = &snapshot.local {
            let address = if raw.is_empty() {
                hl_network::UnixAddress::Unnamed
            } else if raw[0] == 0 {
                hl_network::UnixAddress::Abstract(raw[1..].to_vec())
            } else {
                hl_network::UnixAddress::Pathname(raw.clone())
            };
            object
                .bind_unix(self.unix.clone(), address)
                .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        }
        let registry = &mut self.replacement_registry;
        ObjectBindings::<H>::bind_object(self.state.as_ref(), snapshot.id, object.clone(), registry)?;
        self.rebound.push(object);
        Ok(())
    }
}

impl<H: CheckpointHost> NetworkCatalogRestore for RestoreTransaction<H> {
    fn host_socket(
        &mut self,
        snapshot: &SocketSnapshot,
        resource: NetworkResourceKey,
    ) -> Result<Arc<dyn NetworkSocketResource>, NetworkCheckpointError> {
        let key = self
            .image
            .authority
            .iter()
            .find(|lease| lease.resource == resource)
            .map(|lease| lease.key);
        let restored = match key {
            Some(key) => self.host()?.reconnect_retained(snapshot, resource, key)?,
            None => self.host()?.reconnect(snapshot, resource)?,
        };
        let binding = restored.binding.clone();
        self.hosts.push(HostStage {
            snapshot: snapshot.clone(),
            token: restored.token,
            accepted: Vec::new(),
        });
        Ok(binding)
    }

    fn accepted_socket(&mut self, accepted: &AcceptedSocketCheckpoint) -> Result<(), NetworkCheckpointError> {
        let restored = self.host()?.reconnect_accepted(accepted)?;
        let host = self.hosts.last_mut().ok_or(NetworkCheckpointError::InvalidImage)?;
        host.accepted.push((accepted.clone(), restored.token, restored.binding));
        Ok(())
    }

    fn descriptor(&mut self, identity: u64) -> Result<DescriptionRef, NetworkCheckpointError> {
        self.descriptors
            .export_checkpoint_identity(identity)
            .map_err(|_| NetworkCheckpointError::InvalidImage)
    }

    fn unix_pair(&mut self, pair: &UnixPairSnapshot) -> Result<Arc<UnixSocketPair>, NetworkCheckpointError> {
        let pair = UnixSocketPair::restore(pair, StatusFlags::default(), |identity| self.descriptor(identity).ok())
            .map(Arc::new)
            .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        self.pairs.push(pair.clone());
        Ok(pair)
    }

    fn bind_catalog(&mut self, catalog: Arc<NetworkCatalog>) -> Result<(), NetworkCheckpointError> {
        if self.catalog_bound {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let mut host_index = 0;
        let mut pair_index = 0;
        let states = self.image.sockets.clone();
        for state in &states {
            match state {
                NetworkSocketState::Host { snapshot, .. } => {
                    self.bind_host(&catalog, snapshot, host_index)?;
                    host_index += 1;
                }
                NetworkSocketState::UnixPair { endpoints, .. } => {
                    self.bind_pair(&catalog, endpoints, pair_index)?;
                    pair_index += 1;
                }
                NetworkSocketState::Unix { snapshot, .. } => {
                    self.bind_unix(&catalog, snapshot)?;
                }
            }
        }
        if host_index != self.hosts.len() || pair_index != self.pairs.len() {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        self.catalog_bound = true;
        Ok(())
    }

    fn commit(&mut self) -> Result<(), NetworkCheckpointError> {
        if !self.catalog_bound {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        if let Some(host) = &self.host {
            host.checkpoint_commit()?;
        }
        let replacement = std::mem::take(&mut self.replacement_registry);
        let (generation, previous) = self
            .sockets
            .checkpoint_replace(self.registry_generation, replacement)
            .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        self.previous_registry = Some(previous);
        self.previous_unix = Some(self.sockets.replace_unix(self.unix.clone()));
        self.committed_registry = Some(generation);
        self.state
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)?
            .phase = Phase::Committed;
        Ok(())
    }

    fn rollback(&mut self) {
        if let (Some(generation), Some(previous)) = (self.committed_registry.take(), self.previous_registry.take()) {
            let _ = self.sockets.checkpoint_replace(generation, previous);
        }
        if let Some(previous) = self.previous_unix.take() {
            let _ = self.sockets.replace_unix(previous);
        }
        if let Some(host) = &self.host {
            host.checkpoint_rollback();
        }
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .clear();
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase = Phase::Staging;
        self.close_rebound();
    }

    fn resume(&mut self) -> Result<(), NetworkCheckpointError> {
        if self.committed_registry.is_none() {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        if let Some(host) = &self.host {
            host.checkpoint_resume()?;
        }
        let mut state = self.state.lock().map_err(|_| NetworkCheckpointError::InvalidImage)?;
        state.pending.clear();
        state.phase = Phase::Resumed;
        self.rebound.clear();
        Ok(())
    }
}
