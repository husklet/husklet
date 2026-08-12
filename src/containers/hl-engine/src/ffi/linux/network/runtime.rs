use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptorFlags, StatusFlags};
use hl_network::{
    AddressFamily, NetworkCheckpointError, ShutdownState, SocketAddress, SocketProtocol, SocketSnapshot, SocketState,
    SocketType,
};
use hl_runtime::{
    CheckpointDescriptorTable, CheckpointNetworkCatalog, NetworkCheckpointParticipant, NetworkObjectBindings,
    PortableNetworkCodec, RuntimeSocketRegistry,
};

use super::Native;
use super::transfer::NativeTransfer;
use crate::ffi::linux::execution::path::FileTransferRegistry;

/// Process-owned composition of live network routing and checkpoint authority.
pub struct CheckpointRuntime {
    host: Arc<Native>,
    sockets: Arc<RuntimeSocketRegistry<Native>>,
    bindings: Arc<NetworkObjectBindings<Native>>,
    catalog: Arc<CheckpointNetworkCatalog>,
    files: Arc<FileTransferRegistry>,
    policy: hl_network::NetworkPolicy,
}

impl CheckpointRuntime {
    pub fn new(
        catalog: Arc<CheckpointNetworkCatalog>,
        descriptors: Arc<CheckpointDescriptorTable>,
        authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
        policy: hl_network::NetworkPolicy,
    ) -> Arc<Self> {
        let host = Arc::new(match authority {
            Some(authority) => Native::authorized(authority),
            None => Native::new(),
        });
        let sockets = Arc::new(RuntimeSocketRegistry::default());
        let bindings = Arc::new(NetworkObjectBindings::new(
            descriptors,
            Arc::clone(&sockets),
            Some(Arc::clone(&host)),
        ));
        let files = Arc::new(FileTransferRegistry::default());
        Arc::new(Self {
            host,
            sockets,
            bindings,
            catalog,
            files,
            policy,
        })
    }

    /// Records the launch's published port rules on the live network host.
    pub fn publish(&self, records: &[u8]) {
        self.host
            .set_publications(super::native::publish::Publication::parse(records));
    }

    pub(super) fn host(&self) -> Arc<Native> {
        Arc::clone(&self.host)
    }

    pub(super) fn sockets(&self) -> Arc<RuntimeSocketRegistry<Native>> {
        Arc::clone(&self.sockets)
    }

    pub(super) fn policy(&self) -> hl_network::NetworkPolicy {
        self.policy.clone()
    }

    pub(in crate::ffi::linux) fn unix_namespace(&self) -> Arc<hl_network::UnixNamespace> {
        self.sockets.unix_namespace()
    }

    pub(in crate::ffi::linux) fn bindings(&self) -> Arc<NetworkObjectBindings<Native>> {
        Arc::clone(&self.bindings)
    }

    pub(super) fn catalog(&self) -> Arc<CheckpointNetworkCatalog> {
        Arc::clone(&self.catalog)
    }

    pub(in crate::ffi::linux) fn files(&self) -> Arc<FileTransferRegistry> {
        Arc::clone(&self.files)
    }

    pub(in crate::ffi::linux) fn socket_ioctl(&self) -> Arc<hl_runtime::SocketIoctl<Native>> {
        Arc::new(
            hl_runtime::SocketIoctl::new(Arc::clone(&self.host), Arc::clone(&self.sockets))
                .with_policy(self.policy.clone()),
        )
    }

    pub(super) fn transfer(&self) -> Arc<NativeTransfer> {
        Arc::new(NativeTransfer::new(
            Arc::clone(&self.host),
            Arc::clone(&self.sockets),
            self.catalog.current(),
            Arc::clone(&self.files),
        ))
    }

