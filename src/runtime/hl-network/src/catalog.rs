use crate::port_binding::PortEntry;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{
    AcceptedSocketCheckpoint, NETWORK_CHECKPOINT_SOCKET_MAXIMUM, NetworkCheckpointError, NetworkConfiguration,
    NetworkResourceKey, NetworkSocketResource, SocketId, SocketSnapshot, UnixSocketPair,
};

mod checkpoint;
mod unix;

pub(crate) enum CatalogSocket {
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

pub(crate) struct Slot {
    pub(crate) generation: u64,
    pub(crate) socket: Option<Arc<CatalogSocket>>,
}

pub struct NetworkCatalog {
    pub(crate) configuration: NetworkConfiguration,
    pub(crate) ports: Mutex<Vec<PortEntry>>,
    pub(crate) slots: Mutex<Vec<Slot>>,
    pub(crate) generation: AtomicU64,
    pub(crate) port_generation: AtomicU64,
    pub(crate) activity: crate::checkpoint_activity::CheckpointActivity,
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
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
    pub fn remove(&self, id: SocketId) -> Result<(), NetworkCatalogError> {
        let _admission = self.activity.admit();
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
            Self::drop_pending(&mut slots, removed);
        }
        self.advance_generation();
        self.ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|port| port.checkpoint().owner != id);
        Ok(())
    }
    fn drop_pending(slots: &mut [Slot], removed: [SocketId; 2]) {
        for slot in slots {
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

    pub(crate) fn advance_generation(&self) {
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
        let mut slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match Self::slot(&slots, id)?.socket.as_deref() {
            Some(CatalogSocket::Host { snapshot, binding, .. }) => {
                // Naming the binding documents that it must outlive the operation below.
                #[allow(clippy::no_effect_underscore_binding)]
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
        let slots = self.slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match Self::slot(&slots, id)?.socket.as_deref() {
            Some(CatalogSocket::Unix {
                datagram: Some(socket), ..
            }) => Ok(socket.clone()),
            Some(_) => Err(NetworkCatalogError::Invalid),
            None => Err(NetworkCatalogError::Stale),
        }
    }

    pub(crate) fn slot(slots: &[Slot], id: SocketId) -> Result<&Slot, NetworkCatalogError> {
        let index = usize::from(id.slot).checked_sub(1).ok_or(NetworkCatalogError::Stale)?;
        let slot = slots.get(index).ok_or(NetworkCatalogError::Stale)?;
        if slot.generation != id.generation {
            return Err(NetworkCatalogError::Stale);
        }
        Ok(slot)
    }

    pub(crate) fn slot_mut(slots: &mut [Slot], id: SocketId) -> Result<&mut Slot, NetworkCatalogError> {
        let index = usize::from(id.slot).checked_sub(1).ok_or(NetworkCatalogError::Stale)?;
        let slot = slots.get_mut(index).ok_or(NetworkCatalogError::Stale)?;
        if slot.generation != id.generation {
            return Err(NetworkCatalogError::Stale);
        }
        Ok(slot)
    }
}
