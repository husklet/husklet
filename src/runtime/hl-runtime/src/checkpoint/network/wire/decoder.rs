use hl_network::{
    AcceptedSocketCheckpoint, AddressFamily, ControlMessage, NETWORK_CHECKPOINT_SOCKET_MAXIMUM, NetworkConfiguration,
    NetworkResourceKey, NetworkSocketState, PortCheckpoint, QueueMessageSnapshot, QueueRightsSnapshot, QueueSnapshot,
    Route, SenderCredentials, ShutdownState, SocketAddress, SocketId, SocketProtocol, SocketSnapshot, SocketState,
    SocketType, UnixAddress, UnixDatagramRecordSnapshot, UnixDatagramSnapshot, UnixEndpointSnapshot, UnixPairSnapshot,
};

use super::NETWORK_CHECKPOINT_BYTES_MAXIMUM;

pub(super) struct Input<'a> {
    pub(super) bytes: &'a [u8],
    pub(super) offset: usize,
}

impl Input<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], ()> {
        let end = self.offset.checked_add(count).ok_or(())?;
        let value = self.bytes.get(self.offset..end).ok_or(())?;
        self.offset = end;
        Ok(value)
    }
    fn u8(&mut self) -> Result<u8, ()> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool, ()> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(()),
        }
    }
    fn i32(&mut self) -> Result<i32, ()> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }
    fn u16(&mut self) -> Result<u16, ()> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().map_err(|_| ())?))
    }
    pub(super) fn u32(&mut self) -> Result<u32, ()> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }
    pub(super) fn u64(&mut self) -> Result<u64, ()> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().map_err(|_| ())?))
    }
    pub(super) fn cardinality(&mut self, maximum: usize) -> Result<usize, ()> {
        let value = usize::try_from(self.u32()?).map_err(|_| ())?;
        (value <= maximum).then_some(value).ok_or(())
    }
    fn length(&mut self) -> Result<usize, ()> {
        let value = usize::try_from(self.u32()?).map_err(|_| ())?;
        (value <= NETWORK_CHECKPOINT_BYTES_MAXIMUM).then_some(value).ok_or(())
    }
    pub(super) fn capacity(&mut self) -> Result<usize, ()> {
        usize::try_from(self.u64()?).map_err(|_| ())
    }
    fn bytes(&mut self) -> Result<Vec<u8>, ()> {
        let count = self.length()?;
        Ok(self.take(count)?.to_vec())
    }
    fn text(&mut self) -> Result<String, ()> {
        String::from_utf8(self.bytes()?).map_err(|_| ())
    }
    fn family(&mut self) -> Result<AddressFamily, ()> {
        match self.u8()? {
            1 => Ok(AddressFamily::Unix),
            2 => Ok(AddressFamily::Inet4),
            3 => Ok(AddressFamily::Inet6),
            _ => Err(()),
        }
    }
    fn socket_type(&mut self) -> Result<SocketType, ()> {
        match self.u8()? {
            1 => Ok(SocketType::Stream),
            2 => Ok(SocketType::Datagram),
            3 => Ok(SocketType::SequencePacket),
            4 => Ok(SocketType::Raw),
            _ => Err(()),
        }
    }
    fn protocol(&mut self) -> Result<SocketProtocol, ()> {
        match self.u8()? {
            1 => Ok(SocketProtocol::Default),
            2 => Ok(SocketProtocol::Tcp),
            3 => Ok(SocketProtocol::Udp),
            4 => Ok(SocketProtocol::Icmp),
            _ => Err(()),
        }
    }
    fn id(&mut self) -> Result<SocketId, ()> {
        Ok(SocketId {
            slot: self.u16()?,
            generation: self.u64()?,
        })
    }
    fn address(&mut self) -> Result<SocketAddress, ()> {
        match self.u8()? {
            1 => Ok(SocketAddress::Unix(self.bytes()?)),
            2 => Ok(SocketAddress::Inet4 {
                address: self.take(4)?.try_into().map_err(|_| ())?,
                port: self.u16()?,
            }),
            3 => Ok(SocketAddress::Inet6 {
                address: self.take(16)?.try_into().map_err(|_| ())?,
                port: self.u16()?,
                scope: self.u32()?,
            }),
            _ => Err(()),
        }
    }
    fn optional_address(&mut self) -> Result<Option<SocketAddress>, ()> {
        if self.bool()? {
            Ok(Some(self.address()?))
        } else {
            Ok(None)
        }
    }
    fn unix_address(&mut self) -> Result<UnixAddress, ()> {
        match self.u8()? {
            0 => Ok(UnixAddress::Unnamed),
            1 => Ok(UnixAddress::Pathname(self.bytes()?)),
            2 => Ok(UnixAddress::Abstract(self.bytes()?)),
            _ => Err(()),
        }
    }
    fn unix_datagram(&mut self) -> Result<UnixDatagramSnapshot, ()> {
        let capacity = self.capacity()?;
        let connected = if self.bool()? { Some(self.unix_address()?) } else { None };
        let count = self.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            records.push(UnixDatagramRecordSnapshot {
                payload: self.bytes()?,
                source: self.unix_address()?,
            });
        }
        Ok(UnixDatagramSnapshot {
            capacity,
            connected,
            records,
            closed: self.bool()?,
        })
    }
    fn connect_error(&mut self) -> Result<Option<hl_network::SocketConnectError>, ()> {
        Ok(match self.u8()? {
            0 => None,
            1 => Some(hl_network::SocketConnectError::InProgress),
            2 => Some(hl_network::SocketConnectError::Already),
            3 => Some(hl_network::SocketConnectError::Connected),
            4 => Some(hl_network::SocketConnectError::Interrupted),
            5 => Some(hl_network::SocketConnectError::Refused),
            6 => Some(hl_network::SocketConnectError::TimedOut),
            7 => Some(hl_network::SocketConnectError::Canceled),
            8 => Some(hl_network::SocketConnectError::Io),
            _ => return Err(()),
        })
    }
    fn snapshot(&mut self) -> Result<SocketSnapshot, ()> {
        let id = self.id()?;
        let family = self.family()?;
        let socket_type = self.socket_type()?;
        let protocol = self.protocol()?;
        let state = match self.u8()? {
            1 => SocketState::Created,
            2 => SocketState::Bound,
            3 => SocketState::Listening { backlog: self.u32()? },
            4 => SocketState::Connecting,
            5 => SocketState::Connected,
            6 => SocketState::Closed,
            _ => return Err(()),
        };
        Ok(SocketSnapshot {
            id,
            family,
            socket_type,
            protocol,
            state,
            local: self.optional_address()?,
            peer: self.optional_address()?,
            connect_error: self.connect_error()?,
            nonblocking: self.bool()?,
            shutdown: ShutdownState {
                read: self.bool()?,
                write: self.bool()?,
            },
        })
    }
    fn route(&mut self) -> Result<Route, ()> {
        let family = self.family()?;
        let destination = self.take(16)?.try_into().map_err(|_| ())?;
        let prefix_bits = self.u8()?;
        let gateway = if self.bool()? {
            Some(self.take(16)?.try_into().map_err(|_| ())?)
        } else {
            None
        };
        Ok(Route {
            family,
            destination,
            prefix_bits,
            gateway,
            interface: self.u32()?,
            metric: self.u32()?,
        })
    }
    pub(super) fn configuration(&mut self) -> Result<NetworkConfiguration, ()> {
        let routes = self.cardinality(256)?;
        let dns = self.cardinality(8)?;
        let domains = self.cardinality(16)?;
        let mut route_values = Vec::with_capacity(routes);
        for _ in 0..routes {
            route_values.push(self.route()?);
        }
        let mut dns_values = Vec::with_capacity(dns);
        for _ in 0..dns {
            dns_values.push(self.address()?);
        }
        let mut domain_values = Vec::with_capacity(domains);
        for _ in 0..domains {
            domain_values.push(self.text()?);
        }
        NetworkConfiguration::new(route_values, dns_values, domain_values).map_err(|_| ())
    }
    pub(super) fn port(&mut self) -> Result<PortCheckpoint, ()> {
        Ok(PortCheckpoint {
            family: self.family()?,
            port: self.u16()?,
            owner: self.id()?,
        })
    }
    fn accepted(&mut self) -> Result<AcceptedSocketCheckpoint, ()> {
        Ok(AcceptedSocketCheckpoint {
            resource: NetworkResourceKey::new(self.u64()?).ok_or(())?,
            local: self.address()?,
            peer: self.address()?,
        })
    }
    fn control(&mut self) -> Result<ControlMessage, ()> {
        match self.u8()? {
            1 => {
                let count = self.cardinality(253)?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.i32()?);
                }
                Ok(ControlMessage::Rights(values))
            }
            2 => Ok(ControlMessage::Credentials {
                process: self.u32()?,
                user: self.u32()?,
                group: self.u32()?,
            }),
            3 => Ok(ControlMessage::Unknown {
                level: self.i32()?,
                kind: self.i32()?,
                data: self.bytes()?,
            }),
            _ => Err(()),
        }
    }
    fn queue(&mut self) -> Result<QueueSnapshot, ()> {
        let count = self.cardinality(1024)?;
        let mut messages = Vec::with_capacity(count);
        for _ in 0..count {
            messages.push(self.queue_message()?);
        }
        Ok(QueueSnapshot { messages })
    }
    fn queue_message(&mut self) -> Result<QueueMessageSnapshot, ()> {
        let payload = self.bytes()?;
        let credentials = self.credentials()?;
        let automatic = self.bool()?;
        let controls = self.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        let mut control_values = Vec::with_capacity(controls);
        for _ in 0..controls {
            control_values.push(self.control()?);
        }
        let rights = self.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        let mut right_values = Vec::with_capacity(rights);
        for _ in 0..rights {
            right_values.push(self.rights()?);
        }
        Ok(QueueMessageSnapshot {
            payload,
            controls: control_values,
            rights: right_values,
            credentials,
            automatic,
        })
    }
    fn credentials(&mut self) -> Result<Option<SenderCredentials>, ()> {
        if !self.bool()? {
            return Ok(None);
        }
        Ok(Some(SenderCredentials {
            process: self.u32()?,
            user: self.u32()?,
            group: self.u32()?,
        }))
    }
    fn rights(&mut self) -> Result<QueueRightsSnapshot, ()> {
        let count = self.cardinality(253)?;
        let mut identities = Vec::with_capacity(count);
        for _ in 0..count {
            identities.push(self.u64()?);
        }
        Ok(QueueRightsSnapshot { identities })
    }
    fn endpoint(&mut self) -> Result<UnixEndpointSnapshot, ()> {
        let address = match self.u8()? {
            1 => hl_network::UnixAddress::Unnamed,
            2 => hl_network::UnixAddress::Pathname(self.bytes()?),
            3 => hl_network::UnixAddress::Abstract(self.bytes()?),
            _ => return Err(()),
        };
        let count = self.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        let mut incoming = Vec::with_capacity(count);
        for _ in 0..count {
            incoming.push(self.bytes()?);
        }
        let peer_write_shutdown = self.bool()?;
        let read_shutdown = self.bool()?;
        let write_shutdown = self.bool()?;
        let closed = self.bool()?;
        let passcred = self.bool()?;
        let peer_credentials = self.credentials()?;
        Ok(UnixEndpointSnapshot {
            address,
            incoming,
            peer_write_shutdown,
            read_shutdown,
            write_shutdown,
            closed,
            passcred,
            peer_credentials,
            ancillary: self.queue()?,
        })
    }
    fn pair(&mut self) -> Result<UnixPairSnapshot, ()> {
        Ok(UnixPairSnapshot {
            socket_type: self.socket_type()?,
            capacity: self.capacity()?,
            endpoints: [self.endpoint()?, self.endpoint()?],
        })
    }
    pub(super) fn socket_state(&mut self) -> Result<NetworkSocketState, ()> {
        match self.u8()? {
            1 => {
                let snapshot = self.snapshot()?;
                let resource = NetworkResourceKey::new(self.u64()?).ok_or(())?;
                let count = self.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
                let mut accepted = Vec::with_capacity(count);
                for _ in 0..count {
                    accepted.push(self.accepted()?);
                }
                Ok(NetworkSocketState::Host {
                    snapshot,
                    resource,
                    accepted,
                })
            }
            2 => Ok(NetworkSocketState::UnixPair {
                endpoints: [self.snapshot()?, self.snapshot()?],
                pair: self.pair()?,
            }),
            3 => {
                let snapshot = self.snapshot()?;
                let count = self.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
                let mut pending = Vec::with_capacity(count);
                for _ in 0..count {
                    pending.push(self.id()?);
                }
                let datagram = if self.bool()? {
                    Some(self.unix_datagram()?)
                } else {
                    None
                };
                Ok(NetworkSocketState::Unix {
                    snapshot,
                    pending,
                    datagram,
                })
            }
            _ => Err(()),
        }
    }
}
