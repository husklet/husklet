use std::sync::Arc;

use hl_linux::{Errno, GuestMemory, LinuxResult, NetworkAbi};
use hl_network::{SocketAddress, SocketState, UnixAddress};

use crate::{RuntimeNetworkHost, RuntimeNetworkSyscalls, RuntimeSocketKind};

use super::errno::SocketErrno;

impl<H: RuntimeNetworkHost, M: GuestMemory> RuntimeNetworkSyscalls<H, M> {
    pub(crate) fn bind(&self, descriptor: i32, pointer: u64, length: u32) -> LinuxResult {
        let result = self.bind_result(descriptor, pointer, length);
        hl_log::hl_debug!(
            hl_log::tag::NET,
            "bind descriptor={descriptor} address={pointer:#x} length={length} result={:#x}",
            result.encode(),
        );
        result
    }

    fn override_port(address: &mut SocketAddress, port: u16) {
        match address {
            SocketAddress::Inet4 { port: current, .. } | SocketAddress::Inet6 { port: current, .. } => {
                *current = port;
            }
            SocketAddress::Unix(_) => unreachable!(),
        }
    }

    pub(crate) fn reserve_unix_connect(
        &self,
        client: &Arc<hl_network::UnixNamedSocket>,
        listener: &Arc<hl_network::UnixNamedSocket>,
        nonblocking: bool,
    ) -> Result<hl_network::UnixConnectReservation, LinuxResult> {
        let queue = listener.wait_queue();
        loop {
            let observed = queue.observation();
            match client.reserve_connect(listener, true) {
                Ok(value) => return Ok(value),
                Err(hl_network::UnixNamedSocketError::WouldBlock) if !nonblocking => {}
                Err(hl_network::UnixNamedSocketError::WouldBlock) => {
                    return Err(LinuxResult::Error(Errno::EAGAIN));
                }
                Err(_) => return Err(LinuxResult::Error(Errno::ECONNREFUSED)),
            }
            let Some(wait) = &self.wait else {
                return Err(LinuxResult::Error(Errno::EAGAIN));
            };
            match wait.wait(&queue, observed, None) {
                Ok(hl_sync::WaitOutcome::Notified) => {}
                Ok(hl_sync::WaitOutcome::Interrupted) => return Err(LinuxResult::Error(Errno::EINTR)),
                Ok(hl_sync::WaitOutcome::TimedOut) | Err(_) => return Err(LinuxResult::Error(Errno::EIO)),
            }
        }
    }

    fn prepare_bind_path(&self, raw: &[u8]) -> Result<Option<Box<dyn crate::PreparedUnixSocketPathBind>>, Errno> {
        if raw.is_empty() || raw[0] == 0 {
            return Ok(None);
        }
        let Some(paths) = &self.unix_socket_paths else {
            return Ok(None);
        };
        let Ok(pathname) = hl_vfs::GuestPathBytes::new(raw) else {
            return Err(Errno::EINVAL);
        };
        paths.prepare_bind(&pathname).map(Some)
    }

    fn rollback_prepared_bind(prepared: &mut Option<Box<dyn crate::PreparedUnixSocketPathBind>>) {
        if let Some(prepared) = prepared.take() {
            prepared.rollback();
        }
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
            let mut prepared_path = match self.prepare_bind_path(&raw) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error),
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
                    Self::rollback_prepared_bind(&mut prepared_path);
                    return LinuxResult::Error(Errno::EADDRINUSE);
                }
                Err(hl_network::UnixNamespaceError::Invalid) => {
                    Self::rollback_prepared_bind(&mut prepared_path);
                    return LinuxResult::Error(Errno::EINVAL);
                }
                Err(hl_network::UnixNamespaceError::Exhausted) => {
                    Self::rollback_prepared_bind(&mut prepared_path);
                    return LinuxResult::Error(Errno::ENOSPC);
                }
            };
            if socket
                .named_unix()
                .is_some_and(|named| named.bind(bound.clone()).is_err())
            {
                socket.rollback_unix_bind();
                Self::rollback_prepared_bind(&mut prepared_path);
                return LinuxResult::Error(Errno::EINVAL);
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
                    Self::rollback_prepared_bind(&mut prepared_path);
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
        // An internet socket cannot carry a unix address.
        if matches!(address, SocketAddress::Unix(_)) {
            return LinuxResult::Error(Errno::EAFNOSUPPORT);
        }
        if !self.host_projection && !Self::local_projection(&address) {
            // Only an address a namespace interface owns is bindable; Linux
            // reports EADDRNOTAVAIL for every other non-loopback address.
            if self.bind_route(address.clone()).interface.is_none() {
                return LinuxResult::Error(Errno::EADDRNOTAVAIL);
            }
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
                Self::override_port(&mut address, port);
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
            ) else {
                return LinuxResult::Error(Errno::EADDRINUSE);
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
}
