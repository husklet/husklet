use hl_network::{
    AuthoritySocketKey, AuthoritySocketLease, NETWORK_CHECKPOINT_SOCKET_MAXIMUM, NetworkCheckpointImage,
    NetworkResourceKey,
};

use super::NetworkCheckpointCodec;

mod decoder;
mod encoder;
use decoder::Input;
use encoder::Output;

const MAGIC: u64 = 0x314b_574e_4c48_4e48;
const WIRE_VERSION: u32 = 4;
pub const NETWORK_CHECKPOINT_BYTES_MAXIMUM: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub struct PortableNetworkCodec;

impl PortableNetworkCodec {
    fn checksum(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |digest, byte| {
            (digest ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3)
        })
    }
}

impl NetworkCheckpointCodec for PortableNetworkCodec {
    fn encode(&self, image: &NetworkCheckpointImage) -> Result<Vec<u8>, ()> {
        image.validate().map_err(|_| ())?;
        let mut output = Output::default();
        output.u64(MAGIC)?;
        output.u32(WIRE_VERSION)?;
        output.u32(image.version)?;
        output.cardinality(image.generations.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        output.cardinality(image.ports.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        output.cardinality(image.authority.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        output.cardinality(image.sockets.len(), NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        output.configuration(&image.configuration)?;
        for generation in &image.generations {
            output.u64(*generation)?;
        }
        for port in &image.ports {
            output.port(port)?;
        }
        for lease in &image.authority {
            output.u64(lease.resource.value())?;
            output.u32(lease.key.slot())?;
            output.u64(lease.key.generation())?;
        }
        for socket in &image.sockets {
            output.socket_state(socket)?;
        }
        let checksum = Self::checksum(&output.bytes);
        output.u64(checksum)?;
        Ok(output.bytes)
    }

    fn decode(&self, bytes: &[u8]) -> Result<NetworkCheckpointImage, ()> {
        if bytes.len() > NETWORK_CHECKPOINT_BYTES_MAXIMUM {
            return Err(());
        }
        let payload_end = bytes.len().checked_sub(8).ok_or(())?;
        let expected = u64::from_le_bytes(bytes[payload_end..].try_into().map_err(|_| ())?);
        let payload = &bytes[..payload_end];
        if Self::checksum(payload) != expected {
            return Err(());
        }
        let mut input = Input {
            bytes: payload,
            offset: 0,
        };
        if input.u64()? != MAGIC || input.u32()? != WIRE_VERSION {
            return Err(());
        }
        let version = input.u32()?;
        let generation_count = input.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        let port_count = input.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        let authority_count = input.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        let socket_count = input.cardinality(NETWORK_CHECKPOINT_SOCKET_MAXIMUM)?;
        let configuration = input.configuration()?;
        let mut generations = Vec::with_capacity(generation_count);
        for _ in 0..generation_count {
            generations.push(input.u64()?);
        }
        let mut ports = Vec::with_capacity(port_count);
        for _ in 0..port_count {
            ports.push(input.port()?);
        }
        let mut authority = Vec::with_capacity(authority_count);
        for _ in 0..authority_count {
            authority.push(AuthoritySocketLease {
                resource: NetworkResourceKey::new(input.u64()?).ok_or(())?,
                key: AuthoritySocketKey::new(input.u32()?, input.u64()?).ok_or(())?,
            });
        }
        let mut sockets = Vec::with_capacity(socket_count);
        for _ in 0..socket_count {
            sockets.push(input.socket_state()?);
        }
        if input.offset != payload.len() {
            return Err(());
        }
        let image = NetworkCheckpointImage {
            version,
            generations,
            configuration,
            ports,
            authority,
            sockets,
        };
        image.validate().map_err(|_| ())?;
        Ok(image)
    }
}

#[cfg(test)]
mod tests {
    use hl_network::{
        AddressFamily, ControlMessage, NETWORK_CHECKPOINT_VERSION, NetworkCheckpointImage, NetworkConfiguration,
        NetworkResourceKey, NetworkSocketState, QueueMessageSnapshot, QueueRightsSnapshot, QueueSnapshot,
        ShutdownState, SocketAddress, SocketId, SocketProtocol, SocketSnapshot, SocketState, SocketType, UnixAddress,
        UnixEndpointSnapshot, UnixPairSnapshot,
    };

    use super::{Input, NetworkCheckpointCodec, Output, PortableNetworkCodec};

    fn socket(id: SocketId, state: SocketState) -> SocketSnapshot {
        let (local, peer) = match state {
            SocketState::Created => (None, None),
            SocketState::Listening { .. } => (
                Some(SocketAddress::Inet4 {
                    address: [127, 0, 0, 1],
                    port: 8080,
                }),
                None,
            ),
            _ => (
                Some(SocketAddress::Unix(Vec::new())),
                Some(SocketAddress::Unix(Vec::new())),
            ),
        };
        SocketSnapshot {
            id,
            family: if matches!(state, SocketState::Listening { .. }) {
                AddressFamily::Inet4
            } else {
                AddressFamily::Unix
            },
            socket_type: SocketType::Stream,
            protocol: if matches!(state, SocketState::Listening { .. }) {
                SocketProtocol::Tcp
            } else {
                SocketProtocol::Default
            },
            state,
            local,
            peer,
            connect_error: None,
            nonblocking: true,
            shutdown: ShutdownState::default(),
        }
    }

    fn image() -> NetworkCheckpointImage {
        let first = SocketId { slot: 1, generation: 3 };
        let second = SocketId { slot: 2, generation: 2 };
        let listener = SocketId { slot: 3, generation: 7 };
        let standalone = SocketId { slot: 4, generation: 5 };
        let queue = QueueSnapshot {
            messages: vec![QueueMessageSnapshot {
                payload: b"queued".to_vec(),
                controls: vec![ControlMessage::Rights(Vec::new())],
                rights: vec![QueueRightsSnapshot { identities: vec![91] }],
                credentials: Some(hl_network::SenderCredentials {
                    process: 17,
                    user: 23,
                    group: 29,
                }),
                automatic: true,
            }],
        };
        let endpoint = |write_shutdown| UnixEndpointSnapshot {
            address: UnixAddress::Unnamed,
            incoming: vec![b"bytes".to_vec()],
            peer_write_shutdown: false,
            read_shutdown: false,
            write_shutdown,
            closed: false,
            passcred: write_shutdown,
            peer_credentials: Some(hl_network::SenderCredentials {
                process: 31,
                user: 37,
                group: 41,
            }),
            ancillary: queue.clone(),
        };
        NetworkCheckpointImage {
            version: NETWORK_CHECKPOINT_VERSION,
            generations: vec![3, 2, 7, 5],
            configuration: NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
            ports: Vec::new(),
            authority: Vec::new(),
            sockets: vec![
                NetworkSocketState::UnixPair {
                    endpoints: [
                        socket(first, SocketState::Connected),
                        socket(second, SocketState::Connected),
                    ],
                    pair: UnixPairSnapshot {
                        socket_type: SocketType::Stream,
                        capacity: 65_536,
                        endpoints: [endpoint(false), endpoint(true)],
                    },
                },
                NetworkSocketState::Host {
                    snapshot: socket(listener, SocketState::Listening { backlog: 32 }),
                    resource: NetworkResourceKey::new(41).unwrap(),
                    accepted: Vec::new(),
                },
                NetworkSocketState::Unix {
                    snapshot: socket(standalone, SocketState::Created),
                    pending: Vec::new(),
                    datagram: None,
                },
            ],
        }
    }

    #[test]
    fn wire_roundtrip() {
        let codec = PortableNetworkCodec;
        let image = image();
        let bytes = codec.encode(&image).unwrap();
        assert_eq!(codec.decode(&bytes).unwrap(), image);
    }

    mod limits {
        use super::{Input, Output};

        #[test]
        fn capacity() {
            let mut output = Output::default();
            output.capacity(65_536).unwrap();
            let mut input = Input {
                bytes: &output.bytes,
                offset: 0,
            };
            assert_eq!(input.capacity().unwrap(), 65_536);
            assert_eq!(input.offset, output.bytes.len());
        }

        #[test]
        fn cardinality() {
            let maximum = hl_network::NETWORK_CHECKPOINT_SOCKET_MAXIMUM;
            let mut output = Output::default();
            output.cardinality(maximum, maximum).unwrap();
            assert!(output.cardinality(maximum + 1, maximum).is_err());

            let accepted = u32::try_from(maximum).unwrap().to_le_bytes();
            let rejected = u32::try_from(maximum + 1).unwrap().to_le_bytes();
            assert_eq!(
                Input {
                    bytes: &accepted,
                    offset: 0
                }
                .cardinality(maximum)
                .unwrap(),
                maximum
            );
            assert!(
                Input {
                    bytes: &rejected,
                    offset: 0
                }
                .cardinality(maximum)
                .is_err()
            );
        }
    }

    #[test]
    fn corruption_rejected() {
        let codec = PortableNetworkCodec;
        let bytes = codec.encode(&image()).unwrap();
        for index in 0..bytes.len() {
            let mut damaged = bytes.clone();
            damaged[index] ^= 0x80;
            assert!(codec.decode(&damaged).is_err(), "accepted byte {index}");
        }
    }

    #[test]
    fn order_rejected() {
        let codec = PortableNetworkCodec;
        let mut image = image();
        image.sockets.swap(0, 1);
        assert!(codec.encode(&image).is_err());
    }

    #[test]
    fn size_rejected() {
        let codec = PortableNetworkCodec;
        assert!(
            codec
                .decode(&vec![0; super::NETWORK_CHECKPOINT_BYTES_MAXIMUM + 1])
                .is_err()
        );
    }
}
