//! Host-neutral guest socket namespace and lifecycle values.

#![forbid(unsafe_code)]


mod ancillary;
mod blocking;
mod catalog;
mod configuration;
mod checkpoint;
mod checkpoint_activity;
mod egress;
mod listener;
mod platform;
mod policy;
mod port_binding;
mod port_registry;
mod routing;
mod socket_namespace;
mod socket_ofd;
mod unix;
mod view;
pub use configuration::NetworkConfiguration;
pub use port_registry::PortRegistry;
pub use routing::{Route, RouteTable};
pub use socket_namespace::{SocketHost, SocketNamespace, SocketOperation};
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

pub(crate) const SOCKET_MAXIMUM: usize = 4096;

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

#[cfg(test)]
mod catalog_test;
#[cfg(test)]
mod ofd_test;
#[cfg(test)]
mod test;
#[cfg(test)]
#[path = "unix/transport_test.rs"]
mod unix_transport_test;
