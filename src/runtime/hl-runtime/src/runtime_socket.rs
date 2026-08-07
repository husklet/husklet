use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::{DescriptionIdentity, ObjectError, OpenFileDescription};
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

mod description;
mod registry;

pub use registry::RuntimeSocketRegistry;
