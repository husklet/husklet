use std::sync::Arc;

use hl_descriptor::{DescriptorFlags, OpenFileDescription, StatusFlags};
use hl_network::{NetworkCatalog, NetworkCheckpointError, SocketAddress, SocketDescription, SocketSnapshot};

use crate::{CreatedSocket, RuntimeNetworkHost, RuntimeSocket, RuntimeSocketKind};

use super::{CheckpointHost, ObjectBindings};

impl<H: CheckpointHost> ObjectBindings<H> {
    /// Publishes a host socket through the same catalog, OFD registry, and
    /// descriptor table used by checkpoint capture and restore.
    pub fn publish_host(
        &self,
        network: Arc<NetworkCatalog>,
        created: CreatedSocket<H::Token>,
        mut snapshot: SocketSnapshot,
        status: StatusFlags,
        flags: DescriptorFlags,
    ) -> Result<i32, NetworkCheckpointError> {
        let host = self.host.as_ref().ok_or(NetworkCheckpointError::InvalidImage)?;
        let description = Arc::new(SocketDescription::new(Arc::clone(host), created.token, status));
        description.bind_readiness();
        if let hl_network::SocketState::Listening { backlog } = snapshot.state {
            description.listen(backlog as usize);
        }
        let descriptors = self.descriptors.current();
        let id = network
            .insert_host(snapshot.clone(), created.resource, created.binding, Vec::new())
            .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        snapshot.id = id;
        let object = RuntimeSocket::host(description, created.token, id, snapshot, network);
        let Ok(install) = descriptors.prepare_open(0, object.clone(), status, flags) else {
            object.close();
            return Err(NetworkCheckpointError::ResourceLimit);
        };
        if self
            .sockets
            .register(install.description_identity(), object.clone())
            .is_err()
        {
            object.close();
            return Err(NetworkCheckpointError::ResourceLimit);
        }
        Ok(install.publish())
    }

    /// Resolves one published guest descriptor through the checkpoint-owned
    /// OFD registry and asks the selected host for its live local address.
    pub fn host_local_address(&self, descriptor: i32) -> Result<SocketAddress, NetworkCheckpointError>
    where
        H: RuntimeNetworkHost,
    {
        let table = self.descriptors.current();
        let lease = table
            .pin(descriptor)
            .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        let socket = self
            .sockets
            .get(lease.description_identity())
            .ok_or(NetworkCheckpointError::InvalidImage)?;
        let token = match &socket.kind {
            RuntimeSocketKind::Host { token, .. } => *token,
            RuntimeSocketKind::Unix { .. } | RuntimeSocketKind::UnixStandalone { .. } => {
                return Err(NetworkCheckpointError::InvalidImage);
            }
        };
        self.host
            .as_ref()
            .ok_or(NetworkCheckpointError::InvalidImage)?
            .local_address(token)
            .map_err(|_| NetworkCheckpointError::InvalidImage)
    }
}
