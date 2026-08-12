use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_descriptor::DescriptionIdentity;
use hl_network::{
    AuthoritySocketLease, NetworkCatalogRestore, NetworkCheckpointError, NetworkCheckpointImage,
    NetworkCheckpointRebind, NetworkSocketState, SocketId,
};

use crate::{CheckpointDescriptorTable, RuntimeSocket, SocketRegistry};

mod adoption;
mod descriptor;
mod host;
mod pending;
mod restore;

pub use host::{CheckpointHost, ReconnectedSocket};
use pending::PendingSocket;
use restore::RestoreTransaction;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Staging,
    Committed,
    Resumed,
}

struct PendingBinding {
    identity: DescriptionIdentity,
    object: Arc<PendingSocket>,
}

struct State {
    pending: BTreeMap<SocketId, Vec<PendingBinding>>,
    phase: Phase,
}

pub struct ObjectBindings<H: CheckpointHost> {
    descriptors: Arc<CheckpointDescriptorTable>,
    sockets: Arc<SocketRegistry<H>>,
    host: Option<Arc<H>>,
    state: Arc<Mutex<State>>,
}

struct StageAbort<'a, H: CheckpointHost> {
    bindings: &'a ObjectBindings<H>,
    host: Option<&'a Arc<H>>,
    armed: bool,
}

impl<'a, H: CheckpointHost> StageAbort<'a, H> {
    fn new(bindings: &'a ObjectBindings<H>) -> Self {
        Self {
            bindings,
            host: None,
            armed: true,
        }
    }

    fn touched_host(&mut self, host: &'a Arc<H>) {
        self.host = Some(host);
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl<H: CheckpointHost> Drop for StageAbort<'_, H> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(host) = self.host {
            host.checkpoint_rollback();
        }
        self.bindings.abort_stage();
    }
}

impl<H: CheckpointHost> ObjectBindings<H> {
    #[must_use]
    pub fn new(
        descriptors: Arc<CheckpointDescriptorTable>,
        sockets: Arc<SocketRegistry<H>>,
        host: Option<Arc<H>>,
    ) -> Self {
        Self {
            descriptors,
            sockets,
            host,
            state: Arc::new(Mutex::new(State {
                pending: BTreeMap::new(),
                phase: Phase::Staging,
            })),
        }
    }

    fn bind_object(
        state: &Mutex<State>,
        id: SocketId,
        object: Arc<RuntimeSocket<H>>,
        replacement: &mut BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>,
    ) -> Result<(), NetworkCheckpointError> {
        let state = state.lock().map_err(|_| NetworkCheckpointError::InvalidImage)?;
        if state.phase != Phase::Staging {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let pending = state.pending.get(&id).ok_or(NetworkCheckpointError::InvalidImage)?;
        if pending.len() != 1 {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        pending[0].object.bind(object.clone())?;
        if replacement.insert(pending[0].identity, object).is_some() {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn abort_stage(&self) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.pending.clear();
        state.phase = Phase::Staging;
    }
}

impl<H: CheckpointHost> NetworkCheckpointRebind for ObjectBindings<H> {
    fn capture_prepare(&self) -> Result<(), NetworkCheckpointError> {
        match &self.host {
            Some(host) => host.capture_prepare(),
            None => Ok(()),
        }
    }

    fn capture(&self, image: &mut NetworkCheckpointImage) -> Result<(), NetworkCheckpointError> {
        image.authority.clear();
        for socket in &image.sockets {
            let NetworkSocketState::Host {
                snapshot,
                resource,
                accepted,
            } = socket
            else {
                continue;
            };
            if !accepted.is_empty() {
                return Err(NetworkCheckpointError::InvalidImage);
            }
            match snapshot.state {
                hl_network::SocketState::Created | hl_network::SocketState::Bound => {}
                hl_network::SocketState::Listening { .. } => {
                    let key = self
                        .host
                        .as_ref()
                        .ok_or(NetworkCheckpointError::InvalidImage)?
                        .retain_listener(snapshot, *resource)?;
                    image.authority.push(AuthoritySocketLease {
                        resource: *resource,
                        key,
                    });
                }
                hl_network::SocketState::Connecting
                | hl_network::SocketState::Connected
                | hl_network::SocketState::Closed => {
                    return Err(NetworkCheckpointError::InvalidImage);
                }
            }
        }
        image.validate()
    }

    fn capture_publish(&self, digest: [u8; 32]) -> Result<(), NetworkCheckpointError> {
        match &self.host {
            Some(host) => host.capture_publish(digest),
            None => Ok(()),
        }
    }

    fn capture_abort(&self) {
        if let Some(host) = &self.host {
            host.capture_abort();
        }
    }

    fn capture_finish(&self) {
        if let Some(host) = &self.host {
            host.capture_finish();
        }
    }

    fn stage(&self, image: &NetworkCheckpointImage) -> Result<Box<dyn NetworkCatalogRestore>, NetworkCheckpointError> {
        self.stage_bound([0; 32], image)
    }

    fn stage_bound(
        &self,
        digest: [u8; 32],
        image: &NetworkCheckpointImage,
    ) -> Result<Box<dyn NetworkCatalogRestore>, NetworkCheckpointError> {
        let mut abort = StageAbort::new(self);
        let descriptors = self.descriptors.staged().ok_or(NetworkCheckpointError::InvalidImage)?;
        let (generation, previous) = self.sockets.checkpoint_lease();
        if let Some(host) = &self.host {
            abort.touched_host(host);
            host.restore_begin(digest, image)?;
            host.reserve_ports(&image.ports)?;
        } else if !image.ports.is_empty() {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let transaction = Box::new(RestoreTransaction {
            state: self.state.clone(),
            sockets: self.sockets.clone(),
            host: self.host.clone(),
            descriptors,
            image: image.clone(),
            registry_generation: generation,
            previous_registry: Some(previous),
            committed_registry: None,
            hosts: Vec::new(),
            pairs: Vec::new(),
            replacement_registry: BTreeMap::new(),
            unix: Arc::new(hl_network::UnixNamespace::default()),
            previous_unix: None,
            rebound: Vec::new(),
            catalog_bound: false,
        });
        abort.disarm();
        Ok(transaction)
    }
}
