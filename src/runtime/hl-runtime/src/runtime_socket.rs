use std::collections::BTreeMap;
use std::io::{IoSlice, IoSliceMut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::{
    DescriptionIdentity, DescriptionRef, ObjectError, ObjectKind, OpenFileDescription, Readiness, ReadinessObserver,
    ReadinessSubscription, StatusFlags,
};
use hl_network::{
    NetworkCatalog, SocketConnectStatus, SocketDescription, SocketHostIo, SocketId, SocketSnapshot, SocketState,
    UnixSocketPair,
};

pub(crate) struct CatalogLifetime {
    catalog: Arc<NetworkCatalog>,
    id: SocketId,
    remaining: AtomicUsize,
}

impl CatalogLifetime {
    fn release(&self) {
        if self.remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
            let _ = self.catalog.remove(self.id);
        }
    }
}

pub(crate) enum RuntimeSocketKind<H: SocketHostIo> {
    Host {
        description: Arc<SocketDescription<H>>,
        token: H::Token,
    },
    Unix {
        pair: Arc<UnixSocketPair>,
        endpoint: usize,
    },
    UnixStandalone {
        named: Option<Arc<hl_network::UnixNamedSocket>>,
        datagram: Option<Arc<hl_network::UnixDatagramSocket>>,
        connection: Mutex<Option<(Arc<UnixSocketPair>, usize)>>,
        pending: Mutex<BTreeMap<SocketId, Arc<RuntimeSocket<H>>>>,
    },
}

pub(crate) struct RuntimeSocket<H: SocketHostIo> {
    pub(crate) kind: RuntimeSocketKind<H>,
    pub(crate) id: SocketId,
    pub(crate) snapshot: Mutex<SocketSnapshot>,
    registry: Mutex<Option<(Weak<RuntimeSocketRegistry<H>>, DescriptionIdentity)>>,
    catalog: Mutex<Arc<CatalogLifetime>>,
    closed: AtomicBool,
    options: Mutex<BTreeMap<(i32, i32), hl_linux::GuestSocketOption>>,
    netlink: Option<Arc<crate::network::netlink::RouteSocket>>,
    unix_binding: Mutex<
        Option<(
            Arc<hl_network::UnixNamespace>,
            hl_network::UnixAddress,
            hl_network::UnixBinding,
        )>,
    >,
}

impl<H: SocketHostIo> std::fmt::Debug for RuntimeSocket<H> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("RuntimeSocket").field("id", &self.id).finish()
    }
}

impl<H: SocketHostIo> RuntimeSocket<H> {
    fn unregister(&self) {
        let Some((registry, identity)) = self
            .registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        if let Some(registry) = registry.upgrade() {
            registry.retire(identity);
        }
    }

    fn attach_registry(&self, registry: &Arc<RuntimeSocketRegistry<H>>, identity: DescriptionIdentity) {
        *self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((Arc::downgrade(registry), identity));
    }

    fn detach_registry(&self, identity: DescriptionIdentity) {
        let mut current = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.as_ref().is_some_and(|(_, value)| *value == identity) {
            current.take();
        }
    }

