use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::num::NonZeroU64;
use std::sync::Arc;

use hl_descriptor::DescriptionRef;

use crate::{
    AddressFamily, NetworkConfiguration, SocketAddress, SocketId, SocketSnapshot, UnixDatagramSnapshot,
    UnixPairSnapshot,
};

pub const NETWORK_CHECKPOINT_VERSION: u32 = 6;
pub const NETWORK_CHECKPOINT_SOCKET_MAXIMUM: usize = 4096;
const NETWORK_CHECKPOINT_LISTENER_MAXIMUM: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct NetworkResourceKey(NonZeroU64);

impl NetworkResourceKey {
    pub fn new(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0.get()
    }
}

/// Generation-qualified capability naming an OFD retained by host authority.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AuthoritySocketKey {
    slot: NonZeroU32,
    generation: NonZeroU64,
}

impl AuthoritySocketKey {
    #[must_use]
    pub fn new(slot: u32, generation: u64) -> Option<Self> {
        Some(Self {
            slot: NonZeroU32::new(slot)?,
            generation: NonZeroU64::new(generation)?,
        })
    }

    #[must_use]
    pub const fn slot(self) -> u32 {
        self.slot.get()
    }

    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthoritySocketLease {
    pub resource: NetworkResourceKey,
    pub key: AuthoritySocketKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortCheckpoint {
    pub family: AddressFamily,
    pub port: u16,
    pub owner: SocketId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedSocketCheckpoint {
    pub resource: NetworkResourceKey,
    pub local: SocketAddress,
    pub peer: SocketAddress,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkSocketState {
    Host {
        snapshot: SocketSnapshot,
        resource: NetworkResourceKey,
        accepted: Vec<AcceptedSocketCheckpoint>,
    },
    UnixPair {
        endpoints: [SocketSnapshot; 2],
        pair: UnixPairSnapshot,
    },
    Unix {
        snapshot: SocketSnapshot,
        pending: Vec<SocketId>,
        datagram: Option<UnixDatagramSnapshot>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkCheckpointImage {
    pub version: u32,
    pub generations: Vec<u64>,
    pub configuration: NetworkConfiguration,
    pub ports: Vec<PortCheckpoint>,
    pub authority: Vec<AuthoritySocketLease>,
    pub sockets: Vec<NetworkSocketState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkCheckpointError {
    InvalidImage,
    ResourceLimit,
}

pub trait NetworkSocketResource: Send + Sync {}
impl<T: Send + Sync> NetworkSocketResource for T {}

pub trait NetworkCatalogRestore: Send {
    fn host_socket(
        &mut self,
        snapshot: &SocketSnapshot,
        resource: NetworkResourceKey,
    ) -> Result<Arc<dyn NetworkSocketResource>, NetworkCheckpointError>;
    fn accepted_socket(&mut self, accepted: &AcceptedSocketCheckpoint) -> Result<(), NetworkCheckpointError>;
    fn descriptor(&mut self, identity: u64) -> Result<DescriptionRef, NetworkCheckpointError>;
    fn unix_pair(&mut self, pair: &UnixPairSnapshot) -> Result<Arc<crate::UnixSocketPair>, NetworkCheckpointError> {
        crate::UnixSocketPair::restore(pair, hl_descriptor::StatusFlags::default(), |identity| {
            self.descriptor(identity).ok()
        })
        .map(Arc::new)
        .map_err(|_| NetworkCheckpointError::InvalidImage)
    }
    fn bind_catalog(&mut self, _catalog: Arc<crate::NetworkCatalog>) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }
    fn commit(&mut self) -> Result<(), NetworkCheckpointError>;
    fn rollback(&mut self);
    fn resume(&mut self) -> Result<(), NetworkCheckpointError>;
}

pub trait NetworkCheckpointRebind: Send + Sync {
    fn capture_prepare(&self) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }
    fn capture(&self, _image: &mut NetworkCheckpointImage) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }
    fn capture_publish(&self, _digest: [u8; 32]) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }
    fn capture_abort(&self) {}
    fn capture_finish(&self) {}
    fn stage(&self, image: &NetworkCheckpointImage) -> Result<Box<dyn NetworkCatalogRestore>, NetworkCheckpointError>;
    fn stage_bound(
        &self,
        _digest: [u8; 32],
        image: &NetworkCheckpointImage,
    ) -> Result<Box<dyn NetworkCatalogRestore>, NetworkCheckpointError> {
        self.stage(image)
    }
}

impl NetworkCheckpointImage {
    pub fn validate(&self) -> Result<(), NetworkCheckpointError> {
        if self.version != NETWORK_CHECKPOINT_VERSION
            || self.generations.len() > NETWORK_CHECKPOINT_SOCKET_MAXIMUM
            || NetworkConfiguration::restore(&self.configuration).is_err()
        {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let mut ids = BTreeMap::new();
        let mut resources = BTreeSet::new();
        let mut previous = None;
        for socket in &self.sockets {
            let first = Self::validate_resources(socket, &mut resources)?;
            if matches!(previous, Some(value) if value >= first) {
                return Err(NetworkCheckpointError::InvalidImage);
            }
            previous = Some(first);
            self.validate_network_socket(socket, &mut ids)?;
        }
        self.validate_unix_pending(&ids)?;
        if self.authority.len() > NETWORK_CHECKPOINT_SOCKET_MAXIMUM {
            return Err(NetworkCheckpointError::ResourceLimit);
        }
        let mut authority_resources = BTreeSet::new();
        let mut authority_keys = BTreeSet::new();
        for lease in &self.authority {
            if !resources.contains(&lease.resource)
                || !authority_resources.insert(lease.resource)
                || !authority_keys.insert(lease.key)
            {
                return Err(NetworkCheckpointError::InvalidImage);
            }
        }
        self.validate_ports(&ids)
    }

    fn validate_resources(
        socket: &NetworkSocketState,
        resources: &mut BTreeSet<NetworkResourceKey>,
    ) -> Result<SocketId, NetworkCheckpointError> {
        let (snapshot, resource, accepted) = match socket {
            NetworkSocketState::Host {
                snapshot,
                resource,
                accepted,
            } => (snapshot, resource, accepted),
            NetworkSocketState::UnixPair { endpoints, .. } => return Ok(endpoints[0].id),
            NetworkSocketState::Unix { snapshot, .. } => return Ok(snapshot.id),
        };
        if !resources.insert(*resource) {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        for item in accepted {
            if !resources.insert(item.resource) {
                return Err(NetworkCheckpointError::InvalidImage);
            }
        }
        Ok(snapshot.id)
    }

    fn validate_network_socket<'image>(
        &'image self,
        socket: &'image NetworkSocketState,
        ids: &mut BTreeMap<SocketId, &'image SocketSnapshot>,
    ) -> Result<(), NetworkCheckpointError> {
        match socket {
            NetworkSocketState::Host { snapshot, accepted, .. } => {
                self.validate_socket(snapshot, ids)?;
                if accepted.len() > NETWORK_CHECKPOINT_LISTENER_MAXIMUM {
                    return Err(NetworkCheckpointError::ResourceLimit);
                }
            }
            NetworkSocketState::UnixPair { endpoints, pair } => {
                self.validate_unix_pair(endpoints, pair, ids)?;
            }
            NetworkSocketState::Unix {
                snapshot,
                pending,
                datagram,
            } => {
                self.validate_socket(snapshot, ids)?;
                let datagram_valid = match (snapshot.socket_type, datagram) {
                    (crate::SocketType::Datagram, Some(image)) => {
                        let peer = snapshot.peer.as_ref().and_then(Self::unix_address);
                        crate::UnixDatagramSocket::restore(image).is_ok()
                            && image.connected == peer
                            && matches!(snapshot.state, crate::SocketState::Created | crate::SocketState::Bound)
                                == image.connected.is_none()
                    }
                    (crate::SocketType::Datagram, None) => false,
                    (_, None) => true,
                    (_, Some(_)) => false,
                };
                if snapshot.family != AddressFamily::Unix
                    || pending.len() > NETWORK_CHECKPOINT_LISTENER_MAXIMUM
                    || !datagram_valid
                    || datagram.is_some() && !pending.is_empty()
                {
                    return Err(NetworkCheckpointError::InvalidImage);
                }
            }
        }
        Ok(())
    }

    fn validate_unix_pending(&self, ids: &BTreeMap<SocketId, &SocketSnapshot>) -> Result<(), NetworkCheckpointError> {
        let mut claimed = BTreeSet::new();
        for socket in &self.sockets {
            let NetworkSocketState::Unix {
                snapshot: listener,
                pending,
                ..
            } = socket
            else {
                continue;
            };
            let crate::SocketState::Listening { backlog } = listener.state else {
                if pending.is_empty() {
                    continue;
                }
                return Err(NetworkCheckpointError::InvalidImage);
            };
            if u32::try_from(pending.len()).map_or(true, |count| count > backlog) {
                return Err(NetworkCheckpointError::InvalidImage);
            }
            for id in pending {
                let Some(accepted) = ids.get(id) else {
                    return Err(NetworkCheckpointError::InvalidImage);
                };
                if !claimed.insert(*id)
                    || !self.sockets.iter().any(|socket| {
                        matches!(socket, NetworkSocketState::UnixPair { endpoints, .. } if endpoints[1].id == *id)
                    })
                    || accepted.family != AddressFamily::Unix
                    || accepted.state != crate::SocketState::Connected
                    || accepted.local != listener.local
                {
                    return Err(NetworkCheckpointError::InvalidImage);
                }
            }
        }
        Ok(())
    }

    fn validate_unix_pair<'image>(
        &'image self,
        endpoints: &'image [SocketSnapshot; 2],
        pair: &UnixPairSnapshot,
        ids: &mut BTreeMap<SocketId, &'image SocketSnapshot>,
    ) -> Result<(), NetworkCheckpointError> {
        self.validate_socket(&endpoints[0], ids)?;
        self.validate_socket(&endpoints[1], ids)?;
        pair.validate().map_err(|_| NetworkCheckpointError::InvalidImage)?;
        if endpoints[0].family != AddressFamily::Unix
            || endpoints[1].family != AddressFamily::Unix
            || endpoints[0].state != crate::SocketState::Connected
            || endpoints[1].state != crate::SocketState::Connected
            || endpoints[0].peer != endpoints[1].local
            || endpoints[1].peer != endpoints[0].local
        {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn validate_socket<'image>(
        &self,
        snapshot: &'image SocketSnapshot,
        ids: &mut BTreeMap<SocketId, &'image SocketSnapshot>,
    ) -> Result<(), NetworkCheckpointError> {
        let index = usize::from(snapshot.id.slot)
            .checked_sub(1)
            .ok_or(NetworkCheckpointError::InvalidImage)?;
        if index >= self.generations.len()
            || self.generations[index] != snapshot.id.generation
            || !crate::SocketNamespace::valid_checkpoint_snapshot(snapshot)
            || ids.insert(snapshot.id, snapshot).is_some()
        {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn validate_ports(&self, ids: &BTreeMap<SocketId, &SocketSnapshot>) -> Result<(), NetworkCheckpointError> {
        let mut claimed = BTreeSet::new();
        let mut previous = None;
        for port in &self.ports {
            let Some(owner) = ids.get(&port.owner) else {
                return Err(NetworkCheckpointError::InvalidImage);
            };
            let key = (port.family as u8, port.port);
            if matches!(previous, Some(value) if value >= key)
                || port.port == 0
                || !claimed.insert(key)
                || !Self::address_owns_port(owner.local.as_ref(), port.family, port.port)
            {
                return Err(NetworkCheckpointError::InvalidImage);
            }
            previous = Some(key);
        }
        Ok(())
    }

    fn address_owns_port(address: Option<&SocketAddress>, family: AddressFamily, port: u16) -> bool {
        matches!(
            (address, family),
            (Some(SocketAddress::Inet4 { port: value, .. }), AddressFamily::Inet4)
                | (Some(SocketAddress::Inet6 { port: value, .. }), AddressFamily::Inet6)
                if *value == port
        )
    }

    fn unix_address(address: &SocketAddress) -> Option<crate::UnixAddress> {
        let SocketAddress::Unix(raw) = address else {
            return None;
        };
        Some(if raw.is_empty() {
            crate::UnixAddress::Unnamed
        } else if raw[0] == 0 {
            crate::UnixAddress::Abstract(raw[1..].to_vec())
        } else {
            crate::UnixAddress::Pathname(raw.clone())
        })
    }
}
