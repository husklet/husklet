use std::sync::Arc;

use hl_descriptor::{DescriptorTable, OpenFileDescription};
use hl_linux::{Errno, GuestAccess, GuestArchitecture, GuestMarshaller, GuestMemory, GuestNetworkAddress, LinuxResult};
use hl_network::{
    AddressFamily, BindRoute, EgressRoute, NetworkCatalog, SocketAddress, SocketConnectError, SocketDescription,
    SocketProtocol, SocketType, UnixAddress, UnixSocketPair,
};

use crate::{DescriptorTransfer, RuntimeNetworkHost, RuntimeSocket, RuntimeSocketRegistry};

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
    pub(crate) fn local_projection(address: &SocketAddress) -> bool {
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
            let Ok(id) = catalog.insert_unix(snapshot.clone()) else {
                return LinuxResult::Error(Errno::ENFILE);
            };
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
            return LinuxResult::Error(Errno::ENFILE);
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