    pub(crate) fn read_with(&self, output: &mut [u8], nonblocking: bool) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.receive(output, false).map(|(count, _)| count);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.read_with(output, nonblocking),
            RuntimeSocketKind::Unix { pair, endpoint } => {
                pair.endpoints[*endpoint].description.read_with(output, nonblocking)
            }
            RuntimeSocketKind::UnixStandalone { .. } => self
                .standalone_connection()
                .ok_or(ObjectError::NotSupported)?
                .0
                .endpoints[self.standalone_connection().unwrap().1]
                .description
                .read_with(output, nonblocking),
        }
    }

    pub(crate) fn write_with(&self, input: &[u8], nonblocking: bool) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.send(input);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.write_with(input, nonblocking),
            RuntimeSocketKind::Unix { pair, endpoint } => {
                pair.endpoints[*endpoint].description.write_with(input, nonblocking)
            }
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint].description.write_with(input, nonblocking)
            }
        }
    }

    pub(crate) fn read_blocking(
        &self,
        output: &mut [u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.receive(output, false).map(|(count, _)| count);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.read_with_cancellation(output, cancellation),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint]
                .description
                .read_with_cancellation(output, cancellation),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint]
                    .description
                    .read_with_cancellation(output, cancellation)
            }
        }
    }

    pub(crate) fn write_blocking(
        &self,
        input: &[u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.send(input);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.write_with_cancellation(input, cancellation),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint]
                .description
                .write_with_cancellation(input, cancellation),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint]
                    .description
                    .write_with_cancellation(input, cancellation)
            }
        }
    }

    pub(crate) fn host(
        description: Arc<SocketDescription<H>>,
        token: H::Token,
        id: SocketId,
        snapshot: SocketSnapshot,
        catalog: Arc<NetworkCatalog>,
    ) -> Arc<Self> {
        Arc::new(Self {
            kind: RuntimeSocketKind::Host { description, token },
            id,
            snapshot: Mutex::new(snapshot),
            registry: Mutex::new(None),
            catalog: Mutex::new(Arc::new(CatalogLifetime {
                catalog,
                id,
                remaining: AtomicUsize::new(1),
            })),
            closed: AtomicBool::new(false),
            options: Mutex::new(BTreeMap::new()),
            netlink: None,
            unix_binding: Mutex::new(None),
        })
    }

    pub(crate) fn unix_pair(
        pair: Arc<UnixSocketPair>,
        ids: [SocketId; 2],
        snapshots: [SocketSnapshot; 2],
        catalog: Arc<NetworkCatalog>,
    ) -> [Arc<Self>; 2] {
        let lifetime = Arc::new(CatalogLifetime {
            catalog,
            id: ids[0],
            remaining: AtomicUsize::new(2),
        });
        std::array::from_fn(|endpoint| {
            Arc::new(Self {
                kind: RuntimeSocketKind::Unix {
                    pair: pair.clone(),
                    endpoint,
                },
                id: ids[endpoint],
                snapshot: Mutex::new(snapshots[endpoint].clone()),
                registry: Mutex::new(None),
                catalog: Mutex::new(lifetime.clone()),
                closed: AtomicBool::new(false),
                options: Mutex::new(BTreeMap::new()),
                netlink: None,
                unix_binding: Mutex::new(None),
            })
        })
    }

    pub(crate) fn unix_standalone(id: SocketId, snapshot: SocketSnapshot, catalog: Arc<NetworkCatalog>) -> Arc<Self> {
        let socket_type = snapshot.socket_type;
        let datagram = (socket_type == hl_network::SocketType::Datagram)
            .then(|| catalog.unix_datagram(id).ok())
            .flatten();
        Arc::new(Self {
            kind: RuntimeSocketKind::UnixStandalone {
                named: hl_network::UnixNamedSocket::new(snapshot.socket_type)
                    .ok()
                    .map(Arc::new),
                datagram,
                connection: Mutex::new(None),
                pending: Mutex::new(BTreeMap::new()),
            },
            id,
            snapshot: Mutex::new(snapshot),
            registry: Mutex::new(None),
            catalog: Mutex::new(Arc::new(CatalogLifetime {
                catalog,
                id,
                remaining: AtomicUsize::new(1),
            })),
            closed: AtomicBool::new(false),
            options: Mutex::new(BTreeMap::new()),
            netlink: None,
            unix_binding: Mutex::new(None),
        })
    }

    pub(crate) fn netlink(
        socket: Arc<crate::network::netlink::RouteSocket>,
        snapshot: SocketSnapshot,
        catalog: Arc<NetworkCatalog>,
    ) -> Arc<Self> {
        let id = SocketId { slot: 0, generation: 1 };
        Arc::new(Self {
            kind: RuntimeSocketKind::UnixStandalone {
                named: None,
                datagram: None,
                connection: Mutex::new(None),
                pending: Mutex::new(BTreeMap::new()),
            },
            id,
            snapshot: Mutex::new(snapshot),
            registry: Mutex::new(None),
            catalog: Mutex::new(Arc::new(CatalogLifetime {
                catalog,
                id,
                remaining: AtomicUsize::new(1),
            })),
            closed: AtomicBool::new(false),
            options: Mutex::new(BTreeMap::new()),
            netlink: Some(socket),
            unix_binding: Mutex::new(None),
        })
    }

    pub(crate) fn netlink_socket(&self) -> Option<&Arc<crate::network::netlink::RouteSocket>> {
        self.netlink.as_ref()
    }

    pub(crate) fn bind_unix(
        &self,
        namespace: Arc<hl_network::UnixNamespace>,
        requested: hl_network::UnixAddress,
    ) -> Result<hl_network::UnixAddress, hl_network::UnixNamespaceError> {
        let (address, binding) = namespace.bind(requested, self.id)?;
        *self
            .unix_binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((namespace, address.clone(), binding));
        Ok(address)
    }

    pub(crate) fn rollback_unix_bind(&self) {
        let Some((namespace, address, binding)) = self
            .unix_binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        match &address {
            hl_network::UnixAddress::Pathname(pathname) => {
                namespace.unlink_pathname(pathname, binding);
            }
            _ => namespace.release(&address, binding),
        }
    }

    pub(crate) fn standalone_connection(&self) -> Option<(Arc<UnixSocketPair>, usize)> {
        let RuntimeSocketKind::UnixStandalone { connection, .. } = &self.kind else {
            return None;
        };
        connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn named_unix(&self) -> Option<Arc<hl_network::UnixNamedSocket>> {
        let RuntimeSocketKind::UnixStandalone { named, .. } = &self.kind else {
            return None;
        };
        named.clone()
    }

    pub(crate) fn unix_datagram(&self) -> Option<Arc<hl_network::UnixDatagramSocket>> {
        let RuntimeSocketKind::UnixStandalone { datagram, .. } = &self.kind else {
            return None;
        };
        datagram.clone()
    }

    pub(crate) fn attach_unix_connection(
        &self,
        pair: Arc<UnixSocketPair>,
        endpoint: usize,
        lifetime: Arc<CatalogLifetime>,
    ) {
        if let RuntimeSocketKind::UnixStandalone { connection, .. } = &self.kind {
            *connection.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((pair, endpoint));
            *self.catalog.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = lifetime;
        }
    }

    pub(crate) fn connected_unix(
        pair: Arc<UnixSocketPair>,
        endpoint: usize,
        id: SocketId,
        snapshot: SocketSnapshot,
        lifetime: Arc<CatalogLifetime>,
    ) -> Arc<Self> {
        Arc::new(Self {
            kind: RuntimeSocketKind::UnixStandalone {
                named: None,
                datagram: None,
                connection: Mutex::new(Some((pair, endpoint))),
                pending: Mutex::new(BTreeMap::new()),
            },
            id,
            snapshot: Mutex::new(snapshot),
            registry: Mutex::new(None),
            catalog: Mutex::new(lifetime),
            closed: AtomicBool::new(false),
            options: Mutex::new(BTreeMap::new()),
            netlink: None,
            unix_binding: Mutex::new(None),
        })
    }

    pub(crate) fn pair_lifetime(catalog: Arc<NetworkCatalog>, id: SocketId) -> Arc<CatalogLifetime> {
        Arc::new(CatalogLifetime {
            catalog,
            id,
            remaining: AtomicUsize::new(2),
        })
    }

    pub(crate) fn queue_pending(&self, id: SocketId, socket: Arc<Self>) {
        if let RuntimeSocketKind::UnixStandalone { pending, .. } = &self.kind {
            pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id, socket);
        }
    }

    pub(crate) fn take_pending(&self, id: SocketId) -> Option<Arc<Self>> {
        let RuntimeSocketKind::UnixStandalone { pending, .. } = &self.kind else {
            return None;
        };
        pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id)
    }

    pub(crate) fn set_option(&self, level: i32, option: i32, value: hl_linux::GuestSocketOption) {
        self.options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((level, option), value);
    }

    pub(crate) fn option(&self, level: i32, option: i32) -> Option<hl_linux::GuestSocketOption> {
        self.options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(level, option))
            .cloned()
    }

    pub(crate) fn connect_status(&self) -> Result<SocketConnectStatus, ObjectError> {
        let description = match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description,
            RuntimeSocketKind::Unix { .. } => return Ok(SocketConnectStatus::Connected),
            RuntimeSocketKind::UnixStandalone { connection, .. } => {
                return Ok(
                    if connection
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_some()
                    {
                        SocketConnectStatus::Connected
                    } else {
                        SocketConnectStatus::Idle
                    },
                );
            }
        };
        let status = description.connect_status();
        let mut snapshot = self.snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match status {
            SocketConnectStatus::Idle if snapshot.connect_error.is_some() => {
                snapshot.state = if snapshot.local.is_some() {
                    SocketState::Bound
                } else {
                    SocketState::Created
                };
                snapshot.peer = None;
                snapshot.connect_error = None;
            }
            SocketConnectStatus::Idle => return Ok(status),
            SocketConnectStatus::Pending => {
                snapshot.state = SocketState::Connecting;
                snapshot.connect_error = None;
            }
            SocketConnectStatus::Connected => {
                snapshot.state = SocketState::Connected;
                snapshot.connect_error = None;
            }
            SocketConnectStatus::Failed(error) => {
                snapshot.state = SocketState::Connecting;
                snapshot.connect_error = Some(error);
            }
        }
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .replace_snapshot(self.id, snapshot.clone())
            .map_err(|_| ObjectError::Io)?;
        Ok(status)
    }
}

