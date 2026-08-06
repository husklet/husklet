use std::sync::Arc;

use hl_descriptor::{DescriptorTable, OpenFileDescription, StatusFlags};
use hl_linux::{
    Errno, GuestAccess, GuestArchitecture, GuestMarshaller, GuestMemory, GuestNetworkAddress, LinuxResult, NetworkAbi,
};
use hl_network::{
    AddressFamily, BindRoute, EgressRoute, NetworkCatalog, SocketAddress, SocketConnectError, SocketConnectStatus,
    SocketDescription, SocketProtocol, SocketState, SocketType, UnixAddress, UnixSocketPair,
};

use crate::{DescriptorTransfer, RuntimeNetworkHost, RuntimeSocket, RuntimeSocketKind, RuntimeSocketRegistry};

use super::{SocketCredentials, errno::SocketErrno};

pub struct RuntimeNetworkSyscalls<H: RuntimeNetworkHost, M: GuestMemory> {
    pub(crate) descriptors: Arc<DescriptorTable>,
    pub(crate) catalog: Arc<NetworkCatalog>,
    pub(crate) checkpoint_catalog: Option<Arc<crate::CheckpointNetworkCatalog>>,
    pub(crate) memory: M,
    pub(crate) architecture: GuestArchitecture,
    pub(crate) host: Option<Arc<H>>,
    pub(crate) sockets: Arc<RuntimeSocketRegistry<H>>,
    pub(crate) credentials: Option<Arc<dyn SocketCredentials>>,
    pub(crate) wait: Option<Arc<dyn crate::SocketWait>>,
    pub(crate) transfer: Option<Arc<dyn DescriptorTransfer<H::Attachment>>>,
    pub(crate) unix_socket_paths: Option<Arc<dyn crate::UnixSocketPathPort>>,
    pub(crate) policy: Option<hl_network::NetworkPolicy>,
    pub(crate) host_projection: bool,
}

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    fn local_projection(address: &SocketAddress) -> bool {
        match address {
            SocketAddress::Inet4 { address, .. } => *address == [0; 4] || *address == [127, 0, 0, 1],
            SocketAddress::Inet6 { address, .. } => {
                address.iter().all(|byte| *byte == 0) || address[..15].iter().all(|byte| *byte == 0) && address[15] == 1
            }
            SocketAddress::Unix(_) => false,
        }
    }

    pub fn new(
        descriptors: Arc<DescriptorTable>,
        catalog: Arc<NetworkCatalog>,
        memory: M,
        architecture: GuestArchitecture,
    ) -> Self {
        Self {
            descriptors,
            catalog,
            checkpoint_catalog: None,
            memory,
            architecture,
            host: None,
            sockets: Arc::new(RuntimeSocketRegistry::default()),
            credentials: None,
            wait: None,
            transfer: None,
            unix_socket_paths: None,
            policy: None,
            host_projection: false,
        }
    }

    #[must_use]
    pub fn with_checkpoint_catalog(mut self, catalog: Arc<crate::CheckpointNetworkCatalog>) -> Self {
        self.catalog = catalog.current();
        self.checkpoint_catalog = Some(catalog);
        self
    }

    pub(crate) fn current_catalog(&self) -> Arc<NetworkCatalog> {
        self.checkpoint_catalog
            .as_ref()
            .map_or_else(|| self.catalog.clone(), |catalog| catalog.current())
    }

    #[must_use]
    pub fn with_host(mut self, host: Arc<H>) -> Self {
        self.host = Some(host);
        self
    }

    #[must_use]
    pub fn with_registry(mut self, sockets: Arc<RuntimeSocketRegistry<H>>) -> Self {
        self.sockets = sockets;
        self
    }

    #[must_use]
    pub fn with_credentials(mut self, credentials: hl_network::SenderCredentials) -> Self {
        self.credentials = Some(Arc::new(credentials));
        self
    }

    #[must_use]
    pub fn with_credential_source(mut self, credentials: Arc<dyn SocketCredentials>) -> Self {
        self.credentials = Some(credentials);
        self
    }

    #[must_use]
    pub fn with_wait_port(mut self, wait: Arc<dyn crate::SocketWait>) -> Self {
        self.wait = Some(wait);
        self
    }

    #[must_use]
    pub fn with_descriptor_transfer(mut self, transfer: Arc<dyn DescriptorTransfer<H::Attachment>>) -> Self {
        self.transfer = Some(transfer);
        self
    }

    #[must_use]
    pub fn with_unix_socket_paths(mut self, paths: Arc<dyn crate::UnixSocketPathPort>) -> Self {
        self.unix_socket_paths = Some(paths);
        self
    }

    #[must_use]
    pub fn with_network_policy(mut self, policy: hl_network::NetworkPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    #[must_use]
    pub fn with_host_projection(mut self, projected: bool) -> Self {
        self.host_projection = projected;
        self
    }

    pub(crate) fn route(&self, address: &SocketAddress) -> Result<(), Errno> {
        match self.policy.as_ref().map(|policy| policy.route(address)) {
            Some(hl_network::RouteDisposition::NetworkUnreachable) => Err(Errno::ENETUNREACH),
            Some(hl_network::RouteDisposition::Host) | None => Ok(()),
        }
    }

    pub(crate) fn bind_route(&self, address: SocketAddress) -> BindRoute {
        match &self.policy {
            Some(policy) => policy.bind_route(address),
            None => BindRoute {
                address,
                interface: None,
                aliases: Vec::new(),
            },
        }
    }

    pub(crate) fn connect_route(&self, address: SocketAddress) -> EgressRoute {
        match &self.policy {
            Some(policy) => policy.connect_route(address),
            None => EgressRoute {
                address,
                interface: None,
            },
        }
    }

    fn socket(&self, domain: i32, raw_type: u32, protocol: i32) -> LinuxResult {
        let result = self.socket_result(domain, raw_type, protocol);
        hl_log::hl_debug!(
            hl_log::tag::NET,
            "socket domain={domain} type={raw_type:#x} protocol={protocol} result={:#x}",
            result.encode(),
        );
        result
    }

    fn socket_result(&self, domain: i32, raw_type: u32, protocol: i32) -> LinuxResult {
        if domain == 17 || (matches!(domain, 2 | 10) && raw_type & 0xf == 3) {
            return LinuxResult::Error(Errno::EPERM);
        }
        if domain == 16 {
            if raw_type & !(0xf | 0x800 | 0x8_0000) != 0 || !matches!(raw_type & 0xf, 2 | 3) {
                return LinuxResult::Error(Errno::EINVAL);
            }
            if protocol != 0 {
                return LinuxResult::Error(Errno::EPROTONOSUPPORT);
            }
            let (status, _) = Self::descriptor_flags(raw_type);
            let policy = self.policy.clone().unwrap_or_else(|| {
                hl_network::NetworkPolicy::from_launch(false, b"", b"", b"").expect("default network policy")
            });
            let port = self
                .credentials
                .as_ref()
                .and_then(|credentials| credentials.current())
                .map_or(1, |value| value.process);
            let route = crate::network::netlink::RouteSocket::new(policy.namespace_interfaces(), port);
            let snapshot = Self::snapshot(
                AddressFamily::Unix,
                SocketType::Datagram,
                SocketProtocol::Default,
                status,
                None,
                None,
            );
            return self.install_one(
                RuntimeSocket::netlink(route, snapshot, self.current_catalog()),
                raw_type,
            );
        }
        let (family, socket_type, protocol, status, local) = match Self::socket_parameters(domain, raw_type, protocol) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if family == AddressFamily::Unix {
            let mut snapshot = Self::snapshot(family, socket_type, protocol, status, local, None);
            let catalog = self.current_catalog();
            let Ok(id) = catalog.insert_unix(snapshot.clone()) else { return LinuxResult::Error(Errno::ENFILE) };
            snapshot.id = id;
            return self.install_one(RuntimeSocket::unix_standalone(id, snapshot, catalog), raw_type);
        }
        let Some(host) = &self.host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let created = match host.create(family, socket_type, protocol) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::runtime(error)),
        };
        let description = Arc::new(SocketDescription::new(host.clone(), created.token, status));
        description.bind_readiness();
        let snapshot = Self::snapshot(family, socket_type, protocol, status, local, None);
        let catalog = self.current_catalog();
        let Ok(id) = catalog.insert_host(snapshot.clone(), created.resource, created.binding, Vec::new()) else {
            description.close();
            return LinuxResult::Error(Errno::ENFILE);
        };
        let mut snapshot = snapshot;
        snapshot.id = id;
        let object = RuntimeSocket::host(description, created.token, id, snapshot, catalog);
        self.install_one(object, raw_type)
    }

    fn socketpair(&self, domain: i32, raw_type: u32, protocol: i32, output: u64) -> LinuxResult {
        let (family, socket_type, protocol, status, local) = match Self::socket_parameters(domain, raw_type, protocol) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if family != AddressFamily::Unix || protocol != SocketProtocol::Default {
            return LinuxResult::Error(Errno::EOPNOTSUPP);
        }
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        match marshaller.probe(output, 8, GuestAccess::Write) {
            Ok(8) => {}
            _ => return LinuxResult::Error(Errno::EFAULT),
        }
        let pair = match UnixSocketPair::new(socket_type, status) {
            Ok(value) => Arc::new(value),
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        if let Some(credentials) = self.credentials.as_ref().and_then(|source| source.current()) {
            pair.set_peer_credentials(credentials);
        }
        let peer = Some(SocketAddress::Unix(Vec::new()));
        let mut snapshots = [
            Self::snapshot(family, socket_type, protocol, status, local.clone(), peer.clone()),
            Self::snapshot(family, socket_type, protocol, status, local, peer),
        ];
        let catalog = self.current_catalog();
        let Ok(ids) = catalog.insert_unix_pair(snapshots.clone(), pair.clone()) else {
            return LinuxResult::Error(Errno::ENFILE)
        };
        snapshots[0].id = ids[0];
        snapshots[1].id = ids[1];
        let objects = RuntimeSocket::unix_pair(pair, ids, snapshots, catalog);
        self.install_pair(objects, raw_type, output, &marshaller)
    }

    fn install_one(&self, object: Arc<RuntimeSocket<H>>, raw_type: u32) -> LinuxResult {
        let (status, local) = Self::descriptor_flags(raw_type);
        let install = match self.descriptors.prepare_open(0, object.clone(), status, local) {
            Ok(value) => value,
            Err(error) => {
                object.close();
                return LinuxResult::Error(crate::filesystem::FileErrno::descriptor(error));
            }
        };
        if self.sockets.register(install.description_identity(), object).is_err() {
            return LinuxResult::Error(Errno::ENFILE);
        }
        LinuxResult::Value(install.publish() as u64)
    }

    pub(crate) fn lookup(&self, descriptor: i32) -> Result<Arc<RuntimeSocket<H>>, Errno> {
        let lease = self.descriptors.pin(descriptor).map_err(|_| Errno::EBADF)?;
        self.sockets.get(lease.description_identity()).ok_or(Errno::ENOTSOCK)
    }

    fn bind(&self, descriptor: i32, pointer: u64, length: u32) -> LinuxResult {
        let result = self.bind_result(descriptor, pointer, length);
        hl_log::hl_debug!(
            hl_log::tag::NET,
            "bind descriptor={descriptor} address={pointer:#x} length={length} result={:#x}",
            result.encode(),
        );
        result
    }

    fn bind_result(&self, descriptor: i32, pointer: u64, length: u32) -> LinuxResult {
        if let Ok(socket) = self.lookup(descriptor) {
            if socket.netlink_socket().is_some() {
                return if length >= 12 {
                    LinuxResult::Value(0)
                } else {
                    LinuxResult::Error(Errno::EINVAL)
                };
            }
        }
        let mut address = match NetworkAbi::new(&self.memory, self.architecture)
            .decode_sockaddr(pointer, length)
            .and_then(Self::host_address)
        {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if matches!(&socket.kind, RuntimeSocketKind::UnixStandalone { .. }) {
            let SocketAddress::Unix(raw) = address else {
                return LinuxResult::Error(Errno::EINVAL);
            };
            let mut prepared_path = if !raw.is_empty() && raw[0] != 0 {
                if let Some(paths) = &self.unix_socket_paths {
                    let Ok(pathname) = hl_vfs::GuestPathBytes::new(&raw) else {
                        return LinuxResult::Error(Errno::EINVAL)
                    };
                    match paths.prepare_bind(&pathname) {
                        Ok(prepared) => Some(prepared),
                        Err(error) => return LinuxResult::Error(error),
                    }
                } else {
                    None
                }
            } else {
                None
            };
            let requested = if raw.is_empty() {
                UnixAddress::Unnamed
            } else if raw[0] == 0 {
                UnixAddress::Abstract(raw[1..].to_vec())
            } else {
                UnixAddress::Pathname(raw)
            };
            let bound = match socket.bind_unix(self.sockets.unix_namespace(), requested) {
                Ok(value) => value,
                Err(hl_network::UnixNamespaceError::AddressInUse) => {
                    if let Some(prepared) = prepared_path.take() {
                        prepared.rollback();
                    }
                    return LinuxResult::Error(Errno::EADDRINUSE);
                }
                Err(hl_network::UnixNamespaceError::Invalid) => {
                    if let Some(prepared) = prepared_path.take() {
                        prepared.rollback();
                    }
                    return LinuxResult::Error(Errno::EINVAL);
                }
                Err(hl_network::UnixNamespaceError::Exhausted) => {
                    if let Some(prepared) = prepared_path.take() {
                        prepared.rollback();
                    }
                    return LinuxResult::Error(Errno::ENOSPC);
                }
            };
            if let Some(named) = socket.named_unix() {
                if named.bind(bound.clone()).is_err() {
                    socket.rollback_unix_bind();
                    if let Some(prepared) = prepared_path.take() {
                        prepared.rollback();
                    }
                    return LinuxResult::Error(Errno::EINVAL);
                }
            }
            let local = match bound {
                UnixAddress::Unnamed => SocketAddress::Unix(Vec::new()),
                UnixAddress::Pathname(value) => SocketAddress::Unix(value),
                UnixAddress::Abstract(value) => SocketAddress::Unix([vec![0], value].concat()),
            };
            let mut snapshot = socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            snapshot.local = Some(local);
            snapshot.state = SocketState::Bound;
            return match self.current_catalog().replace_snapshot(socket.id, snapshot.clone()) {
                Ok(()) => {
                    if let Some(prepared) = prepared_path.take() {
                        prepared.commit();
                    }
                    LinuxResult::Value(0)
                }
                Err(_) => {
                    socket.rollback_unix_bind();
                    if let Some(prepared) = prepared_path.take() {
                        prepared.rollback();
                    }
                    LinuxResult::Error(Errno::EIO)
                }
            };
        }
        let RuntimeSocketKind::Host { token, .. } = &socket.kind else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let Some(host) = &self.host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        if !self.host_projection && !Self::local_projection(&address) {
            let mut snapshot = socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let family = snapshot.family;
            let mut port = match &address {
                SocketAddress::Inet4 { port, .. } | SocketAddress::Inet6 { port, .. } => *port,
                SocketAddress::Unix(_) => return LinuxResult::Error(Errno::EINVAL),
            };
            if port == 0 {
                port = 32_768_u16.saturating_add(socket.id.slot);
                match &mut address {
                    SocketAddress::Inet4 { port: current, .. } | SocketAddress::Inet6 { port: current, .. } => {
                        *current = port;
                    }
                    SocketAddress::Unix(_) => unreachable!(),
                }
            }
            snapshot.local = Some(address);
            snapshot.state = SocketState::Bound;
            let catalog = self.current_catalog();
            let Ok(prepared) = catalog.prepare_host_bind(
                snapshot.clone(),
                hl_network::PortCheckpoint {
                    family,
                    port,
                    owner: socket.id,
                },
            )
            else {
                return LinuxResult::Error(Errno::EADDRINUSE)
            };
            if prepared.commit().is_err() {
                return LinuxResult::Error(Errno::EADDRINUSE);
            }
            return LinuxResult::Value(0);
        }
        let local = match host.bind_route(*token, self.bind_route(address)) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::runtime(error)),
        };
        let mut snapshot = socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.local = Some(local);
        snapshot.state = SocketState::Bound;
        if self
            .current_catalog()
            .replace_host_snapshot(socket.id, snapshot.clone())
            .is_err()
        {
            return LinuxResult::Error(Errno::EIO);
        }
        LinuxResult::Value(0)
    }

    fn listen(&self, descriptor: i32, backlog: i32) -> LinuxResult {
        let result = self.listen_result(descriptor, backlog);
        hl_log::hl_debug!(
            hl_log::tag::NET,
            "listen descriptor={descriptor} backlog={backlog} result={:#x}",
            result.encode(),
        );
        result
    }

    fn listen_result(&self, descriptor: i32, backlog: i32) -> LinuxResult {
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if let RuntimeSocketKind::UnixStandalone { .. } = &socket.kind {
            let Some(named) = socket.named_unix() else {
                return LinuxResult::Error(Errno::EOPNOTSUPP);
            };
            let backlog = backlog.max(0) as u32;
            if named.listen(backlog as usize).is_err() {
                return LinuxResult::Error(Errno::EINVAL);
            }
            let mut snapshot = socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            snapshot.state = SocketState::Listening {
                backlog: backlog.max(1),
            };
            return match self.current_catalog().replace_snapshot(socket.id, snapshot.clone()) {
                Ok(()) => LinuxResult::Value(0),
                Err(_) => LinuxResult::Error(Errno::EIO),
            };
        }
        let RuntimeSocketKind::Host { description, token } = &socket.kind else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let Some(host) = &self.host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let backlog = backlog.max(0) as u32;
        let local_projection = socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .local
            .as_ref()
            .is_some_and(|address| {
                Self::local_projection(address)
                    || self
                        .policy
                        .as_ref()
                        .is_some_and(|policy| policy.bind_route(address.clone()).interface.is_some())
            });
        if !self.host_projection && !local_projection {
            let mut snapshot = socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if snapshot.state != SocketState::Bound {
                return LinuxResult::Error(Errno::EINVAL);
            }
            snapshot.state = SocketState::Listening { backlog };
            return match self
                .current_catalog()
                .replace_host_snapshot(socket.id, snapshot.clone())
            {
                Ok(()) => LinuxResult::Value(0),
                Err(_) => LinuxResult::Error(Errno::EIO),
            };
        }
        if let Err(error) = host.listen(*token, backlog) {
            return LinuxResult::Error(SocketErrno::runtime(error));
        }
        description.listen(backlog as usize);
        let mut snapshot = socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.state = SocketState::Listening { backlog };
        if self
            .current_catalog()
            .replace_host_snapshot(socket.id, snapshot.clone())
            .is_err()
        {
            return LinuxResult::Error(Errno::EIO);
        }
        LinuxResult::Value(0)
    }

    fn connect(&self, descriptor: i32, pointer: u64, length: u32) -> LinuxResult {
        let result = self.connect_result(descriptor, pointer, length);
        hl_log::hl_debug!(
            hl_log::tag::NET,
            "connect descriptor={descriptor} address={pointer:#x} length={length} result={:#x}",
            result.encode(),
        );
        result
    }

    fn connect_result(&self, descriptor: i32, pointer: u64, length: u32) -> LinuxResult {
        let guest_address = match NetworkAbi::new(&self.memory, self.architecture).decode_sockaddr(pointer, length) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        {
            let mut snapshot = socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(snapshot.state, SocketState::Listening { .. }) {
                return LinuxResult::Error(Errno::EISCONN);
            }
            if matches!(guest_address, GuestNetworkAddress::Unspecified) && snapshot.socket_type == SocketType::Datagram
            {
                snapshot.peer = None;
                snapshot.state = if snapshot.local.is_some() {
                    SocketState::Bound
                } else {
                    SocketState::Created
                };
                return match self
                    .current_catalog()
                    .replace_host_snapshot(socket.id, snapshot.clone())
                {
                    Ok(()) => LinuxResult::Value(0),
                    Err(_) => LinuxResult::Error(Errno::EIO),
                };
            }
        }
        let address = match Self::host_address(guest_address) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        if let RuntimeSocketKind::UnixStandalone { .. } = &socket.kind {
            let SocketAddress::Unix(raw) = address else {
                return LinuxResult::Error(Errno::EINVAL);
            };
            let target = if raw.is_empty() {
                UnixAddress::Unnamed
            } else if raw[0] == 0 {
                UnixAddress::Abstract(raw[1..].to_vec())
            } else {
                UnixAddress::Pathname(raw)
            };
            let Some(listener_id) = self.sockets.unix_namespace().resolve(&target) else {
                return LinuxResult::Error(Errno::ECONNREFUSED);
            };
            let Some(listener) = self.sockets.get_id(listener_id) else {
                return LinuxResult::Error(Errno::ECONNREFUSED);
            };
            if let Some(datagram) = socket.unix_datagram() {
                if listener.unix_datagram().is_none() || datagram.connect(target).is_err() {
                    return LinuxResult::Error(Errno::ECONNREFUSED);
                }
                let mut snapshot = socket
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                snapshot.peer = listener
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .local
                    .clone();
                snapshot.state = SocketState::Connected;
                return match self.current_catalog().replace_snapshot(socket.id, snapshot.clone()) {
                    Ok(()) => LinuxResult::Value(0),
                    Err(_) => LinuxResult::Error(Errno::EIO),
                };
            }
            let (Some(client_named), Some(listener_named)) = (socket.named_unix(), listener.named_unix()) else {
                return LinuxResult::Error(Errno::ECONNREFUSED);
            };
            let current = socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let listener_snapshot = listener
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let nonblocking = current.nonblocking;
            let queue = listener_named.wait_queue();
            let reservation = loop {
                let observed = queue.observation();
                match client_named.reserve_connect(&listener_named, true) {
                    Ok(value) => break value,
                    Err(hl_network::UnixNamedSocketError::WouldBlock) if !nonblocking => {}
                    Err(hl_network::UnixNamedSocketError::WouldBlock) => return LinuxResult::Error(Errno::EAGAIN),
                    Err(_) => return LinuxResult::Error(Errno::ECONNREFUSED),
                }
                let Some(wait) = &self.wait else {
                    return LinuxResult::Error(Errno::EAGAIN);
                };
                match wait.wait(&queue, observed, None) {
                    Ok(hl_sync::WaitOutcome::Notified) => {}
                    Ok(hl_sync::WaitOutcome::Interrupted) => return LinuxResult::Error(Errno::EINTR),
                    Ok(hl_sync::WaitOutcome::TimedOut) | Err(_) => return LinuxResult::Error(Errno::EIO),
                }
            };
            let flags = StatusFlags::from_bits(if nonblocking { StatusFlags::NONBLOCKING } else { 0 });
            let pair = match UnixSocketPair::new(current.socket_type, flags) {
                Ok(value) => Arc::new(value),
                Err(_) => return LinuxResult::Error(Errno::EPROTONOSUPPORT),
            };
            if let Some(credentials) = self.credentials.as_ref().and_then(|source| source.current()) {
                pair.set_peer_credentials(credentials);
            }
            let client_local = current.local.clone().or(Some(SocketAddress::Unix(Vec::new())));
            let mut client_snapshot = current.clone();
            client_snapshot.local = client_local.clone();
            client_snapshot.peer = listener_snapshot.local.clone();
            client_snapshot.state = SocketState::Connected;
            let mut accepted_snapshot = Self::snapshot(
                current.family,
                current.socket_type,
                current.protocol,
                flags,
                listener_snapshot.local.clone(),
                client_local,
            );
            accepted_snapshot.state = SocketState::Connected;
            let catalog = self.current_catalog();
            let Ok(accepted_id) = catalog.connect_unix_pair(
                listener.id,
                socket.id,
                client_snapshot.clone(),
                accepted_snapshot.clone(),
                pair.clone(),
            )
            else {
                return LinuxResult::Error(Errno::EIO)
            };
            accepted_snapshot.id = accepted_id;
            let lifetime = RuntimeSocket::<H>::pair_lifetime(catalog, socket.id);
            socket.attach_unix_connection(pair.clone(), 0, lifetime.clone());
            *socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = client_snapshot;
            listener.queue_pending(
                accepted_id,
                RuntimeSocket::connected_unix(pair.clone(), 1, accepted_id, accepted_snapshot, lifetime),
            );
            if reservation.commit(pair).is_err() {
                return LinuxResult::Error(Errno::EIO);
            }
            return LinuxResult::Value(0);
        }
        let RuntimeSocketKind::Host { description, token } = &socket.kind else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let Some(host) = &self.host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        if socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .socket_type
            == SocketType::Datagram
        {
            if let Err(error) = host.prepare_connect_route(*token, self.connect_route(address.clone())) {
                return LinuxResult::Error(SocketErrno::runtime(error));
            }
            match hl_network::SocketHostIo::start_connect(host.as_ref(), *token, false) {
                SocketConnectStatus::Connected => {
                    let mut snapshot = socket
                        .snapshot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    snapshot.peer = Some(address);
                    snapshot.state = SocketState::Connected;
                    return match self
                        .current_catalog()
                        .replace_host_snapshot(socket.id, snapshot.clone())
                    {
                        Ok(()) => LinuxResult::Value(0),
                        Err(_) => LinuxResult::Error(Errno::EIO),
                    };
                }
                SocketConnectStatus::Failed(error) => return LinuxResult::Error(Self::connect_errno(error)),
                _ => return LinuxResult::Error(Errno::EIO),
            }
        }
        if let Err(error) = self.route(&address) {
            return LinuxResult::Error(error);
        }
        if let Err(error) = host.prepare_connect_route(*token, self.connect_route(address.clone())) {
            return LinuxResult::Error(SocketErrno::runtime(error));
        }
        {
            let mut snapshot = socket
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            snapshot.peer = Some(address);
            snapshot.state = SocketState::Connecting;
            if self
                .current_catalog()
                .replace_host_snapshot(socket.id, snapshot.clone())
                .is_err()
            {
                return LinuxResult::Error(Errno::EIO);
            }
        }
        let blocking = !socket
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .nonblocking;
        let connected = if blocking {
            if let Some(wait) = &self.wait {
                let cancellation = crate::network::wait::SocketCancellation::new(wait.interruption());
                description.connect_with_cancellation(&cancellation)
            } else {
                description.connect()
            }
        } else {
            description.connect()
        };
        if socket.connect_status().is_err() {
            return LinuxResult::Error(Errno::EIO);
        }
        match connected {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(Self::connect_errno(error)),
        }
    }

    fn address(&self, descriptor: i32, pointer: u64, length: u64, peer: bool) -> LinuxResult {
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if let Some(netlink) = socket.netlink_socket() {
            if peer {
                return LinuxResult::Error(Errno::ENOTCONN);
            }
            let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
            let mut capacity = [0_u8; 4];
            if marshaller.copy_from(length, &mut capacity).fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
            let capacity = u32::from_le_bytes(capacity) as usize;
            let mut address = [0_u8; 12];
            address[..2].copy_from_slice(&16_u16.to_le_bytes());
            address[4..8].copy_from_slice(&netlink.port().to_le_bytes());
            if marshaller
                .copy_to(pointer, &address[..capacity.min(12)])
                .fault
                .is_some()
                || marshaller.copy_to(length, &12_u32.to_le_bytes()).fault.is_some()
            {
                return LinuxResult::Error(Errno::EFAULT);
            }
            return LinuxResult::Value(0);
        }
        let address = match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => {
                let snapshot = socket
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if peer && snapshot.socket_type == SocketType::Datagram {
                    snapshot
                        .peer
                        .clone()
                        .map(GuestNetworkAddress::Inet)
                        .ok_or(crate::RuntimeNetworkError::NotConnected)
                } else {
                    drop(snapshot);
                    let Some(host) = &self.host else {
                        return LinuxResult::Error(Errno::ENOSYS);
                    };
                    if peer {
                        host.peer_address(*token)
                    } else {
                        host.local_address(*token)
                    }
                    .map(GuestNetworkAddress::Inet)
                }
            }
            RuntimeSocketKind::Unix { pair, endpoint } => {
                Ok(GuestNetworkAddress::Unix(pair.endpoints[*endpoint].address().clone()))
            }
            RuntimeSocketKind::UnixStandalone { .. } => {
                let snapshot = socket
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let address = if peer {
                    snapshot.peer.as_ref()
                } else {
                    snapshot.local.as_ref()
                };
                match address {
                    Some(SocketAddress::Unix(value)) => Ok(Self::guest_address(&SocketAddress::Unix(value.clone()))),
                    _ if peer => return LinuxResult::Error(Errno::ENOTCONN),
                    _ => Ok(GuestNetworkAddress::Unix(UnixAddress::Unnamed)),
                }
            }
        };
        let address = match address {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::runtime(error)),
        };
        let abi = NetworkAbi::new(&self.memory, self.architecture);
        let staged = match abi.prepare_sockaddr_copyout(pointer, length, &address) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(SocketErrno::marshal(error)),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(SocketErrno::marshal(error)),
        }
    }

    fn shutdown(&self, descriptor: i32, how: i32) -> LinuxResult {
        let (read, write) = match how {
            0 => (true, false),
            1 => (false, true),
            2 => (true, true),
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        let socket = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let result = match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => self
                .host
                .as_ref()
                .ok_or(crate::RuntimeNetworkError::Unsupported)
                .and_then(|host| host.shutdown(*token, read, write)),
            RuntimeSocketKind::Unix { pair, endpoint } => {
                pair.endpoints[*endpoint].shutdown(read, write);
                Ok(())
            }
            RuntimeSocketKind::UnixStandalone { .. } => match socket.standalone_connection() {
                Some((pair, endpoint)) => {
                    pair.endpoints[endpoint].shutdown(read, write);
                    Ok(())
                }
                None => Err(crate::RuntimeNetworkError::NotConnected),
            },
        };
        match result {
            Ok(()) => {
                let mut snapshot = socket
                    .snapshot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                snapshot.shutdown.read |= read;
                snapshot.shutdown.write |= write;
                if self
                    .current_catalog()
                    .replace_snapshot(socket.id, snapshot.clone())
                    .is_err()
                {
                    return LinuxResult::Error(Errno::EIO);
                }
                LinuxResult::Value(0)
            }
            Err(error) => LinuxResult::Error(SocketErrno::runtime(error)),
        }
    }

    pub(crate) fn host_address(address: GuestNetworkAddress) -> Result<SocketAddress, hl_linux::NetworkMarshalError> {
        match address {
            GuestNetworkAddress::Unspecified => Err(hl_linux::NetworkMarshalError::InvalidFamily),
            GuestNetworkAddress::Inet(value) => Ok(value),
            GuestNetworkAddress::Unix(UnixAddress::Pathname(value)) => Ok(SocketAddress::Unix(value)),
            GuestNetworkAddress::Unix(UnixAddress::Abstract(value)) => {
                let mut address = vec![0];
                address.extend(value);
                Ok(SocketAddress::Unix(address))
            }
            GuestNetworkAddress::Unix(UnixAddress::Unnamed) => Ok(SocketAddress::Unix(Vec::new())),
            GuestNetworkAddress::Netlink { .. } => Err(hl_linux::NetworkMarshalError::InvalidFamily),
        }
    }

    pub(crate) fn connect_errno(error: SocketConnectError) -> Errno {
        match error {
            SocketConnectError::InProgress => Errno::EINPROGRESS,
            SocketConnectError::Already => Errno::EALREADY,
            SocketConnectError::Connected => Errno::EISCONN,
            SocketConnectError::Interrupted => Errno::EINTR,
            SocketConnectError::Refused => Errno::ECONNREFUSED,
            SocketConnectError::TimedOut => Errno::ETIMEDOUT,
            SocketConnectError::Canceled => Errno::EINTR,
            SocketConnectError::Io => Errno::EIO,
        }
    }

    fn install_pair(
        &self,
        objects: [Arc<RuntimeSocket<H>>; 2],
        raw_type: u32,
        output: u64,
        marshaller: &GuestMarshaller<'_, M>,
    ) -> LinuxResult {
        let (status, local) = Self::descriptor_flags(raw_type);
        let installs = vec![
            (objects[0].clone() as Arc<dyn OpenFileDescription>, status, local),
            (objects[1].clone() as Arc<dyn OpenFileDescription>, status, local),
        ];
        let batch = match self.descriptors.prepare_open_batch(0, installs) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(crate::filesystem::FileErrno::descriptor(error)),
        };
        let identities = batch.description_identities();
        if self.sockets.register(identities[0], objects[0].clone()).is_err()
            || self.sockets.register(identities[1], objects[1].clone()).is_err()
        {
            objects[0].close();
            objects[1].close();
            return LinuxResult::Error(Errno::ENFILE);
        }
        let selected = batch.numbers();
        let numbers = [selected[0], selected[1]];
        let bytes = [numbers[0].to_le_bytes(), numbers[1].to_le_bytes()].concat();
        if marshaller.copy_to(output, &bytes).fault.is_some() {
            objects[0].close();
            objects[1].close();
            return LinuxResult::Error(Errno::EFAULT);
        }
        let published = batch.publish_all();
        debug_assert_eq!(published, numbers);
        LinuxResult::Value(0)
    }
}

#[path = "dispatch.rs"]
mod dispatch;

#[cfg(test)]
#[path = "syscalls_test.rs"]
mod tests;
