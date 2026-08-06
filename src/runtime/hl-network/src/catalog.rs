use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{
    AcceptedSocketCheckpoint, NETWORK_CHECKPOINT_SOCKET_MAXIMUM, NetworkCheckpointError, NetworkConfiguration,
    NetworkResourceKey, NetworkSocketResource, PortCheckpoint, SocketId, SocketSnapshot, UnixSocketPair,
};

mod checkpoint;
mod unix;

enum CatalogSocket {
    Host {
        snapshot: SocketSnapshot,
        resource: NetworkResourceKey,
        binding: Arc<dyn NetworkSocketResource>,
        accepted: Vec<AcceptedSocketCheckpoint>,
    },
    UnixPair {
        endpoints: [SocketSnapshot; 2],
        pair: Arc<UnixSocketPair>,
    },
    Unix {
        snapshot: SocketSnapshot,
        pending: Vec<SocketId>,
        datagram: Option<Arc<crate::UnixDatagramSocket>>,
    },
}

struct Slot {
    generation: u64,
    socket: Option<Arc<CatalogSocket>>,
}

pub struct NetworkCatalog {
    configuration: NetworkConfiguration,
    ports: Mutex<Vec<PortEntry>>,
    slots: Mutex<Vec<Slot>>,
    generation: AtomicU64,
    port_generation: AtomicU64,
    activity: crate::checkpoint_activity::CheckpointActivity,
}

/// Generation-qualified rollback capability for one atomic host bind update.
#[derive(Clone)]
pub(crate) enum PortEntry {
    Published(PortCheckpoint),
    Prepared { checkpoint: PortCheckpoint, generation: u64 },
}

impl PortEntry {
    fn checkpoint(&self) -> &PortCheckpoint {
        match self {
            Self::Published(checkpoint) | Self::Prepared { checkpoint, .. } => checkpoint,
        }
    }
}

#[must_use = "dropping an uncommitted bind rolls its reservation back"]
pub struct PreparedBind<'a> {
    catalog: &'a NetworkCatalog,
    _admission: crate::checkpoint_activity::Admission<'a>,
    previous: Arc<CatalogSocket>,
    installed: SocketSnapshot,
    port: PortCheckpoint,
    generation: u64,
    finalized: bool,
}

impl PreparedBind<'_> {
    pub fn commit(mut self) -> Result<(), NetworkCatalogError> {
        self.catalog.commit_prepared(&mut self)
    }

    pub fn rollback(mut self) -> Result<(), NetworkCatalogError> {
        self.catalog.rollback_prepared(&mut self)
    }
}

impl Drop for PreparedBind<'_> {
    fn drop(&mut self) {
        if !self.finalized {
            let _ = self.catalog.rollback_prepared(self);
        }
    }
}

/// One coherent observation of the live sockets in a network namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkNamespaceView {
    /// Changes after every catalog mutation represented by this view.
    pub generation: u64,
    pub unix: Vec<UnixSocketView>,
    pub internet: Vec<InternetSocketView>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InternetSocketView {
    pub inode: u64,
    pub family: crate::AddressFamily,
    pub socket_type: crate::SocketType,
    pub state: crate::SocketState,
    pub local: Option<crate::SocketAddress>,
    pub peer: Option<crate::SocketAddress>,
}

/// A live AF_UNIX socket as observed while holding the catalog lock once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnixSocketView {
    pub id: SocketId,
    pub inode: u64,
    pub socket_type: crate::SocketType,
    pub state: crate::SocketState,
    pub path: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkCatalogError {
    Capacity,
    Stale,
    Invalid,
    Checkpoint(NetworkCheckpointError),
}

impl From<NetworkCheckpointError> for NetworkCatalogError {
    fn from(error: NetworkCheckpointError) -> Self {
        Self::Checkpoint(error)
    }
}

impl NetworkCatalog {
    #[must_use]
    pub fn new(configuration: NetworkConfiguration) -> Self {
        Self {
            configuration,
            ports: Mutex::new(Vec::new()),
            slots: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
            port_generation: AtomicU64::new(1),
            activity: crate::checkpoint_activity::CheckpointActivity::default(),
        }
    }

