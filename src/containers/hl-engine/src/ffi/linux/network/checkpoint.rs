use std::net::TcpListener;
use std::os::fd::IntoRawFd;
use std::sync::{Arc, MutexGuard};

use hl_network::{
    AuthoritySocketKey, NetworkCheckpointError, NetworkCheckpointImage, NetworkResourceKey, SocketHostIo,
    SocketSnapshot, SocketState,
};
use hl_runtime::{CreatedSocket, NetworkCheckpointHost, ReconnectedSocket, RuntimeNetworkHost};

use super::native::Native;

impl Native {
    pub(super) fn adopt_listener(&self, listener: TcpListener) -> Result<CreatedSocket<u64>, NetworkCheckpointError> {
        let adopted = self.checkpoint_insert(listener.into_raw_fd())?;
        let resource = NetworkResourceKey::new(adopted.token).ok_or(NetworkCheckpointError::ResourceLimit)?;
        Ok(CreatedSocket {
            token: adopted.token,
            resource,
            binding: adopted.binding,
        })
    }

    fn authority(&self) -> Result<MutexGuard<'_, crate::native::AuthorityWorker>, NetworkCheckpointError> {
        self.authority
            .as_ref()
            .ok_or(NetworkCheckpointError::InvalidImage)?
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)
    }

    fn checkpoint_insert(&self, descriptor: i32) -> Result<ReconnectedSocket<u64>, NetworkCheckpointError> {
        // SAFETY: descriptor is newly owned here and both calls mutate only its flags.
        let configured = unsafe {
            libc::fcntl(descriptor, libc::F_SETFL, libc::O_NONBLOCK) == 0
                && libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) == 0
        };
        if !configured {
            // SAFETY: descriptor remains solely owned because it has not entered the reactor.
            unsafe {
                libc::close(descriptor);
            }
            return Err(NetworkCheckpointError::ResourceLimit);
        }
        let token = self
            .insert(descriptor)
            .map_err(|_| NetworkCheckpointError::ResourceLimit)?;
        Ok(ReconnectedSocket {
            token,
            binding: Arc::new(()),
        })
    }

    fn recreate(&self, snapshot: &SocketSnapshot) -> Result<ReconnectedSocket<u64>, NetworkCheckpointError> {
        if !matches!(snapshot.state, SocketState::Created | SocketState::Bound) {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let created = self
            .create(snapshot.family, snapshot.socket_type, snapshot.protocol)
            .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        if snapshot.state == SocketState::Bound {
            let address = snapshot.local.clone().ok_or(NetworkCheckpointError::InvalidImage)?;
            if self.bind(created.token, address).is_err() {
                self.close(created.token);
                return Err(NetworkCheckpointError::InvalidImage);
            }
        }
        Ok(ReconnectedSocket {
            token: created.token,
            binding: created.binding,
        })
    }
}

