use std::sync::Arc;

use hl_descriptor::{OpenFileDescription, ReadinessObserver, StatusFlags};
use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult, NetworkAbi};
use hl_network::{SocketDescription, SocketState};

use crate::{
    RuntimeNetworkHost, RuntimeNetworkSyscalls, RuntimeSocket, RuntimeSocketKind, network::errno::SocketErrno,
};

struct AcceptWake(Arc<hl_sync::WaitQueue>);

impl ReadinessObserver for AcceptWake {
    fn readiness_changed(&self) {
        self.0.notify_all();
    }
}

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn accept4(
        &self,
        descriptor: i32,
        address_pointer: u64,
        length_pointer: u64,
        flags: u32,
    ) -> LinuxResult {
        let result = self.accept4_result(descriptor, address_pointer, length_pointer, flags);
        hl_log::hl_debug!(
            hl_log::tag::NET,
            "accept4 listener={descriptor} address={address_pointer:#x} flags={flags:#x} result={:#x}",
            result.encode(),
        );
        result
    }

    fn await_unix_pending(&self, named: &hl_network::UnixNamedSocket, nonblocking: bool) -> Option<LinuxResult> {
        let queue = named.wait_queue();
        loop {
            let observed = queue.observation();
            match named.accept(true) {
                Ok(_) => return None,
                Err(hl_network::UnixNamedSocketError::WouldBlock) if !nonblocking => {}
                Err(hl_network::UnixNamedSocketError::WouldBlock) => {
                    return Some(LinuxResult::Error(Errno::EAGAIN));
                }
                Err(_) => return Some(LinuxResult::Error(Errno::EINVAL)),
            }
            let Some(wait) = &self.wait else {
                return Some(LinuxResult::Error(Errno::EAGAIN));
            };
            match wait.wait(&queue, observed, None) {
                Ok(hl_sync::WaitOutcome::Notified) => {}
                Ok(hl_sync::WaitOutcome::Interrupted) => return Some(LinuxResult::Error(Errno::EINTR)),
                Ok(hl_sync::WaitOutcome::TimedOut) | Err(_) => return Some(LinuxResult::Error(Errno::EIO)),
            }
        }
    }

    fn accept4_result(&self, descriptor: i32, address_pointer: u64, length_pointer: u64, flags: u32) -> LinuxResult {
        if flags & !(0x800 | 0x8_0000) != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let listener = match self.lookup(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if let RuntimeSocketKind::UnixStandalone { .. } = &listener.kind {
            let Some(named) = listener.named_unix() else {
                return LinuxResult::Error(Errno::EINVAL);
            };
            let nonblocking = listener
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .nonblocking;
            if let Some(error) = self.await_unix_pending(&named, nonblocking) {
                return error;
            }
            let Ok(id) = self.current_catalog().accept_pending_unix(listener.id) else {
                return LinuxResult::Error(Errno::EIO);
            };
            let Some(object) = listener.take_pending(id) else {
                return LinuxResult::Error(Errno::EIO);
            };
            let snapshot = object
                .snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let peer = snapshot
                .peer
                .clone()
                .unwrap_or(hl_network::SocketAddress::Unix(Vec::new()));
            let (status, descriptor_flags) = Self::descriptor_flags(flags);
            return self.install_accepted(object, status, descriptor_flags, address_pointer, length_pointer, &peer);
        }
        let (host, description, token) = match (&self.host, &listener.kind) {
            (Some(host), RuntimeSocketKind::Host { description, token }) => (host, description, *token),
            _ => return LinuxResult::Error(Errno::ENOSYS),
        };
        let listener_snapshot = listener
            .snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let nonblocking = listener_snapshot.nonblocking;
        // Only a listener bind left virtual lacks a host socket to accept on; a
        // loopback listener is real and must consult the host.
        let virtual_listener = !listener_snapshot.local.as_ref().is_some_and(Self::local_projection);
        if !self.host_projection
            && virtual_listener
            && nonblocking
            && matches!(listener_snapshot.state, SocketState::Listening { .. })
        {
            return LinuxResult::Error(Errno::EAGAIN);
        }
        let queue = Arc::new(hl_sync::WaitQueue::new());
        let observer: Arc<dyn ReadinessObserver> = Arc::new(AcceptWake(queue.clone()));
        let Ok(_subscription) = description.subscribe_readiness(observer) else {
            return LinuxResult::Error(Errno::EIO);
        };
        let accepted = loop {
            let observed = queue.observation();
            match host.accept(token) {
                Ok(value) => break value,
                Err(crate::RuntimeNetworkError::WouldBlock) if !nonblocking => {}
                Err(error) => return LinuxResult::Error(SocketErrno::runtime(error)),
            }
            let Some(wait) = &self.wait else {
                return LinuxResult::Error(Errno::EAGAIN);
            };
            match wait.wait(&queue, observed, None) {
                Ok(hl_sync::WaitOutcome::Notified) => {}
                Ok(hl_sync::WaitOutcome::Interrupted) => {
                    return LinuxResult::Error(Errno::EINTR);
                }
                Ok(hl_sync::WaitOutcome::TimedOut) | Err(_) => {
                    return LinuxResult::Error(Errno::EIO);
                }
            }
        };
        let (status, descriptor_flags) = Self::descriptor_flags(flags);
        let description = Arc::new(SocketDescription::new(host.clone(), accepted.token, status));
        description.bind_readiness();
        let mut snapshot = Self::snapshot(
            listener_snapshot.family,
            listener_snapshot.socket_type,
            listener_snapshot.protocol,
            status,
            Some(accepted.local.clone()),
            Some(accepted.peer.clone()),
        );
        snapshot.state = SocketState::Connected;
        let catalog = self.current_catalog();
        let Ok(id) = catalog.insert_host(snapshot.clone(), accepted.resource, accepted.binding, Vec::new()) else {
            description.close();
            return LinuxResult::Error(Errno::ENFILE);
        };
        snapshot.id = id;
        let object = RuntimeSocket::host(description, accepted.token, id, snapshot, catalog);
        self.install_accepted(
            object,
            status,
            descriptor_flags,
            address_pointer,
            length_pointer,
            &accepted.peer,
        )
    }

    fn install_accepted(
        &self,
        object: Arc<RuntimeSocket<H>>,
        status: StatusFlags,
        flags: hl_descriptor::DescriptorFlags,
        address_pointer: u64,
        length_pointer: u64,
        peer: &hl_network::SocketAddress,
    ) -> LinuxResult {
        let install =
            match self
                .descriptors
                .prepare_open(0, object.clone() as Arc<dyn OpenFileDescription>, status, flags)
            {
                Ok(value) => value,
                Err(error) => {
                    object.close();
                    return LinuxResult::Error(crate::filesystem::FileErrno::descriptor(error));
                }
            };
        if self
            .sockets
            .register(install.description_identity(), object.clone())
            .is_err()
        {
            object.close();
            return LinuxResult::Error(Errno::ENFILE);
        }
        if address_pointer != 0 {
            let address = Self::guest_address(peer);
            let abi = NetworkAbi::new(&self.memory, self.architecture);
            let copyout = match abi.prepare_sockaddr_copyout(address_pointer, length_pointer, &address) {
                Ok(value) => value,
                Err(error) => {
                    object.close();
                    return LinuxResult::Error(SocketErrno::marshal(error));
                }
            };
            if let Err(error) = copyout.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
                object.close();
                return LinuxResult::Error(SocketErrno::marshal(error));
            }
        }
        LinuxResult::Value(install.publish() as u64)
    }
}
