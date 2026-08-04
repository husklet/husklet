use hl_network::{
    AcceptedSocketCheckpoint, AddressFamily, ControlMessage, NETWORK_CHECKPOINT_SOCKET_MAXIMUM, NetworkConfiguration,
    NetworkSocketState, PortCheckpoint, QueueMessageSnapshot, QueueRightsSnapshot, QueueSnapshot, Route,
    SenderCredentials, SocketAddress, SocketId, SocketProtocol, SocketSnapshot, SocketState, SocketType, UnixAddress,
    UnixDatagramSnapshot, UnixEndpointSnapshot, UnixPairSnapshot,
};

use super::NETWORK_CHECKPOINT_BYTES_MAXIMUM;

#[derive(Default)]
pub(super) struct Output {
    pub(super) bytes: Vec<u8>,
}

impl Output {
    fn extend(&mut self, value: &[u8]) -> Result<(), ()> {
        let end = self.bytes.len().checked_add(value.len()).ok_or(())?;
        if end > NETWORK_CHECKPOINT_BYTES_MAXIMUM {
            return Err(());
        }
        self.bytes.extend_from_slice(value);
        Ok(())
    }
    fn u8(&mut self, value: u8) -> Result<(), ()> {
        self.extend(&[value])
    }
    fn bool(&mut self, value: bool) -> Result<(), ()> {
        self.u8(u8::from(value))
    }
    fn i32(&mut self, value: i32) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }
    fn u16(&mut self, value: u16) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }
    pub(super) fn u32(&mut self, value: u32) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }
    pub(super) fn u64(&mut self, value: u64) -> Result<(), ()> {
        self.extend(&value.to_le_bytes())
    }
    pub(super) fn cardinality(&mut self, value: usize, maximum: usize) -> Result<(), ()> {
        if value > maximum {
            return Err(());
        }
        self.u32(u32::try_from(value).map_err(|_| ())?)
    }
    fn length(&mut self, value: usize) -> Result<(), ()> {
        if value > NETWORK_CHECKPOINT_BYTES_MAXIMUM {
            return Err(());
        }
        self.u32(u32::try_from(value).map_err(|_| ())?)
    }
    pub(super) fn capacity(&mut self, value: usize) -> Result<(), ()> {
        self.u64(u64::try_from(value).map_err(|_| ())?)
    }
    fn bytes(&mut self, value: &[u8]) -> Result<(), ()> {
        self.length(value.len())?;
        self.extend(value)
    }
    fn text(&mut self, value: &str) -> Result<(), ()> {
        self.bytes(value.as_bytes())
    }
    fn family(&mut self, value: AddressFamily) -> Result<(), ()> {
        self.u8(match value {
            AddressFamily::Unix => 1,
            AddressFamily::Inet4 => 2,
            AddressFamily::Inet6 => 3,
        })
    }
    fn socket_type(&mut self, value: SocketType) -> Result<(), ()> {
        self.u8(match value {
            SocketType::Stream => 1,
            SocketType::Datagram => 2,
            SocketType::SequencePacket => 3,
            SocketType::Raw => 4,
        })
    }
    fn protocol(&mut self, value: SocketProtocol) -> Result<(), ()> {
        self.u8(match value {
            SocketProtocol::Default => 1,
            SocketProtocol::Tcp => 2,
            SocketProtocol::Udp => 3,
            SocketProtocol::Icmp => 4,
        })
    }
    fn id(&mut self, value: SocketId) -> Result<(), ()> {
        self.u16(value.slot)?;
        self.u64(value.generation)
    }
    fn address(&mut self, value: &SocketAddress) -> Result<(), ()> {
        match value {
            SocketAddress::Unix(path) => {
                self.u8(1)?;
                self.bytes(path)
            }
            SocketAddress::Inet4 { address, port } => {
                self.u8(2)?;
                self.extend(address)?;
                self.u16(*port)
            }
            SocketAddress::Inet6 { address, port, scope } => {
                self.u8(3)?;
                self.extend(address)?;
                self.u16(*port)?;
                self.u32(*scope)
            }
        }
    }
    fn optional_address(&mut self, value: Option<&SocketAddress>) -> Result<(), ()> {
        self.bool(value.is_some())?;
        if let Some(value) = value {
            self.address(value)?;
        }
        Ok(())
    }
    fn unix_address(&mut self, value: &UnixAddress) -> Result<(), ()> {
        match value {
            UnixAddress::Unnamed => self.u8(0),
            UnixAddress::Pathname(value) => {
                self.u8(1)?;
                self.bytes(value)
            }
            UnixAddress::Abstract(value) => {
                self.u8(2)?;
                self.bytes(value)
            }
        }
    }
    fn unix_datagram(&mut self, value: &UnixDatagramSnapshot) -> Result<(), ()> {
        self.capacity(value.capacity)?;
        self.bool(value.connected.is_some())?;
        if let Some(peer) = &value.connected {
            self.unix_address(peer)?;
        }
        self.cardinality(value.records.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        for record in &value.records {
            self.bytes(&record.payload)?;
            self.unix_address(&record.source)?;
        }
        self.bool(value.closed)
    }
    fn connect_error(&mut self, value: Option<hl_network::SocketConnectError>) -> Result<(), ()> {
        self.u8(match value {
            None => 0,
            Some(hl_network::SocketConnectError::InProgress) => 1,
            Some(hl_network::SocketConnectError::Already) => 2,
            Some(hl_network::SocketConnectError::Connected) => 3,
            Some(hl_network::SocketConnectError::Interrupted) => 4,
            Some(hl_network::SocketConnectError::Refused) => 5,
            Some(hl_network::SocketConnectError::TimedOut) => 6,
            Some(hl_network::SocketConnectError::Canceled) => 7,
            Some(hl_network::SocketConnectError::Io) => 8,
        })
    }
    fn snapshot(&mut self, value: &SocketSnapshot) -> Result<(), ()> {
        self.id(value.id)?;
        self.family(value.family)?;
        self.socket_type(value.socket_type)?;
        self.protocol(value.protocol)?;
        match value.state {
            SocketState::Created => self.u8(1)?,
            SocketState::Bound => self.u8(2)?,
            SocketState::Listening { backlog } => {
                self.u8(3)?;
                self.u32(backlog)?;
            }
            SocketState::Connecting => self.u8(4)?,
            SocketState::Connected => self.u8(5)?,
            SocketState::Closed => self.u8(6)?,
        }
        self.optional_address(value.local.as_ref())?;
        self.optional_address(value.peer.as_ref())?;
        self.connect_error(value.connect_error)?;
        self.bool(value.nonblocking)?;
        self.bool(value.shutdown.read)?;
        self.bool(value.shutdown.write)
    }
    fn route(&mut self, value: &Route) -> Result<(), ()> {
        self.family(value.family)?;
        self.extend(&value.destination)?;
        self.u8(value.prefix_bits)?;
        self.bool(value.gateway.is_some())?;
        if let Some(gateway) = value.gateway {
            self.extend(&gateway)?;
        }
        self.u32(value.interface)?;
        self.u32(value.metric)
    }
    pub(super) fn configuration(&mut self, value: &NetworkConfiguration) -> Result<(), ()> {
        self.cardinality(value.routes.len(), 256)?;
        self.cardinality(value.dns_servers.len(), 8)?;
        self.cardinality(value.search_domains.len(), 16)?;
        for route in &value.routes {
            self.route(route)?;
        }
        for server in &value.dns_servers {
            self.address(server)?;
        }
        for domain in &value.search_domains {
            self.text(domain)?;
        }
        Ok(())
    }
    pub(super) fn port(&mut self, value: &PortCheckpoint) -> Result<(), ()> {
        self.family(value.family)?;
        self.u16(value.port)?;
        self.id(value.owner)
    }
    fn accepted(&mut self, value: &AcceptedSocketCheckpoint) -> Result<(), ()> {
        self.u64(value.resource.value())?;
        self.address(&value.local)?;
        self.address(&value.peer)
    }
    fn control(&mut self, value: &ControlMessage) -> Result<(), ()> {
        match value {
            ControlMessage::Rights(numbers) => {
                self.u8(1)?;
                self.cardinality(numbers.len(), 253)?;
                for number in numbers {
                    self.i32(*number)?;
                }
            }
            ControlMessage::Credentials { process, user, group } => {
                self.u8(2)?;
                self.u32(*process)?;
                self.u32(*user)?;
                self.u32(*group)?;
            }
            ControlMessage::Unknown { level, kind, data } => {
                self.u8(3)?;
                self.i32(*level)?;
                self.i32(*kind)?;
                self.bytes(data)?;
            }
        }
        Ok(())
    }
    fn queue(&mut self, value: &QueueSnapshot) -> Result<(), ()> {
        self.cardinality(value.messages.len(), 1024)?;
        for message in &value.messages {
            self.queue_message(message)?;
        }
        Ok(())
    }
    fn queue_message(&mut self, message: &QueueMessageSnapshot) -> Result<(), ()> {
        self.bytes(&message.payload)?;
        self.credentials(message.credentials)?;
        self.bool(message.automatic)?;
        self.cardinality(message.controls.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        for control in &message.controls {
            self.control(control)?;
        }
        self.cardinality(message.rights.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        for group in &message.rights {
            self.rights(group)?;
        }
        Ok(())
    }
    fn credentials(&mut self, value: Option<SenderCredentials>) -> Result<(), ()> {
        self.bool(value.is_some())?;
        if let Some(value) = value {
            self.u32(value.process)?;
            self.u32(value.user)?;
            self.u32(value.group)?;
        }
        Ok(())
    }
    fn rights(&mut self, group: &QueueRightsSnapshot) -> Result<(), ()> {
        self.cardinality(group.identities.len(), 253)?;
        for identity in &group.identities {
            self.u64(*identity)?;
        }
        Ok(())
    }
    fn endpoint(&mut self, value: &UnixEndpointSnapshot) -> Result<(), ()> {
        match &value.address {
            hl_network::UnixAddress::Unnamed => self.u8(1)?,
            hl_network::UnixAddress::Pathname(path) => {
                self.u8(2)?;
                self.bytes(path)?;
            }
            hl_network::UnixAddress::Abstract(path) => {
                self.u8(3)?;
                self.bytes(path)?;
            }
        }
        self.cardinality(value.incoming.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        for record in &value.incoming {
            self.bytes(record)?;
        }
        self.bool(value.peer_write_shutdown)?;
        self.bool(value.read_shutdown)?;
        self.bool(value.write_shutdown)?;
        self.bool(value.closed)?;
        self.bool(value.passcred)?;
        self.bool(value.peer_credentials.is_some())?;
        if let Some(credentials) = value.peer_credentials {
            self.u32(credentials.process)?;
            self.u32(credentials.user)?;
            self.u32(credentials.group)?;
        }
        self.queue(&value.ancillary)
    }
    fn pair(&mut self, value: &UnixPairSnapshot) -> Result<(), ()> {
        self.socket_type(value.socket_type)?;
        self.capacity(value.capacity)?;
        self.endpoint(&value.endpoints[0])?;
        self.endpoint(&value.endpoints[1])
    }
    pub(super) fn socket_state(&mut self, value: &NetworkSocketState) -> Result<(), ()> {
        match value {
            NetworkSocketState::Host {
                snapshot,
                resource,
                accepted,
            } => {
                self.u8(1)?;
                self.snapshot(snapshot)?;
                self.u64(resource.value())?;
                self.cardinality(accepted.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
                for item in accepted {
                    self.accepted(item)?;
                }
            }
            NetworkSocketState::UnixPair { endpoints, pair } => {
                self.u8(2)?;
                self.snapshot(&endpoints[0])?;
                self.snapshot(&endpoints[1])?;
                self.pair(pair)?;
            }
            NetworkSocketState::Unix {
                snapshot,
                pending,
                datagram,
            } => {
                self.u8(3)?;
                self.snapshot(snapshot)?;
                self.cardinality(pending.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
                for id in pending {
                    self.id(*id)?;
                }
                self.bool(datagram.is_some())?;
                if let Some(datagram) = datagram {
                    self.unix_datagram(datagram)?;
                }
            }
        }
        Ok(())
    }
}
