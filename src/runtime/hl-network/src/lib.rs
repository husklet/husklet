//! Host-neutral guest socket namespace and lifecycle values.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::RwLock;

mod ancillary;
mod blocking;
mod catalog;
mod checkpoint;
mod checkpoint_activity;
mod egress;
mod listener;
mod platform;
mod policy;
mod port_binding;
mod socket_ofd;
mod unix;
mod view;
pub use ancillary::{
    ControlCodec, ControlEncoding, ControlError, ControlMessage, ControlWord, QueueMessageSnapshot,
    QueueRightsSnapshot, QueueSnapshot, ReceiveControl, SenderCredentials, UnixMessageQueue,
};
pub use catalog::{NetworkCatalog, NetworkCatalogError};
pub use port_binding::PreparedBind;
pub use view::{InternetSocketView, NetworkNamespaceView, UnixSocketView};
pub use checkpoint::{
    AcceptedSocketCheckpoint, AuthoritySocketKey, AuthoritySocketLease, NETWORK_CHECKPOINT_SOCKET_MAXIMUM,
    NETWORK_CHECKPOINT_VERSION, NetworkCatalogRestore, NetworkCheckpointError, NetworkCheckpointImage,
    NetworkCheckpointRebind, NetworkResourceKey, NetworkSocketResource, NetworkSocketState, PortCheckpoint,
};
pub use egress::{BIND_ROUTE_ALIAS_MAXIMUM, BindRoute, EgressInterface, EgressRoute};
pub use listener::{AcceptError, AcceptedToken};
pub use policy::{InterfaceConfiguration, NamespaceInterface, NetworkPolicy, NetworkPolicyError, RouteDisposition};
pub use socket_ofd::{
    AcceptedDescription, SocketConnectError, SocketConnectStatus, SocketDescription, SocketHostError, SocketHostIo,
    SocketHostReadiness,
};
pub use unix::address::{UnixAddress, UnixAddressError};
pub use unix::datagram::{
    UnixDatagramError, UnixDatagramReceive, UnixDatagramRecordSnapshot, UnixDatagramSnapshot, UnixDatagramSocket,
};
pub use unix::named::{
    ConnectReservation as UnixConnectReservation, NamedSocket as UnixNamedSocket,
    NamedSocketError as UnixNamedSocketError, NamedSocketState as UnixNamedSocketState,
};
pub use unix::namespace::{
    Binding as UnixBinding, Namespace as UnixNamespace, NamespaceError as UnixNamespaceError,
    PathnameResolution as UnixPathnameResolution,
};
pub use unix::pair::UnixSocketPair;
pub use unix::snapshot::{UnixEndpointSnapshot, UnixPairSnapshot};
pub use unix::transport::{UnixSocketEndpoint, UnixSocketHost, UnixTransportError};

