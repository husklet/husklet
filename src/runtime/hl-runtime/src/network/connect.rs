use std::sync::Arc;

use hl_descriptor::StatusFlags;
use hl_linux::{Errno, GuestMemory, GuestNetworkAddress, LinuxResult, NetworkAbi};
use hl_network::{SocketAddress, SocketConnectStatus, SocketState, SocketType, UnixAddress, UnixSocketPair};

use crate::{RuntimeNetworkHost, RuntimeNetworkSyscalls, RuntimeSocket, RuntimeSocketKind};

use super::errno::SocketErrno;

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn listen(&self, descriptor: i32, backlog: i32) -> LinuxResult {
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

    pub(crate) fn connect(&self, descriptor: i32, pointer: u64, length: u32) -> LinuxResult {
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
            if let Some(datagram) = socket.unix_datagram()
                && (listener.unix_datagram().is_none() || datagram.connect(target).is_err())
            {
                return LinuxResult::Error(Errno::ECONNREFUSED);
            }
            if socket.unix_datagram().is_some() {
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
            let reservation = match self.reserve_unix_connect(&client_named, &listener_named, nonblocking) {
                Ok(value) => value,
                Err(result) => return result,
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
            ) else {
                return LinuxResult::Error(Errno::EIO);
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
}