    pub fn insert_host(
        &self,
        mut snapshot: SocketSnapshot,
        resource: NetworkResourceKey,
        binding: Arc<dyn NetworkSocketResource>,
        accepted: Vec<AcceptedSocketCheckpoint>,
    ) -> Result<SocketId, NetworkCatalogError> {
        let _admission = self.activity.admit();
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let id = Self::allocate(&mut slots)?;
        snapshot.id = id;
        if !crate::SocketNamespace::valid_checkpoint_snapshot(&snapshot) {
            return Err(NetworkCatalogError::Invalid);
        }
        slots[usize::from(id.slot) - 1].socket = Some(Arc::new(CatalogSocket::Host {
            snapshot,
            resource,
            binding,
            accepted,
        }));
        self.advance_generation();
        Ok(id)
    }

    pub fn insert_unix_pair(
        &self,
        mut endpoints: [SocketSnapshot; 2],
        pair: Arc<UnixSocketPair>,
    ) -> Result<[SocketId; 2], NetworkCatalogError> {
        let _admission = self.activity.admit();
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let [first, second] = Self::allocate_pair(&mut slots)?;
        endpoints[0].id = first;
        endpoints[1].id = second;
        if endpoints
            .iter()
            .any(|snapshot| !crate::SocketNamespace::valid_checkpoint_snapshot(snapshot))
        {
            return Err(NetworkCatalogError::Invalid);
        }
        let object = Arc::new(CatalogSocket::UnixPair { endpoints, pair });
        slots[usize::from(first.slot) - 1].socket = Some(object.clone());
        slots[usize::from(second.slot) - 1].socket = Some(object);
        self.advance_generation();
        Ok([first, second])
    }

    pub fn insert_unix(&self, mut snapshot: SocketSnapshot) -> Result<SocketId, NetworkCatalogError> {
        let _admission = self.activity.admit();
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let id = Self::allocate(&mut slots)?;
        snapshot.id = id;
        if snapshot.family != crate::AddressFamily::Unix
            || !crate::SocketNamespace::valid_checkpoint_snapshot(&snapshot)
        {
            return Err(NetworkCatalogError::Invalid);
        }
        let datagram =
            (snapshot.socket_type == crate::SocketType::Datagram).then(|| Arc::new(crate::UnixDatagramSocket::new()));
        slots[usize::from(id.slot) - 1].socket = Some(Arc::new(CatalogSocket::Unix {
            snapshot,
            pending: Vec::new(),
            datagram,
        }));
        self.advance_generation();
        Ok(id)
    }