impl NetworkCheckpointHost for Native {
    fn capture_prepare(&self) -> Result<(), NetworkCheckpointError> {
        let Some(authority) = &self.authority else {
            return Ok(());
        };
        authority
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)?
            .network()
            .capture_prepare()
            .map_err(|_| NetworkCheckpointError::InvalidImage)
    }

    fn retain_listener(
        &self,
        snapshot: &SocketSnapshot,
        resource: NetworkResourceKey,
    ) -> Result<AuthoritySocketKey, NetworkCheckpointError> {
        if !matches!(snapshot.state, SocketState::Listening { .. }) {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let descriptor = self
            .descriptor(resource.value())
            .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        self.authority()?
            .network()
            .retain_listener(descriptor, resource.value())
            .map_err(|_| NetworkCheckpointError::InvalidImage)
    }

    fn capture_publish(&self, digest: [u8; 32]) -> Result<(), NetworkCheckpointError> {
        let Some(authority) = &self.authority else {
            return Ok(());
        };
        authority
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)?
            .network()
            .capture_publish(digest)
            .map_err(|_| NetworkCheckpointError::InvalidImage)
    }

    fn capture_abort(&self) {
        let result = self.authority().and_then(|mut authority| {
            authority
                .network()
                .capture_abort()
                .map_err(|_| NetworkCheckpointError::InvalidImage)
        });
        match result {
            Ok(()) | Err(_) => {}
        }
    }

    fn capture_finish(&self) {
        let result = self.authority().and_then(|mut authority| {
            authority
                .network()
                .capture_finish()
                .map_err(|_| NetworkCheckpointError::InvalidImage)
        });
        match result {
            Ok(()) | Err(_) => {}
        }
    }

    fn restore_begin(&self, digest: [u8; 32], image: &NetworkCheckpointImage) -> Result<(), NetworkCheckpointError> {
        let Some(authority) = &self.authority else {
            return image
                .authority
                .is_empty()
                .then_some(())
                .ok_or(NetworkCheckpointError::InvalidImage);
        };
        authority
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)?
            .network()
            .restore_begin(digest, image.authority.len())
            .map_err(|_| NetworkCheckpointError::InvalidImage)
    }

    fn reserve_ports(&self, _: &[hl_network::PortCheckpoint]) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }

    fn reconnect(
        &self,
        snapshot: &SocketSnapshot,
        _: NetworkResourceKey,
    ) -> Result<ReconnectedSocket<u64>, NetworkCheckpointError> {
        self.recreate(snapshot)
    }

    fn reconnect_retained(
        &self,
        snapshot: &SocketSnapshot,
        _: NetworkResourceKey,
        key: AuthoritySocketKey,
    ) -> Result<ReconnectedSocket<u64>, NetworkCheckpointError> {
        if !matches!(snapshot.state, SocketState::Listening { .. }) {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let descriptor = self
            .authority()?
            .network()
            .restore_stage(key)
            .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        let restored = self.checkpoint_insert(descriptor.into_raw_fd())?;
        let SocketState::Listening { backlog } = snapshot.state else {
            return Err(NetworkCheckpointError::InvalidImage);
        };
        if self.listen(restored.token, backlog).is_err() {
            self.close(restored.token);
            return Err(NetworkCheckpointError::InvalidImage);
        }
        Ok(restored)
    }

    fn reconnect_accepted(
        &self,
        _: &hl_network::AcceptedSocketCheckpoint,
    ) -> Result<ReconnectedSocket<u64>, NetworkCheckpointError> {
        Err(NetworkCheckpointError::InvalidImage)
    }

    fn checkpoint_commit(&self) -> Result<(), NetworkCheckpointError> {
        let Some(authority) = &self.authority else {
            return Ok(());
        };
        authority
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)?
            .network()
            .restore_commit()
            .map_err(|_| NetworkCheckpointError::InvalidImage)
    }

    fn checkpoint_rollback(&self) {
        let result = self.authority().and_then(|mut authority| {
            authority
                .network()
                .restore_abort()
                .map_err(|_| NetworkCheckpointError::InvalidImage)
        });
        match result {
            Ok(()) | Err(_) => {}
        }
    }

    fn checkpoint_resume(&self) -> Result<(), NetworkCheckpointError> {
        let Some(authority) = &self.authority else {
            return Ok(());
        };
        authority
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)?
            .network()
            .restore_resume()
            .map_err(|_| NetworkCheckpointError::InvalidImage)
    }
}

#[cfg(test)]
mod tests {
    use hl_network::{
        AuthoritySocketKey, AuthoritySocketLease, NETWORK_CHECKPOINT_VERSION, NetworkCheckpointError,
        NetworkCheckpointImage, NetworkConfiguration, NetworkResourceKey,
    };
    use hl_runtime::NetworkCheckpointHost;

    use super::Native;

    #[test]
    fn internal_network_capture_does_not_require_external_authority() {
        let host = Native::new();
        host.capture_prepare().unwrap();
        host.capture_publish([7; 32]).unwrap();
        host.capture_finish();
    }

    #[test]
    fn internal_network_restore_does_not_require_external_authority() {
        let host = Native::new();
        let image = NetworkCheckpointImage {
            version: NETWORK_CHECKPOINT_VERSION,
            generations: Vec::new(),
            configuration: NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
            ports: Vec::new(),
            authority: Vec::new(),
            sockets: Vec::new(),
        };
        host.restore_begin([9; 32], &image).unwrap();
        host.checkpoint_commit().unwrap();
        host.checkpoint_resume().unwrap();
    }

    #[test]
    fn internal_network_restore_rejects_an_image_carrying_authority_state() {
        let host = Native::new();
        let image = NetworkCheckpointImage {
            version: NETWORK_CHECKPOINT_VERSION,
            generations: Vec::new(),
            configuration: NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
            ports: Vec::new(),
            authority: vec![AuthoritySocketLease {
                resource: NetworkResourceKey::new(1).unwrap(),
                key: AuthoritySocketKey::new(1, 1).unwrap(),
            }],
            sockets: Vec::new(),
        };
        // A host with no authority cannot restore sockets the image says an authority retained.
        assert!(matches!(
            host.restore_begin([9; 32], &image),
            Err(NetworkCheckpointError::InvalidImage)
        ));
    }
}