    pub fn adopt_listener(
        &self,
        listener: TcpListener,
        snapshot: SocketSnapshot,
        status: StatusFlags,
        flags: DescriptorFlags,
    ) -> Result<i32, NetworkCheckpointError> {
        if !Self::valid_listener(&listener, &snapshot, status) {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let created = self.host.adopt_listener(listener)?;
        self.bindings
            .publish_host(self.catalog.current(), created, snapshot, status, flags)
    }

    #[must_use]
    pub fn participant(&self) -> Arc<NetworkCheckpointParticipant> {
        Arc::new(NetworkCheckpointParticipant::new(
            Arc::clone(&self.catalog),
            self.bindings.clone(),
            Arc::new(PortableNetworkCodec),
        ))
    }

    #[must_use]
    pub fn descriptor_binding(&self) -> Arc<dyn hl_descriptor::DescriptorObjectCheckpoint> {
        self.bindings.clone()
    }

    pub fn listener_address(&self, descriptor: i32) -> Result<SocketAddress, NetworkCheckpointError> {
        self.bindings.host_local_address(descriptor)
    }

    fn valid_listener(listener: &TcpListener, snapshot: &SocketSnapshot, status: StatusFlags) -> bool {
        let Ok(local) = listener.local_addr() else { return false };
        let address_matches = match (&snapshot.local, local) {
            (Some(SocketAddress::Inet4 { address, port }), SocketAddr::V4(local)) => {
                *address == local.ip().octets() && *port == local.port() && *port != 0
            }
            (Some(SocketAddress::Inet6 { address, port, scope }), SocketAddr::V6(local)) => {
                *address == local.ip().octets() && *port == local.port() && *scope == local.scope_id() && *port != 0
            }
            _ => false,
        };
        let family_matches = matches!(
            (snapshot.family, local),
            (AddressFamily::Inet4, SocketAddr::V4(_)) | (AddressFamily::Inet6, SocketAddr::V6(_))
        );
        address_matches
            && family_matches
            && snapshot.socket_type == SocketType::Stream
            && snapshot.protocol == SocketProtocol::Tcp
            && matches!(snapshot.state, SocketState::Listening { .. })
            && snapshot.peer.is_none()
            && snapshot.connect_error.is_none()
            && snapshot.shutdown == ShutdownState::default()
            && snapshot.nonblocking == (status.bits() & StatusFlags::NONBLOCKING != 0)
    }
}

impl hl_runtime::ProcfsNetworkPort for CheckpointRuntime {
    fn view(&self) -> hl_runtime::ProcfsNetworkView {
        let view = self.catalog.current().namespace_view();
        hl_runtime::ProcfsNetworkView {
            generation: view.generation,
            internet: view
                .internet
                .into_iter()
                .map(|socket| {
                    let (ipv6, local, local_port) = match socket.local {
                        Some(hl_network::SocketAddress::Inet4 { address, port }) => {
                            let mut bytes = [0; 16];
                            bytes[..4].copy_from_slice(&address);
                            (false, bytes, port)
                        }
                        Some(hl_network::SocketAddress::Inet6 { address, port, .. }) => (true, address, port),
                        _ => (socket.family == AddressFamily::Inet6, [0; 16], 0),
                    };
                    let (remote, remote_port) = match socket.peer {
                        Some(hl_network::SocketAddress::Inet4 { address, port }) => {
                            let mut bytes = [0; 16];
                            bytes[..4].copy_from_slice(&address);
                            (bytes, port)
                        }
                        Some(hl_network::SocketAddress::Inet6 { address, port, .. }) => (address, port),
                        _ => ([0; 16], 0),
                    };
                    hl_runtime::ProcfsInternetSocketView {
                        ipv6,
                        udp: socket.socket_type == SocketType::Datagram,
                        local,
                        local_port,
                        remote,
                        remote_port,
                        state: match socket.state {
                            SocketState::Listening { .. } => 0x0a,
                            SocketState::Connected => 1,
                            _ => 7,
                        },
                        inode: socket.inode,
                    }
                })
                .collect(),
            interfaces: std::iter::once(hl_runtime::ProcfsNetworkInterfaceView {
                name: b"lo".to_vec(),
                index: 1,
                loopback: true,
                address: [0; 6],
                ipv4: None,
                prefix: 0,
                receive: [0; 8],
                transmit: [0; 8],
            })
            .chain(
                self.policy
                    .interfaces
                    .iter()
                    .map(|interface| hl_runtime::ProcfsNetworkInterfaceView {
                        name: interface.name.clone(),
                        index: interface.index,
                        loopback: false,
                        address: interface.mac,
                        ipv4: Some(interface.ipv4),
                        prefix: interface.prefix,
                        receive: [0; 8],
                        transmit: [0; 8],
                    }),
            )
            .collect(),
            unix: view
                .unix
                .into_iter()
                .map(|socket| hl_runtime::ProcfsUnixSocketView {
                    identity: socket.inode,
                    reference_count: 2,
                    protocol: 0,
                    flags: u32::from(matches!(socket.state, SocketState::Listening { .. })) * 0x0001_0000,
                    socket_type: match socket.socket_type {
                        SocketType::Stream => 1,
                        SocketType::Datagram => 2,
                        SocketType::SequencePacket => 5,
                        SocketType::Raw => 3,
                    },
                    state: match socket.state {
                        SocketState::Connected => 3,
                        _ => 1,
                    },
                    inode: socket.inode,
                    path: socket.path,
                })
                .collect(),
        }
    }
}
