use std::mem::size_of;
use std::sync::{Arc, Weak};

use hl_descriptor::ReadinessObserver;
use hl_network::{
    SocketAddress, SocketConnectError, SocketConnectStatus, SocketHostError, SocketHostIo, SocketHostReadiness,
};

use super::Native;

pub(super) const SWITCH_CONNECT_ATTEMPTS: usize = 60;
pub(super) const SWITCH_CONNECT_PROBE_MILLIS: i32 = 40;

impl SocketHostIo for Native {
    type Token = u64;

    fn read(&self, token: u64, output: &mut [u8], _: bool) -> Result<usize, SocketHostError> {
        {
            let mut sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(entry) = sockets.get_mut(&token).filter(|entry| entry.resolver) {
                let Some(packet) = entry.resolver_packets.pop_front() else {
                    return Err(SocketHostError::WouldBlock);
                };
                entry.resolver_bytes = entry.resolver_bytes.saturating_sub(packet.len());
                let count = output.len().min(packet.len());
                output[..count].copy_from_slice(&packet[..count]);
                return Ok(count);
            }
        }
        let descriptor = self.descriptor(token).map_err(|_| SocketHostError::Canceled)?;
        // SAFETY: output is writable for output.len(), and the descriptor remains owned by the table.
        let result = unsafe { libc::recv(descriptor, output.as_mut_ptr().cast(), output.len(), 0) };
        self.arm_read(token);
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(Self::host_error())
        }
    }

    fn peek(&self, token: u64, output: &mut [u8]) -> Result<usize, SocketHostError> {
        {
            let sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(entry) = sockets.get(&token).filter(|entry| entry.resolver) {
                let Some(packet) = entry.resolver_packets.front() else {
                    return Err(SocketHostError::WouldBlock);
                };
                let count = output.len().min(packet.len());
                output[..count].copy_from_slice(&packet[..count]);
                return Ok(count);
            }
        }
        let descriptor = self.descriptor(token).map_err(|_| SocketHostError::Canceled)?;
        // SAFETY: output is writable for output.len(), the descriptor remains
        // owned by the table, and MSG_PEEK leaves queued bytes untouched.
        let result = unsafe {
            libc::recv(
                descriptor,
                output.as_mut_ptr().cast(),
                output.len(),
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if result >= 0 {
            Ok(result as usize)
        } else {
            Err(Self::host_error())
        }
    }

    fn write(&self, token: u64, input: &[u8], _: bool) -> Result<usize, SocketHostError> {
        {
            let mut sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(entry) = sockets.get_mut(&token).filter(|entry| entry.icmp) {
                let peer = entry.icmp_peer.clone().ok_or(SocketHostError::DestinationRequired)?;
                super::icmp::enqueue(&mut entry.icmp_packets, &mut entry.icmp_bytes, input, peer)
                    .map_err(|_| SocketHostError::WouldBlock)?;
                drop(sockets);
                self.notify(token);
                return Ok(input.len());
            }
            if let Some(entry) = sockets.get_mut(&token).filter(|entry| entry.resolver) {
                if let Some(response) = super::resolver::Resolver::answer(input) {
                    if !super::resolver::Resolver::queue_available(
                        entry.resolver_packets.len(),
                        entry.resolver_bytes,
                        response.len(),
                    ) {
                        return Err(SocketHostError::WouldBlock);
                    }
                    entry.resolver_bytes += response.len();
                    entry.resolver_packets.push_back(response);
                }
                drop(sockets);
                self.notify(token);
                return Ok(input.len());
            }
            if let Some(entry) = sockets.get(&token).filter(|entry| entry.datagram_peer.is_some()) {
                let peer = entry.datagram_peer.clone().expect("filtered datagram peer");
                let descriptor = entry.descriptor;
                drop(sockets);
                let (storage, length) = Self::socket_address(&SocketAddress::Unix(peer))
                    .map_err(|_| SocketHostError::DestinationRequired)?;
                // SAFETY: input and bounded sockaddr_un remain live while the table retains descriptor.
                let result = unsafe {
                    libc::sendto(
                        descriptor,
                        input.as_ptr().cast(),
                        input.len(),
                        0,
                        &storage as *const _ as *const _,
                        length,
                    )
                };
                return if result >= 0 {
                    Ok(result as usize)
                } else {
                    Err(Self::host_error())
                };
            }
        }
        let descriptor = self.descriptor(token).map_err(|_| SocketHostError::Canceled)?;
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_NOSIGNAL;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        // SAFETY: input is readable for input.len(), and the descriptor remains owned by the table.
        let result = unsafe { libc::send(descriptor, input.as_ptr().cast(), input.len(), flags) };
        if result >= 0 {
            return Ok(result as usize);
        }
        let error = Self::host_error();
        if error == SocketHostError::WouldBlock {
            let mut sockets = self.shared.sockets.lock().unwrap_or_else(|poison| poison.into_inner());
            let changed = sockets.get_mut(&token).is_some_and(super::Entry::arm_write);
            drop(sockets);
            if changed {
                self.wake();
            }
        }
        Err(error)
    }

    fn readiness(&self, token: u64) -> SocketHostReadiness {
        {
            let sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(entry) = sockets.get(&token).filter(|entry| entry.resolver) {
                return SocketHostReadiness {
                    readable: !entry.resolver_packets.is_empty(),
                    writable: true,
                    ..Default::default()
                };
            }
            if let Some(entry) = sockets.get(&token).filter(|entry| entry.icmp) {
                return SocketHostReadiness {
                    readable: !entry.icmp_packets.is_empty(),
                    writable: true,
                    ..Default::default()
                };
            }
        }
        let Ok(descriptor) = self.descriptor(token) else {
            return SocketHostReadiness {
                error: true,
                hangup: true,
                ..Default::default()
            };
        };
        let mut poll = libc::pollfd {
            fd: descriptor,
            events: libc::POLLIN | libc::POLLPRI | libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: poll references one initialized pollfd and timeout zero never blocks.
        let result = unsafe { libc::poll(&mut poll, 1, 0) };
        SocketHostReadiness {
            readable: result > 0 && poll.revents & libc::POLLIN != 0,
            priority: result > 0 && poll.revents & libc::POLLPRI != 0,
            read_hangup: false,
            writable: result > 0 && poll.revents & libc::POLLOUT != 0,
            error: result < 0 || poll.revents & (libc::POLLERR | libc::POLLNVAL) != 0,
            hangup: result > 0 && poll.revents & libc::POLLHUP != 0,
        }
    }

    fn start_connect(&self, token: u64, nonblocking: bool) -> SocketConnectStatus {
        let mut sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
        let Some(entry) = sockets.get_mut(&token) else {
            return SocketConnectStatus::Failed(SocketConnectError::Canceled);
        };
        let Some(address) = entry.pending.take() else {
            return SocketConnectStatus::Failed(SocketConnectError::Io);
        };
        if entry.datagram_peer.is_some() {
            entry.connecting = false;
            drop(sockets);
            self.notify(token);
            return SocketConnectStatus::Connected;
        }
        if super::resolver::Resolver::accepts(&address) {
            entry.resolver = true;
            entry.connecting = false;
            drop(sockets);
            self.notify(token);
            return SocketConnectStatus::Connected;
        }
        if entry.icmp {
            entry.icmp_peer = Some(address);
            entry.connecting = false;
            drop(sockets);
            self.notify(token);
            return SocketConnectStatus::Connected;
        }
        let retry_switch = entry.switched && matches!(address, SocketAddress::Unix(_));
        let loopback = entry.loopback_switch.take();
        drop(sockets);
        let (mut status, mut connect_failure) = self.attempt_connect(token, &address, nonblocking, retry_switch);
        // A nonblocking loopback connect only reports refusal from poll_connect, so the fallback
        // is retained until the host attempt has actually resolved.
        let deferred = match (&status, loopback) {
            (SocketConnectStatus::Failed(_), Some((path, peer))) => {
                if let Some(fallback) = self.loopback_switch_connect(token, path, peer, nonblocking) {
                    (status, connect_failure) = fallback;
                }
                None
            }
            (SocketConnectStatus::Pending, loopback) => loopback,
            _ => None,
        };
        let mut sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = sockets.get_mut(&token) {
            entry.loopback_switch = deferred;
            entry.connecting = status == SocketConnectStatus::Pending;
            entry.connect_failure = connect_failure;
        } else {
            status = SocketConnectStatus::Failed(SocketConnectError::Canceled);
        }
        drop(sockets);
        self.wake();
        status
    }

    fn poll_connect(&self, token: u64) -> SocketConnectStatus {
        let failure = {
            let mut sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
            sockets.get_mut(&token).and_then(|entry| entry.connect_failure.take())
        };
        if let Some(error) = failure {
            let mut sockets = self.shared.sockets.lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(entry) = sockets.get_mut(&token) {
                entry.connecting = false;
            }
            drop(sockets);
            self.wake();
            return SocketConnectStatus::Failed(error);
        }
        let Ok(descriptor) = self.descriptor(token) else {
            return SocketConnectStatus::Failed(SocketConnectError::Canceled);
        };
        let ready = self.readiness(token);
        if !ready.writable && !ready.error {
            return SocketConnectStatus::Pending;
        }
        let mut error = 0_i32;
        let mut length = size_of::<i32>() as libc::socklen_t;
        // SAFETY: error and length are writable and descriptor remains table-owned.
        let result = unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error as *mut i32).cast(),
                &mut length,
            )
        };
        let mut status = if result == 0 {
            Self::connect_error(error)
        } else {
            SocketConnectStatus::Failed(SocketConnectError::Io)
        };
        if matches!(status, SocketConnectStatus::Failed(_))
            && let Some((path, peer)) = self.take_loopback_switch(token)
            && let Some((retried, _)) = self.loopback_switch_connect(token, path, peer, true)
        {
            status = retried;
        }
        if status != SocketConnectStatus::Pending {
            let mut sockets = self.shared.sockets.lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(entry) = sockets.get_mut(&token) {
                entry.connecting = false;
            }
            drop(sockets);
            self.wake();
        }
        status
    }

    fn cancel(&self, token: u64) {
        let Ok(descriptor) = self.descriptor(token) else { return };
        let mut accepting = 0_i32;
        let mut length = size_of::<i32>() as libc::socklen_t;
        // SAFETY: accepting and length are writable scalars and descriptor is
        // retained by the reactor for this non-mutating query.
        let listener = unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_ACCEPTCONN,
                (&mut accepting as *mut i32).cast(),
                &mut length,
            ) == 0
                && accepting != 0
        };
        if listener {
            // Closing the process-local descriptor wakes local users. A
            // shutdown would invalidate the authority duplicate as well.
            self.wake();
            return;
        }
        // SAFETY: shutdown is idempotent for an owned socket descriptor.
        unsafe {
            libc::shutdown(descriptor, libc::SHUT_RDWR);
        }
    }

    fn attach_readiness(&self, token: u64, observer: Weak<dyn ReadinessObserver>) {
        self.shared
            .observers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(token, observer);
        self.wake();
    }

    fn detach_readiness(&self, token: u64) {
        self.shared
            .observers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&token);
    }

    fn close(&self, token: u64) {
        self.detach_readiness(token);
        let entry = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&token);
        if let Some(entry) = entry {
            if let Some(guest) = &entry.guest_local {
                self.shared
                    .bindings
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .retain(|(address, _)| address != guest);
            }
            if let Some(path) = &entry.switch_path
                && Arc::strong_count(path) == 1
            {
                let mut registry = self
                    .shared
                    .switch_paths
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                for owned_path in path.names() {
                    registry.remove(owned_path);
                }
            }
            // SAFETY: removal transfers the sole descriptor ownership to this call. Dropping the
            // final switch_path lease after close unlinks its rendezvous pathname.
            unsafe {
                libc::close(entry.descriptor);
            }
        }
        self.wake();
    }
}
