use std::sync::Arc;

use hl_network::{
    AcceptedSocketCheckpoint, AuthoritySocketKey, NetworkCheckpointError, NetworkCheckpointImage, NetworkResourceKey,
    NetworkSocketResource, PortCheckpoint, SocketSnapshot,
};

use crate::RuntimeNetworkHost;

pub struct ReconnectedSocket<T> {
    pub token: T,
    pub binding: Arc<dyn NetworkSocketResource>,
}

/// External authority used to retain and re-acquire host sockets.
pub trait CheckpointHost: RuntimeNetworkHost {
    fn capture_prepare(&self) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }

    fn retain_listener(
        &self,
        _snapshot: &SocketSnapshot,
        _resource: NetworkResourceKey,
    ) -> Result<AuthoritySocketKey, NetworkCheckpointError> {
        Err(NetworkCheckpointError::InvalidImage)
    }

    fn capture_publish(&self, _digest: [u8; 32]) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }
    fn capture_abort(&self) {}
    fn capture_finish(&self) {}

    fn restore_begin(&self, _digest: [u8; 32], _image: &NetworkCheckpointImage) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }

    fn reserve_ports(&self, ports: &[PortCheckpoint]) -> Result<(), NetworkCheckpointError> {
        if ports.is_empty() {
            Ok(())
        } else {
            Err(NetworkCheckpointError::InvalidImage)
        }
    }

    fn reconnect(
        &self,
        snapshot: &SocketSnapshot,
        resource: NetworkResourceKey,
    ) -> Result<ReconnectedSocket<Self::Token>, NetworkCheckpointError>;

    fn reconnect_accepted(
        &self,
        accepted: &AcceptedSocketCheckpoint,
    ) -> Result<ReconnectedSocket<Self::Token>, NetworkCheckpointError>;

    fn reconnect_retained(
        &self,
        snapshot: &SocketSnapshot,
        resource: NetworkResourceKey,
        _key: AuthoritySocketKey,
    ) -> Result<ReconnectedSocket<Self::Token>, NetworkCheckpointError> {
        self.reconnect(snapshot, resource)
    }

    fn checkpoint_commit(&self) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }
    fn checkpoint_rollback(&self) {}
    fn checkpoint_resume(&self) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }
}