impl<H: SocketHostIo> OpenFileDescription for RuntimeSocket<H> {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Socket
    }
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.receive(output, false).map(|(count, _)| count);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.read(output),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.read(output),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint].description.read(output)
            }
        }
    }
    fn probe_read(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return Ok(netlink.ready().then_some(maximum));
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.probe_read(maximum),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.probe_read(maximum),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint].description.probe_read(maximum)
            }
        }
    }
    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.receive(output, false).map(|(count, _)| count);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.read_with_cancellation(output, cancellation),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint]
                .description
                .read_with_cancellation(output, cancellation),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint]
                    .description
                    .read_with_cancellation(output, cancellation)
            }
        }
    }
    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.send(input);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.write(input),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.write(input),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint].description.write(input)
            }
        }
    }
    fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.send(input);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.write_with_cancellation(input, cancellation),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint]
                .description
                .write_with_cancellation(input, cancellation),
            RuntimeSocketKind::UnixStandalone { .. } => {
                let (pair, endpoint) = self.standalone_connection().ok_or(ObjectError::NotSupported)?;
                pair.endpoints[endpoint]
                    .description
                    .write_with_cancellation(input, cancellation)
            }
        }
    }
    fn read_vector_context(
        &self,
        output: &mut [IoSliceMut<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        let capacity = output
            .iter()
            .try_fold(0_usize, |total, part| total.checked_add(part.len()))
            .ok_or(ObjectError::ResourceLimit)?;
        let mut buffer = vec![0_u8; capacity];
        let count = self.read_context(&mut buffer, context)?;
        let mut copied = 0;
        for part in output {
            let length = part.len().min(count.saturating_sub(copied));
            part[..length].copy_from_slice(&buffer[copied..copied + length]);
            copied += length;
            if copied == count {
                break;
            }
        }
        Ok(count)
    }
    fn write_vector_context(
        &self,
        input: &[IoSlice<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        let length = input
            .iter()
            .try_fold(0_usize, |total, part| total.checked_add(part.len()))
            .ok_or(ObjectError::ResourceLimit)?;
        let mut buffer = Vec::with_capacity(length);
        for part in input {
            buffer.extend_from_slice(part);
        }
        self.write_context(&buffer, context)
    }
    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        if self.netlink.is_some() {
            self.snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking = flags.bits() & StatusFlags::NONBLOCKING != 0;
            return Ok(());
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.set_status_flags(flags),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.set_status_flags(flags),
            RuntimeSocketKind::UnixStandalone { .. } => match self.standalone_connection() {
                Some((pair, endpoint)) => pair.endpoints[endpoint].description.set_status_flags(flags),
                None => Ok(()),
            },
        }?;
        let mut snapshot = self.snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        snapshot.nonblocking = flags.bits() & StatusFlags::NONBLOCKING != 0;
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .replace_snapshot(self.id, snapshot.clone())
            .map_err(|_| ObjectError::Io)
    }
    fn readiness(&self, interests: Readiness) -> Readiness {
        if let Some(netlink) = &self.netlink {
            let bits = Readiness::WRITE | if netlink.ready() { Readiness::READ } else { 0 };
            return Readiness::from_bits(bits & interests.bits());
        }
        let readiness = match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.readiness(interests),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.readiness(interests),
            RuntimeSocketKind::UnixStandalone { named, .. } => match self.standalone_connection() {
                Some((pair, endpoint)) => pair.endpoints[endpoint].description.readiness(interests),
                None if named.as_ref().is_some_and(|socket| socket.readable()) => Readiness::from_bits(Readiness::READ),
                None => Readiness::default(),
            },
        };
        match self.connect_status() {
            Ok(_) => readiness,
            Err(_) => Readiness::from_bits(Readiness::ERROR),
        }
    }
    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        if let Some(netlink) = &self.netlink {
            return netlink.observe(observer);
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.subscribe_readiness(observer),
            RuntimeSocketKind::Unix { pair, endpoint } => {
                pair.endpoints[*endpoint].description.subscribe_readiness(observer)
            }
            RuntimeSocketKind::UnixStandalone { .. } => match self.standalone_connection() {
                Some((pair, endpoint)) => pair.endpoints[endpoint].description.subscribe_readiness(observer),
                None => Err(ObjectError::NotSupported),
            },
        }
    }
    fn retire(&self) {
        if self.netlink.is_some() {
            self.unregister();
            return;
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.retire(),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.retire(),
            RuntimeSocketKind::UnixStandalone { .. } => {
                if let Some((pair, endpoint)) = self.standalone_connection() {
                    pair.endpoints[endpoint].description.retire();
                }
            }
        }
        self.unregister();
    }
    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.netlink.is_some() {
            self.unregister();
            self.catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .release();
            return;
        }
        match &self.kind {
            RuntimeSocketKind::Host { description, .. } => description.close(),
            RuntimeSocketKind::Unix { pair, endpoint } => pair.endpoints[*endpoint].description.close(),
            RuntimeSocketKind::UnixStandalone { .. } => {
                if let Some((pair, endpoint)) = self.standalone_connection() {
                    pair.endpoints[endpoint].description.close();
                }
            }
        }
        if let Some(datagram) = self.unix_datagram() {
            datagram.close();
        }
        self.unregister();
        if let Some((namespace, address, binding)) = self
            .unix_binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            namespace.release(&address, binding);
        }
        self.catalog
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release();
    }
}

