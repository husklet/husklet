use std::sync::{Arc, Condvar, Mutex, OnceLock};

use hl_descriptor::{DescriptionRef, StatusFlags};

use crate::{
    SocketDescription, SocketType, UnixAddress, UnixEndpointSnapshot, UnixMessageQueue, UnixPairSnapshot,
    UnixTransportError,
};

use super::transport::{EndpointState, UNIX_BUFFER_DEFAULT, UnixSocketEndpoint, UnixSocketHost, UnixState};

pub struct SocketPair {
    pub endpoints: [UnixSocketEndpoint; 2],
    peer_credentials: [OnceLock<crate::SenderCredentials>; 2],
}
pub type UnixSocketPair = SocketPair;

impl UnixSocketPair {
    #[must_use]
    pub fn socket_type(&self) -> SocketType {
        self.endpoints[0].host.socket_type
    }

    pub fn new(socket_type: SocketType, flags: StatusFlags) -> Result<Self, UnixTransportError> {
        Self::with_capacity(socket_type, flags, UNIX_BUFFER_DEFAULT)
    }

    pub fn with_capacity(
        socket_type: SocketType,
        flags: StatusFlags,
        capacity: usize,
    ) -> Result<Self, UnixTransportError> {
        if !matches!(
            socket_type,
            SocketType::Stream | SocketType::Datagram | SocketType::SequencePacket
        ) || capacity == 0
        {
            return Err(UnixTransportError::Invalid);
        }
        let ancillary = [Arc::new(UnixMessageQueue::new()), Arc::new(UnixMessageQueue::new())];
        let host = Arc::new(UnixSocketHost {
            socket_type,
            capacity,
            state: Mutex::new(UnixState {
                endpoints: [EndpointState::new(), EndpointState::new()],
            }),
            wake: Condvar::new(),
            message_wait: std::array::from_fn(|token| ancillary[token].wait_handle()),
            readiness: Mutex::new(std::array::from_fn(|_| None)),
        });
        let endpoints = std::array::from_fn(|token| UnixSocketEndpoint {
            host: host.clone(),
            token,
            address: UnixAddress::Unnamed,
            ancillary: ancillary.clone(),
            description: Arc::new(SocketDescription::new(host.clone(), token, flags)),
        });
        for endpoint in &endpoints {
            endpoint.description.bind_readiness();
        }
        Ok(Self {
            endpoints,
            peer_credentials: std::array::from_fn(|_| OnceLock::new()),
        })
    }

    /// Installs the immutable Linux peer identity sampled at pair creation.
    pub fn set_peer_credentials(&self, credentials: crate::SenderCredentials) {
        for peer in &self.peer_credentials {
            let _ = peer.set(credentials);
        }
    }

    #[must_use]
    pub fn peer_credentials(&self, endpoint: usize) -> Option<crate::SenderCredentials> {
        self.peer_credentials.get(endpoint)?.get().copied()
    }

    #[must_use]
    pub fn snapshot(&self) -> UnixPairSnapshot {
        let state = self.endpoints[0]
            .host
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        UnixPairSnapshot {
            socket_type: self.endpoints[0].host.socket_type,
            capacity: self.endpoints[0].host.capacity,
            endpoints: std::array::from_fn(|token| UnixEndpointSnapshot {
                address: self.endpoints[token].address.clone(),
                incoming: state.endpoints[token].incoming.iter().cloned().collect(),
                peer_write_shutdown: state.endpoints[token].peer_write_shutdown,
                read_shutdown: state.endpoints[token].read_shutdown,
                write_shutdown: state.endpoints[token].write_shutdown,
                closed: state.endpoints[token].closed,
                passcred: self.endpoints[token].passcred(),
                peer_credentials: self.peer_credentials(token),
                ancillary: self.endpoints[token].ancillary[token].snapshot(),
            }),
        }
    }

    pub fn restore<F>(
        snapshot: &UnixPairSnapshot,
        flags: StatusFlags,
        mut rebind: F,
    ) -> Result<Self, UnixTransportError>
    where
        F: FnMut(u64) -> Option<DescriptionRef>,
    {
        snapshot.validate()?;
        let queues = [
            Arc::new(
                UnixMessageQueue::restore(&snapshot.endpoints[0].ancillary, |identity| rebind(identity))
                    .map_err(UnixTransportError::Control)?,
            ),
            Arc::new(
                UnixMessageQueue::restore(&snapshot.endpoints[1].ancillary, |identity| rebind(identity))
                    .map_err(UnixTransportError::Control)?,
            ),
        ];
        for token in 0..2 {
            queues[token].set_passcred(snapshot.endpoints[token].passcred);
        }
        let endpoint_states = std::array::from_fn(|token| {
            let saved = &snapshot.endpoints[token];
            let bytes = saved.incoming.iter().map(Vec::len).sum();
            let ancillary_bytes = saved
                .ancillary
                .messages
                .iter()
                .map(|message| message.payload.len())
                .sum();
            EndpointState {
                incoming: saved.incoming.iter().cloned().collect(),
                bytes,
                ancillary_bytes,
                peer_write_shutdown: saved.peer_write_shutdown,
                read_shutdown: saved.read_shutdown,
                write_shutdown: saved.write_shutdown,
                closed: saved.closed,
                canceled: false,
            }
        });
        let host = Arc::new(UnixSocketHost {
            socket_type: snapshot.socket_type,
            capacity: snapshot.capacity,
            state: Mutex::new(UnixState {
                endpoints: endpoint_states,
            }),
            wake: Condvar::new(),
            message_wait: std::array::from_fn(|token| queues[token].wait_handle()),
            readiness: Mutex::new(std::array::from_fn(|_| None)),
        });
        let endpoints = std::array::from_fn(|token| UnixSocketEndpoint {
            host: host.clone(),
            token,
            address: snapshot.endpoints[token].address.clone(),
            ancillary: queues.clone(),
            description: Arc::new(SocketDescription::new(host.clone(), token, flags)),
        });
        for endpoint in &endpoints {
            endpoint.description.bind_readiness();
        }
        Ok(Self {
            endpoints,
            peer_credentials: std::array::from_fn(|token| {
                let peer = OnceLock::new();
                if let Some(credentials) = snapshot.endpoints[token].peer_credentials {
                    let _ = peer.set(credentials);
                }
                peer
            }),
        })
    }
}
