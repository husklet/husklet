use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptionIdentity, DescriptionRef};
use hl_network::{SocketHostIo, SocketId};

use super::{RuntimeSocket, RuntimeSocketKind};

#[derive(Debug)]
pub struct Registry<H: SocketHostIo> {
    state: Mutex<RegistryState<H>>,
    unix: Mutex<Arc<hl_network::UnixNamespace>>,
}

#[derive(Debug)]
struct RegistryState<H: SocketHostIo> {
    generation: u64,
    sockets: BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>,
}

impl<H: SocketHostIo> Default for Registry<H> {
    fn default() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                generation: 1,
                sockets: BTreeMap::new(),
            }),
            unix: Mutex::new(Arc::new(hl_network::UnixNamespace::default())),
        }
    }
}

impl<H: SocketHostIo> Registry<H> {
    #[must_use]
    pub fn unix_namespace(&self) -> Arc<hl_network::UnixNamespace> {
        self.unix
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn replace_unix(&self, replacement: Arc<hl_network::UnixNamespace>) -> Arc<hl_network::UnixNamespace> {
        std::mem::replace(
            &mut *self.unix.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            replacement,
        )
    }

    pub fn host_token(&self, description: &DescriptionRef) -> Option<H::Token> {
        let socket = self.get(description.description_identity())?;
        match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => Some(*token),
            RuntimeSocketKind::Unix { .. } | RuntimeSocketKind::UnixStandalone { .. } => None,
        }
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        identity: DescriptionIdentity,
        socket: Arc<RuntimeSocket<H>>,
    ) -> Result<(), ()> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sockets.contains_key(&identity) {
            return Err(());
        }
        let generation = state.generation.checked_add(1).ok_or(())?;
        state.sockets.insert(identity, socket.clone());
        state.generation = generation;
        drop(state);
        socket.attach_registry(self, identity);
        Ok(())
    }
    pub(crate) fn get(&self, identity: DescriptionIdentity) -> Option<Arc<RuntimeSocket<H>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sockets
            .get(&identity)
            .cloned()
    }
    pub(crate) fn get_id(&self, id: SocketId) -> Option<Arc<RuntimeSocket<H>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sockets
            .values()
            .find(|socket| socket.id == id)
            .cloned()
    }
    pub(super) fn retire(&self, identity: DescriptionIdentity) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sockets.remove(&identity).is_some() {
            state.generation = state.generation.saturating_add(1);
        }
    }

    pub(crate) fn checkpoint_lease(&self) -> (u64, BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>) {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.generation, state.sockets.clone())
    }

    pub(crate) fn checkpoint_replace(
        self: &Arc<Self>,
        expected: u64,
        replacement: BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>,
    ) -> Result<(u64, BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>), ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        if state.generation != expected {
            return Err(());
        }
        let generation = state.generation.checked_add(1).ok_or(())?;
        let previous = std::mem::replace(&mut state.sockets, replacement);
        state.generation = generation;
        let current = state.sockets.clone();
        drop(state);
        for (identity, socket) in &previous {
            socket.detach_registry(*identity);
        }
        for (identity, socket) in &current {
            socket.attach_registry(self, *identity);
        }
        Ok((generation, previous))
    }
}