const SOCKET_MAXIMUM: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SocketId {
    pub slot: u16,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressFamily {
    Unix,
    Inet4,
    Inet6,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketType {
    Stream,
    Datagram,
    SequencePacket,
    Raw,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketProtocol {
    Default,
    Tcp,
    Udp,
    Icmp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SocketAddress {
    Unix(Vec<u8>),
    Inet4 { address: [u8; 4], port: u16 },
    Inet6 { address: [u8; 16], port: u16, scope: u32 },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShutdownState {
    pub read: bool,
    pub write: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketState {
    Created,
    Bound,
    Listening { backlog: u32 },
    Connecting,
    Connected,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocketSnapshot {
    pub id: SocketId,
    pub family: AddressFamily,
    pub socket_type: SocketType,
    pub protocol: SocketProtocol,
    pub state: SocketState,
    pub local: Option<SocketAddress>,
    pub peer: Option<SocketAddress>,
    /// A completed asynchronous connect error until `SO_ERROR` consumes it.
    pub connect_error: Option<SocketConnectError>,
    pub nonblocking: bool,
    pub shutdown: ShutdownState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketError {
    Capacity,
    Stale,
    InvalidTransition,
    AddressInUse,
}

#[derive(Default)]
struct Slot {
    generation: u64,
    socket: Option<SocketSnapshot>,
}

pub struct SocketNamespace {
    slots: RwLock<Vec<Slot>>,
}

impl SocketNamespace {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: RwLock::new(Vec::new()),
        }
    }

    pub fn create(
        &self,
        family: AddressFamily,
        socket_type: SocketType,
        protocol: SocketProtocol,
        nonblocking: bool,
    ) -> Result<SocketId, SocketError> {
        let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = slots
            .iter()
            .position(|slot| slot.socket.is_none())
            .unwrap_or(slots.len());
        if index == SOCKET_MAXIMUM {
            return Err(SocketError::Capacity);
        }
        if index == slots.len() {
            slots.push(Slot::default());
        }
        let slot = &mut slots[index];
        slot.generation = slot.generation.checked_add(1).ok_or(SocketError::Capacity)?;
        let id = SocketId {
            slot: u16::try_from(index + 1).expect("bounded socket slot"),
            generation: slot.generation,
        };
        slot.socket = Some(SocketSnapshot {
            id,
            family,
            socket_type,
            protocol,
            state: SocketState::Created,
            local: None,
            peer: None,
            connect_error: None,
            nonblocking,
            shutdown: ShutdownState::default(),
        });
        Ok(id)
    }

    pub fn update(&self, id: SocketId, operation: SocketOperation) -> Result<(), SocketError> {
        let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let socket = Self::socket_mut(&mut slots, id)?;
        match operation {
            SocketOperation::Bind(address) if socket.state == SocketState::Created => {
                socket.local = Some(address);
                socket.state = SocketState::Bound;
            }
            SocketOperation::Listen(backlog)
                if socket.socket_type == SocketType::Stream && matches!(socket.state, SocketState::Bound) =>
            {
                socket.state = SocketState::Listening { backlog };
            }
            SocketOperation::BeginConnect(address)
                if matches!(socket.state, SocketState::Created | SocketState::Bound) =>
            {
                socket.peer = Some(address);
                socket.state = SocketState::Connecting;
            }
            SocketOperation::FinishConnect if socket.state == SocketState::Connecting => {
                socket.state = SocketState::Connected;
            }
            SocketOperation::Shutdown(shutdown) if socket.state == SocketState::Connected => {
                socket.shutdown.read |= shutdown.read;
                socket.shutdown.write |= shutdown.write;
            }
            SocketOperation::SetNonblocking(value) => socket.nonblocking = value,
            _ => return Err(SocketError::InvalidTransition),
        }
        Ok(())
    }

    pub fn close(&self, id: SocketId) -> Result<(), SocketError> {
        let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let socket = Self::socket_mut(&mut slots, id)?;
        socket.state = SocketState::Closed;
        let index = usize::from(id.slot) - 1;
        slots[index].socket = None;
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self, id: SocketId) -> Option<SocketSnapshot> {
        let slots = self.slots.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = usize::from(id.slot).checked_sub(1)?;
        slots
            .get(index)?
            .socket
            .as_ref()
            .filter(|socket| socket.id == id)
            .cloned()
    }

    pub fn restore(snapshots: &[SocketSnapshot]) -> Result<Self, SocketError> {
        if snapshots.len() > SOCKET_MAXIMUM {
            return Err(SocketError::Capacity);
        }
        let mut slots = Vec::new();
        for snapshot in snapshots {
            let index = usize::from(snapshot.id.slot)
                .checked_sub(1)
                .ok_or(SocketError::InvalidTransition)?;
            if index >= SOCKET_MAXIMUM || snapshot.id.generation == 0 || !Self::valid_checkpoint_snapshot(snapshot) {
                return Err(SocketError::InvalidTransition);
            }
            slots.resize_with(index + 1, Slot::default);
            if slots[index].generation != 0 || slots[index].socket.is_some() {
                return Err(SocketError::InvalidTransition);
            }
            slots[index] = Slot {
                generation: snapshot.id.generation,
                socket: Some(snapshot.clone()),
            };
        }
        Ok(Self {
            slots: RwLock::new(slots),
        })
    }

    pub(crate) fn valid_checkpoint_snapshot(snapshot: &SocketSnapshot) -> bool {
        let valid_state = match snapshot.state {
            SocketState::Created => snapshot.local.is_none() && snapshot.peer.is_none(),
            SocketState::Bound => snapshot.local.is_some() && snapshot.peer.is_none(),
            SocketState::Listening { .. } => {
                matches!(snapshot.socket_type, SocketType::Stream | SocketType::SequencePacket)
                    && snapshot.local.is_some()
                    && snapshot.peer.is_none()
            }
            SocketState::Connecting => snapshot.peer.is_some(),
            SocketState::Connected => snapshot.peer.is_some(),
            SocketState::Closed => false,
        };
        valid_state
            && !matches!(
                snapshot.connect_error,
                Some(SocketConnectError::InProgress | SocketConnectError::Already | SocketConnectError::Connected)
            )
            && (snapshot.connect_error.is_none() || snapshot.state == SocketState::Connecting)
    }

    fn socket_mut(slots: &mut [Slot], id: SocketId) -> Result<&mut SocketSnapshot, SocketError> {
        let index = usize::from(id.slot).checked_sub(1).ok_or(SocketError::Stale)?;
        slots
            .get_mut(index)
            .ok_or(SocketError::Stale)?
            .socket
            .as_mut()
            .filter(|socket| socket.id == id)
            .ok_or(SocketError::Stale)
    }
}

impl Default for SocketNamespace {
    fn default() -> Self {
        Self::new()
    }
}

pub enum SocketOperation {
    Bind(SocketAddress),
    Listen(u32),
    BeginConnect(SocketAddress),
    FinishConnect,
    Shutdown(ShutdownState),
    SetNonblocking(bool),
}

pub trait SocketHost {
    type Token;
    type Error;

    fn open(&self, snapshot: &SocketSnapshot) -> Result<Self::Token, Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Route {
    pub family: AddressFamily,
    pub destination: [u8; 16],
    pub prefix_bits: u8,
    pub gateway: Option<[u8; 16]>,
    pub interface: u32,
    pub metric: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkConfiguration {
    pub routes: Vec<Route>,
    pub dns_servers: Vec<SocketAddress>,
    pub search_domains: Vec<String>,
}

impl NetworkConfiguration {
    pub fn new(
        routes: Vec<Route>,
        dns_servers: Vec<SocketAddress>,
        search_domains: Vec<String>,
    ) -> Result<Self, SocketError> {
        if routes.len() > 256
            || dns_servers.len() > 8
            || search_domains.len() > 16
            || routes.iter().any(|route| !Self::route_valid(route))
            || dns_servers
                .iter()
                .any(|server| !matches!(server, SocketAddress::Inet4 { .. } | SocketAddress::Inet6 { .. }))
            || search_domains.iter().any(|domain| !Self::domain_valid(domain))
        {
            return Err(SocketError::Capacity);
        }
        Ok(Self {
            routes,
            dns_servers,
            search_domains,
        })
    }

    pub fn restore(snapshot: &Self) -> Result<Self, SocketError> {
        Self::new(
            snapshot.routes.clone(),
            snapshot.dns_servers.clone(),
            snapshot.search_domains.clone(),
        )
    }

    fn route_valid(route: &Route) -> bool {
        match route.family {
            AddressFamily::Inet4 => route.prefix_bits <= 32,
            AddressFamily::Inet6 => route.prefix_bits <= 128,
            AddressFamily::Unix => false,
        }
    }

    fn domain_valid(domain: &str) -> bool {
        !domain.is_empty()
            && domain.len() <= 253
            && domain.split('.').all(|label| !label.is_empty() && label.len() <= 63)
    }
}

pub struct RouteTable {
    routes: Vec<Route>,
}

impl RouteTable {
    pub fn new(routes: Vec<Route>) -> Result<Self, SocketError> {
        NetworkConfiguration::new(routes.clone(), Vec::new(), Vec::new())?;
        Ok(Self { routes })
    }

    #[must_use]
    pub fn lookup(&self, family: AddressFamily, address: [u8; 16]) -> Option<&Route> {
        self.routes
            .iter()
            .enumerate()
            .filter(|(_, route)| route.family == family && Self::matches(route, address))
            .max_by(|(left_index, left), (right_index, right)| {
                left.prefix_bits
                    .cmp(&right.prefix_bits)
                    .then_with(|| right.metric.cmp(&left.metric))
                    .then_with(|| right_index.cmp(left_index))
            })
            .map(|(_, route)| route)
    }

    fn matches(route: &Route, address: [u8; 16]) -> bool {
        let full = usize::from(route.prefix_bits / 8);
        let remainder = route.prefix_bits % 8;
        if address[..full] != route.destination[..full] {
            return false;
        }
        if remainder == 0 {
            return true;
        }
        let mask = u8::MAX << (8 - remainder);
        address[full] & mask == route.destination[full] & mask
    }
}

pub struct PortRegistry {
    owners: RwLock<BTreeMap<(AddressFamilyKey, u16), SocketId>>,
    first_ephemeral: u16,
    last_ephemeral: u16,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum AddressFamilyKey {
    Inet4,
    Inet6,
}

impl PortRegistry {
    #[must_use]
    pub const fn new(first_ephemeral: u16, last_ephemeral: u16) -> Self {
        Self {
            owners: RwLock::new(BTreeMap::new()),
            first_ephemeral,
            last_ephemeral,
        }
    }

    pub fn claim(&self, family: AddressFamily, requested: u16, owner: SocketId) -> Result<u16, SocketError> {
        let key = match family {
            AddressFamily::Inet4 => AddressFamilyKey::Inet4,
            AddressFamily::Inet6 => AddressFamilyKey::Inet6,
            AddressFamily::Unix => return Err(SocketError::InvalidTransition),
        };
        let mut owners = self.owners.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if requested != 0 {
            match owners.entry((key, requested)) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(owner);
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(SocketError::AddressInUse);
                }
            }
            return Ok(requested);
        }
        for port in self.first_ephemeral..=self.last_ephemeral {
            if let std::collections::btree_map::Entry::Vacant(entry) = owners.entry((key, port)) {
                entry.insert(owner);
                return Ok(port);
            }
        }
        Err(SocketError::Capacity)
    }

    pub fn release(&self, family: AddressFamily, port: u16, owner: SocketId) {
        let key = match family {
            AddressFamily::Inet4 => AddressFamilyKey::Inet4,
            AddressFamily::Inet6 => AddressFamilyKey::Inet6,
            AddressFamily::Unix => return,
        };
        let mut owners = self.owners.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if owners.get(&(key, port)) == Some(&owner) {
            owners.remove(&(key, port));
        }
    }

    #[must_use]
    pub fn owner(&self, family: AddressFamily, port: u16) -> Option<SocketId> {
        let key = match family {
            AddressFamily::Inet4 => AddressFamilyKey::Inet4,
            AddressFamily::Inet6 => AddressFamilyKey::Inet6,
            AddressFamily::Unix => return None,
        };
        self.owners
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(key, port))
            .copied()
    }
}

#[cfg(test)]
mod catalog_test;
#[cfg(test)]
mod ofd_test;
#[cfg(test)]
mod test;
#[cfg(test)]
#[path = "unix/transport_test.rs"]
mod unix_transport_test;