#[derive(Debug)]
pub struct RuntimeSocketRegistry<H: SocketHostIo> {
    state: Mutex<RegistryState<H>>,
    unix: Mutex<Arc<hl_network::UnixNamespace>>,
}

#[derive(Debug)]
struct RegistryState<H: SocketHostIo> {
    generation: u64,
    sockets: BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>,
}

impl<H: SocketHostIo> Default for RuntimeSocketRegistry<H> {
    fn default() -> Self {
        Self {
            state: Mutex::new(RegistryState {
                generation: 1,
                sockets: BTreeMap::new(),
            }),
            unix: Mutex::new(Arc::new(hl_network::UnixNamespace::default())),
        }
    }
}

impl<H: SocketHostIo> RuntimeSocketRegistry<H> {
    #[must_use]
    pub fn unix_namespace(&self) -> Arc<hl_network::UnixNamespace> {
        self.unix
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn replace_unix(&self, replacement: Arc<hl_network::UnixNamespace>) -> Arc<hl_network::UnixNamespace> {
        std::mem::replace(
            &mut *self.unix.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
            replacement,
        )
    }

    pub fn host_token(&self, description: &DescriptionRef) -> Option<H::Token> {
        let socket = self.get(description.description_identity())?;
        match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => Some(*token),
            RuntimeSocketKind::Unix { .. } | RuntimeSocketKind::UnixStandalone { .. } => None,
        }
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        identity: DescriptionIdentity,
        socket: Arc<RuntimeSocket<H>>,
    ) -> Result<(), ()> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sockets.contains_key(&identity) {
            return Err(());
        }
        let generation = state.generation.checked_add(1).ok_or(())?;
        state.sockets.insert(identity, socket.clone());
        state.generation = generation;
        drop(state);
        socket.attach_registry(self, identity);
        Ok(())
    }
    pub(crate) fn get(&self, identity: DescriptionIdentity) -> Option<Arc<RuntimeSocket<H>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sockets
            .get(&identity)
            .cloned()
    }
    pub(crate) fn get_id(&self, id: SocketId) -> Option<Arc<RuntimeSocket<H>>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sockets
            .values()
            .find(|socket| socket.id == id)
            .cloned()
    }
    fn retire(&self, identity: DescriptionIdentity) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sockets.remove(&identity).is_some() {
            state.generation = state.generation.saturating_add(1);
        }
    }

    pub(crate) fn checkpoint_lease(&self) -> (u64, BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>) {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.generation, state.sockets.clone())
    }

    pub(crate) fn checkpoint_replace(
        self: &Arc<Self>,
        expected: u64,
        replacement: BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>,
    ) -> Result<(u64, BTreeMap<DescriptionIdentity, Arc<RuntimeSocket<H>>>), ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        if state.generation != expected {
            return Err(());
        }
        let generation = state.generation.checked_add(1).ok_or(())?;
        let previous = std::mem::replace(&mut state.sockets, replacement);
        state.generation = generation;
        let current = state.sockets.clone();
        drop(state);
        for (identity, socket) in &previous {
            socket.detach_registry(*identity);
        }
        for (identity, socket) in &current {
            socket.attach_registry(self, *identity);
        }
        Ok((generation, previous))
    }
}
