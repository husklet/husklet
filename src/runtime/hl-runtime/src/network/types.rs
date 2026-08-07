use hl_descriptor::{DescriptorFlags, StatusFlags};
use hl_linux::{Errno, GuestMemory, GuestNetworkAddress};
use hl_network::{AddressFamily, SocketAddress, SocketId, SocketProtocol, SocketSnapshot, SocketState, SocketType};

use crate::{RuntimeNetworkHost, RuntimeNetworkSyscalls};

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn snapshot(
        family: AddressFamily,
        socket_type: SocketType,
        protocol: SocketProtocol,
        status: StatusFlags,
        local: Option<SocketAddress>,
        peer: Option<SocketAddress>,
    ) -> SocketSnapshot {
        SocketSnapshot {
            id: SocketId { slot: 1, generation: 1 },
            family,
            socket_type,
            protocol,
            state: if peer.is_some() {
                SocketState::Connected
            } else {
                SocketState::Created
            },
            local,
            peer,
            connect_error: None,
            nonblocking: status.bits() & StatusFlags::NONBLOCKING != 0,
            shutdown: hl_network::ShutdownState::default(),
        }
    }

    pub(crate) fn socket_parameters(
        domain: i32,
        raw_type: u32,
        protocol: i32,
    ) -> Result<
        (
            AddressFamily,
            SocketType,
            SocketProtocol,
            StatusFlags,
            Option<SocketAddress>,
        ),
        Errno,
    > {
        if raw_type & !(0xf | 0x800 | 0x8_0000) != 0 {
            return Err(Errno::EINVAL);
        }
        let family = match domain {
            1 => AddressFamily::Unix,
            2 => AddressFamily::Inet4,
            10 => AddressFamily::Inet6,
            _ => return Err(Errno::EAFNOSUPPORT),
        };
        let socket_type = match raw_type & 0xf {
            1 => SocketType::Stream,
            2 => SocketType::Datagram,
            3 => SocketType::Raw,
            5 => SocketType::SequencePacket,
            _ => return Err(Errno::EINVAL),
        };
        let protocol = match protocol {
            0 => SocketProtocol::Default,
            1 => SocketProtocol::Icmp,
            6 => SocketProtocol::Tcp,
            17 => SocketProtocol::Udp,
            _ => return Err(Errno::EPROTONOSUPPORT),
        };
        let (status, _) = Self::descriptor_flags(raw_type);
        Ok((family, socket_type, protocol, status, None))
    }

    pub(crate) fn descriptor_flags(raw_type: u32) -> (StatusFlags, DescriptorFlags) {
        (
            StatusFlags::from_bits(
                2 | if raw_type & 0x800 != 0 {
                    StatusFlags::NONBLOCKING
                } else {
                    0
                },
            ),
            DescriptorFlags::from_bits(if raw_type & 0x8_0000 != 0 {
                DescriptorFlags::CLOSE_ON_EXEC
            } else {
                0
            }),
        )
    }

    pub(crate) fn guest_address(address: &SocketAddress) -> GuestNetworkAddress {
        match address {
            SocketAddress::Unix(value) if value.is_empty() => {
                GuestNetworkAddress::Unix(hl_network::UnixAddress::Unnamed)
            }
            SocketAddress::Unix(value) if value[0] == 0 => {
                GuestNetworkAddress::Unix(hl_network::UnixAddress::Abstract(value[1..].to_vec()))
            }
            SocketAddress::Unix(value) => GuestNetworkAddress::Unix(hl_network::UnixAddress::Pathname(value.clone())),
            value => GuestNetworkAddress::Inet(value.clone()),
        }
    }
}
