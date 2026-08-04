use std::sync::Arc;

use hl_descriptor::DescriptionIdentity;
use hl_network::SocketType;

use crate::{RuntimeNetworkHost, RuntimeSocketKind, RuntimeSocketRegistry, SocketIoctlPort};

/// Joins descriptor identity to socket state for socket-owned ioctl operations.
pub struct SocketIoctl<H: RuntimeNetworkHost> {
    host: Arc<H>,
    sockets: Arc<RuntimeSocketRegistry<H>>,
    policy: hl_network::NetworkPolicy,
}

impl<H: RuntimeNetworkHost> SocketIoctl<H> {
    pub fn new(host: Arc<H>, sockets: Arc<RuntimeSocketRegistry<H>>) -> Self {
        let policy =
            hl_network::NetworkPolicy::from_launch(false, b"", b"", b"").expect("default namespace interface policy");
        Self { host, sockets, policy }
    }

    #[must_use]
    pub fn with_policy(mut self, policy: hl_network::NetworkPolicy) -> Self {
        self.policy = policy;
        self
    }
}

impl<H: RuntimeNetworkHost> SocketIoctlPort for SocketIoctl<H> {
    fn input_queue(&self, identity: DescriptionIdentity) -> Result<Option<u64>, ()> {
        let Some(socket) = self.sockets.get(identity) else {
            return Ok(None);
        };
        match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => self.host.input_queue(*token).map(Some).map_err(|_| ()),
            RuntimeSocketKind::Unix { pair, endpoint } => Ok(Some(pair.endpoints[*endpoint].readable_bytes() as u64)),
            RuntimeSocketKind::UnixStandalone { .. } => Ok(Some(
                socket
                    .standalone_connection()
                    .map_or(0, |(pair, endpoint)| pair.endpoints[endpoint].readable_bytes()) as u64,
            )),
        }
    }

    fn output_queue(&self, identity: DescriptionIdentity) -> Result<Option<u64>, ()> {
        let Some(socket) = self.sockets.get(identity) else {
            return Ok(None);
        };
        if socket
            .snapshot
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .socket_type
            != SocketType::Stream
        {
            return Err(());
        }
        match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => self.host.output_queue(*token).map(Some).map_err(|_| ()),
            RuntimeSocketKind::Unix { .. } => Ok(Some(0)),
            RuntimeSocketKind::UnixStandalone { .. } => Ok(Some(0)),
        }
    }

    fn at_urgent_mark(&self, identity: DescriptionIdentity) -> Result<Option<bool>, ()> {
        let Some(socket) = self.sockets.get(identity) else {
            return Ok(None);
        };
        match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => self.host.at_urgent_mark(*token).map(Some).map_err(|_| ()),
            RuntimeSocketKind::Unix { .. } | RuntimeSocketKind::UnixStandalone { .. } => Err(()),
        }
    }

    fn interfaces(&self, identity: DescriptionIdentity) -> Result<Option<Vec<hl_network::NamespaceInterface>>, ()> {
        Ok(self.sockets.get(identity).map(|_| self.policy.namespace_interfaces()))
    }
}