    fn allocate_pair(slots: &mut Vec<Slot>) -> Result<[SocketId; 2], NetworkCatalogError> {
        let mut indexes = slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| slot.socket.is_none().then_some(index))
            .take(2)
            .collect::<Vec<_>>();
        let mut next = slots.len();
        while indexes.len() < 2 {
            indexes.push(next);
            next = next.checked_add(1).ok_or(NetworkCatalogError::Capacity)?;
        }
        if indexes[1] >= NETWORK_CHECKPOINT_SOCKET_MAXIMUM {
            return Err(NetworkCatalogError::Capacity);
        }
        let generations = [
            slots.get(indexes[0]).map_or(0, |slot| slot.generation).checked_add(1),
            slots.get(indexes[1]).map_or(0, |slot| slot.generation).checked_add(1),
        ];
        let [Some(first_generation), Some(second_generation)] = generations else {
            return Err(NetworkCatalogError::Capacity);
        };
        let ids = [
            SocketId {
                slot: u16::try_from(indexes[0] + 1).map_err(|_| NetworkCatalogError::Capacity)?,
                generation: first_generation,
            },
            SocketId {
                slot: u16::try_from(indexes[1] + 1).map_err(|_| NetworkCatalogError::Capacity)?,
                generation: second_generation,
            },
        ];
        slots.resize_with(indexes[1] + 1, || Slot {
            generation: 0,
            socket: None,
        });
        slots[indexes[0]].generation = first_generation;
        slots[indexes[1]].generation = second_generation;
        Ok(ids)
    }

    fn allocate(slots: &mut Vec<Slot>) -> Result<SocketId, NetworkCatalogError> {
        let index = slots
            .iter()
            .position(|slot| slot.socket.is_none())
            .unwrap_or(slots.len());
        if index >= NETWORK_CHECKPOINT_SOCKET_MAXIMUM {
            return Err(NetworkCatalogError::Capacity);
        }
        if index == slots.len() {
            slots.push(Slot {
                generation: 0,
                socket: None,
            });
        }
        let slot = &mut slots[index];
        slot.generation = slot.generation.checked_add(1).ok_or(NetworkCatalogError::Capacity)?;
        Ok(SocketId {
            slot: u16::try_from(index + 1).map_err(|_| NetworkCatalogError::Capacity)?,
            generation: slot.generation,
        })
    }

    pub fn claim_port(&self, checkpoint: PortCheckpoint) -> Result<(), NetworkCatalogError> {
        let _admission = self.activity.admit();
        let slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let owner = match Self::slot(&slots, checkpoint.owner)?.socket.as_deref() {
            Some(CatalogSocket::Host { snapshot, .. }) => snapshot,
            Some(CatalogSocket::UnixPair { endpoints, .. }) => endpoints
                .iter()
                .find(|snapshot| snapshot.id == checkpoint.owner)
                .ok_or(NetworkCatalogError::Stale)?,
            Some(CatalogSocket::Unix { snapshot, .. }) => snapshot,
            None => return Err(NetworkCatalogError::Stale),
        };
        if !Self::owns_port(owner, &checkpoint) {
            return Err(NetworkCatalogError::Invalid);
        }
        let mut ports = self.ports.lock().unwrap_or_else(|error| error.into_inner());
        if ports.iter().any(|port| {
            let port = port.checkpoint();
            port.family == checkpoint.family && port.port == checkpoint.port
        })
        {
            return Err(NetworkCatalogError::Invalid);
        }
        ports.push(PortEntry::Published(checkpoint));
        Ok(())
    }

    /// Atomically publishes a bound host snapshot and reserves its port.
    ///
    /// Locks are acquired in `slots` then `ports` order and released before
    /// returning, so callers may perform host work before commit or rollback.
    pub fn prepare_host_bind(
        &self,
        installed: SocketSnapshot,
        port: PortCheckpoint,
    ) -> Result<PreparedBind<'_>, NetworkCatalogError> {
        let admission = self.activity.admit();
        if installed.id != port.owner || !crate::SocketNamespace::valid_checkpoint_snapshot(&installed) {
            return Err(NetworkCatalogError::Invalid);
        }
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let slot = Self::slot_mut(&mut slots, port.owner)?;
        let Some(previous) = slot.socket.as_ref().filter(|socket| matches!(socket.as_ref(), CatalogSocket::Host { .. }))
        else {
            return Err(NetworkCatalogError::Invalid);
        };
        if !Self::owns_port(&installed, &port) {
            return Err(NetworkCatalogError::Invalid);
        }
        let previous = previous.clone();
        let mut ports = self.ports.lock().unwrap_or_else(|error| error.into_inner());
        if ports.iter().any(|reserved| {
            let reserved = reserved.checkpoint();
            reserved.family == port.family && reserved.port == port.port
        })
        {
            return Err(NetworkCatalogError::Invalid);
        }
        let generation = self.port_generation.fetch_add(1, Ordering::Relaxed);
        if generation == 0 {
            return Err(NetworkCatalogError::Capacity);
        }
        ports.push(PortEntry::Prepared {
            checkpoint: port.clone(),
            generation,
        });
        Ok(PreparedBind {
            catalog: self,
            _admission: admission,
            previous,
            installed,
            port,
            generation,
            finalized: false,
        })
    }

    fn commit_prepared(&self, prepared: &mut PreparedBind<'_>) -> Result<(), NetworkCatalogError> {
        if prepared.finalized {
            return Ok(());
        }
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let slot = Self::slot_mut(&mut slots, prepared.port.owner)?;
        let Some(current) = slot.socket.as_ref() else {
            return Err(NetworkCatalogError::Stale);
        };
        if !Arc::ptr_eq(current, &prepared.previous) {
            return Err(NetworkCatalogError::Stale);
        }
        let CatalogSocket::Host { resource, binding, accepted, .. } = current.as_ref() else {
            return Err(NetworkCatalogError::Stale);
        };
        let replacement = Arc::new(CatalogSocket::Host {
            snapshot: prepared.installed.clone(),
            resource: *resource,
            binding: binding.clone(),
            accepted: accepted.clone(),
        });
        let mut ports = self.ports.lock().unwrap_or_else(|error| error.into_inner());
        let Some(entry) = ports.iter_mut().find(|entry| matches!(
            entry,
            PortEntry::Prepared { checkpoint, generation }
                if checkpoint == &prepared.port && *generation == prepared.generation
        )) else {
            return Err(NetworkCatalogError::Stale);
        };
        *entry = PortEntry::Published(prepared.port.clone());
        slot.socket = Some(replacement);
        self.advance_generation();
        prepared.finalized = true;
        Ok(())
    }

    fn rollback_prepared(&self, prepared: &mut PreparedBind<'_>) -> Result<(), NetworkCatalogError> {
        if prepared.finalized {
            return Ok(());
        }
        let slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let mut ports = self.ports.lock().unwrap_or_else(|error| error.into_inner());
        let Some(index) = ports.iter().position(|entry| matches!(
            entry,
            PortEntry::Prepared { checkpoint, generation }
                if checkpoint == &prepared.port && *generation == prepared.generation
        )) else {
            return Err(NetworkCatalogError::Stale);
        };
        ports.remove(index);
        drop(slots);
        prepared.finalized = true;
        Ok(())
    }

    fn owns_port(snapshot: &SocketSnapshot, checkpoint: &PortCheckpoint) -> bool {
        matches!(
            (&snapshot.local, checkpoint.family),
            (
                Some(crate::SocketAddress::Inet4 { port, .. }),
                crate::AddressFamily::Inet4
            ) | (
                Some(crate::SocketAddress::Inet6 { port, .. }),
                crate::AddressFamily::Inet6
            ) if *port == checkpoint.port && checkpoint.port != 0
        )
    }

    pub fn remove(&self, id: SocketId) -> Result<(), NetworkCatalogError> {
        let _admission = self.activity.admit();
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let object = Self::slot_mut(&mut slots, id)?
            .socket
            .take()
            .ok_or(NetworkCatalogError::Stale)?;
        for slot in &mut *slots {
            let related = match slot.socket.as_ref() {
                Some(candidate) => Arc::ptr_eq(candidate, &object),
                None => false,
            };
            if related {
                slot.socket = None;
            }
        }
        let removed = match object.as_ref() {
            CatalogSocket::UnixPair { endpoints, .. } => Some([endpoints[0].id, endpoints[1].id]),
            _ => None,
        };
        if let Some(removed) = removed {
            for slot in &mut *slots {
                let Some(CatalogSocket::Unix {
                    snapshot,
                    pending,
                    datagram,
                }) = slot.socket.as_deref()
                else {
                    continue;
                };
                if pending.iter().any(|id| removed.contains(id)) {
                    slot.socket = Some(Arc::new(CatalogSocket::Unix {
                        snapshot: snapshot.clone(),
                        pending: pending.iter().copied().filter(|id| !removed.contains(id)).collect(),
                        datagram: datagram.clone(),
                    }));
                }
            }
        }
        self.advance_generation();
        self.ports
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|port| port.checkpoint().owner != id);
        Ok(())
    }

    /// Captures every live AF_UNIX endpoint from a single catalog state.
    #[must_use]
    pub fn namespace_view(&self) -> NetworkNamespaceView {
        let _admission = self.activity.admit();
        let slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let mut unix = Vec::new();
        let mut internet = Vec::new();
        for (index, slot) in slots.iter().enumerate() {
            let Some(socket) = slot.socket.as_deref() else { continue };
            match socket {
                CatalogSocket::UnixPair { endpoints, .. } => {
                    if let Some(snapshot) = endpoints
                        .iter()
                        .find(|snapshot| usize::from(snapshot.id.slot) - 1 == index)
                    {
                        unix.push(Self::unix_view(snapshot));
                    }
                }
                CatalogSocket::Unix { snapshot, .. } => unix.push(Self::unix_view(snapshot)),
                CatalogSocket::Host { snapshot, .. } if snapshot.family == crate::AddressFamily::Unix => {
                    unix.push(Self::unix_view(snapshot));
                }
                CatalogSocket::Host { snapshot, .. } => internet.push(InternetSocketView {
                    inode: snapshot.id.generation.wrapping_shl(16) | u64::from(snapshot.id.slot),
                    family: snapshot.family,
                    socket_type: snapshot.socket_type,
                    state: snapshot.state,
                    local: snapshot.local.clone(),
                    peer: snapshot.peer.clone(),
                }),
            }
        }
        unix.sort_by_key(|socket| socket.id);
        NetworkNamespaceView {
            generation: self.generation.load(Ordering::Acquire),
            unix,
            internet,
        }
    }

    fn unix_view(snapshot: &SocketSnapshot) -> UnixSocketView {
        let path = match &snapshot.local {
            Some(crate::SocketAddress::Unix(path)) => Some(path.clone()),
            _ => None,
        };
        UnixSocketView {
            id: snapshot.id,
            inode: snapshot.id.generation.wrapping_shl(16) | u64::from(snapshot.id.slot),
            socket_type: snapshot.socket_type,
            state: snapshot.state,
            path,
        }
    }

    fn advance_generation(&self) {
        let _ = self
            .generation
            .fetch_update(Ordering::Release, Ordering::Relaxed, |value| value.checked_add(1));
    }

    pub fn snapshot(&self, id: SocketId) -> Result<SocketSnapshot, NetworkCatalogError> {
        self.with_snapshot(id, Clone::clone)
    }

    pub fn replace_host_snapshot(&self, id: SocketId, snapshot: SocketSnapshot) -> Result<(), NetworkCatalogError> {
        self.replace_snapshot(id, snapshot)
    }

    pub fn replace_snapshot(&self, id: SocketId, snapshot: SocketSnapshot) -> Result<(), NetworkCatalogError> {
        let _admission = self.activity.admit();
        if snapshot.id != id || !crate::SocketNamespace::valid_checkpoint_snapshot(&snapshot) {
            return Err(NetworkCatalogError::Invalid);
        }
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        let slot = Self::slot_mut(&mut slots, id)?;
        slot.socket = Some(match slot.socket.as_deref() {
            Some(CatalogSocket::Host {
                resource,
                binding,
                accepted,
                ..
            }) => Arc::new(CatalogSocket::Host {
                snapshot,
                resource: *resource,
                binding: binding.clone(),
                accepted: accepted.clone(),
            }),
            Some(CatalogSocket::UnixPair { endpoints, pair }) => {
                let mut endpoints = endpoints.clone();
                let endpoint = endpoints
                    .iter_mut()
                    .find(|value| value.id == id)
                    .ok_or(NetworkCatalogError::Stale)?;
                *endpoint = snapshot;
                Arc::new(CatalogSocket::UnixPair {
                    endpoints,
                    pair: pair.clone(),
                })
            }
            Some(CatalogSocket::Unix { pending, datagram, .. }) => Arc::new(CatalogSocket::Unix {
                snapshot,
                pending: pending.clone(),
                datagram: datagram.clone(),
            }),
            None => return Err(NetworkCatalogError::Stale),
        });
        self.advance_generation();
        Ok(())
    }

    pub fn with_snapshot<R>(
        &self,
        id: SocketId,
        operation: impl FnOnce(&SocketSnapshot) -> R,
    ) -> Result<R, NetworkCatalogError> {
        let _admission = self.activity.admit();
        let slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        match Self::slot(&slots, id)?.socket.as_deref() {
            Some(CatalogSocket::Host { snapshot, binding, .. }) => {
                let _binding_lifetime = binding;
                Ok(operation(snapshot))
            }
            Some(CatalogSocket::UnixPair { endpoints, .. }) => endpoints
                .iter()
                .find(|snapshot| snapshot.id == id)
                .map(operation)
                .ok_or(NetworkCatalogError::Stale),
            Some(CatalogSocket::Unix { snapshot, .. }) => Ok(operation(snapshot)),
            None => Err(NetworkCatalogError::Stale),
        }
    }

    pub fn unix_datagram(&self, id: SocketId) -> Result<Arc<crate::UnixDatagramSocket>, NetworkCatalogError> {
        let _admission = self.activity.admit();
        let slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        match Self::slot(&slots, id)?.socket.as_deref() {
            Some(CatalogSocket::Unix {
                datagram: Some(socket), ..
            }) => Ok(socket.clone()),
            Some(_) => Err(NetworkCatalogError::Invalid),
            None => Err(NetworkCatalogError::Stale),
        }
    }

    fn slot(slots: &[Slot], id: SocketId) -> Result<&Slot, NetworkCatalogError> {
        let index = usize::from(id.slot).checked_sub(1).ok_or(NetworkCatalogError::Stale)?;
        let slot = slots.get(index).ok_or(NetworkCatalogError::Stale)?;
        if slot.generation != id.generation {
            return Err(NetworkCatalogError::Stale);
        }
        Ok(slot)
    }

    fn slot_mut(slots: &mut [Slot], id: SocketId) -> Result<&mut Slot, NetworkCatalogError> {
        let index = usize::from(id.slot).checked_sub(1).ok_or(NetworkCatalogError::Stale)?;
        let slot = slots.get_mut(index).ok_or(NetworkCatalogError::Stale)?;
        if slot.generation != id.generation {
            return Err(NetworkCatalogError::Stale);
        }
        Ok(slot)
    }
}
