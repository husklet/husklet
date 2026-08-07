//! Connect attempts and loopback switch handoff for the native socket host.

use hl_network::{SocketAddress, SocketConnectError, SocketConnectStatus};

use super::Native;
use super::io::{SWITCH_CONNECT_ATTEMPTS, SWITCH_CONNECT_PROBE_MILLIS};

impl Native {
    pub(super) fn attempt_connect(
        &self,
        token: u64,
        address: &SocketAddress,
        nonblocking: bool,
        retry_switch: bool,
    ) -> (SocketConnectStatus, Option<SocketConnectError>) {
        let Ok((storage, length)) = Self::socket_address(address) else {
            return (SocketConnectStatus::Failed(SocketConnectError::Io), None);
        };
        let mut status = SocketConnectStatus::Failed(SocketConnectError::Io);
        let mut connect_failure = None;
        for attempt in 0..SWITCH_CONNECT_ATTEMPTS {
            if attempt != 0 && self.reset_switch_socket(token, libc::SOCK_STREAM).is_err() {
                break;
            }
            let Ok(descriptor) = self.duplicate_descriptor(token) else {
                status = SocketConnectStatus::Failed(SocketConnectError::Canceled);
                break;
            };
            // SAFETY: storage is immutable and descriptor is an independently owned duplicate.
            let result = unsafe { libc::connect(descriptor, (&raw const storage).cast(), length) };
            let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if result == 0 {
                let dead_on_arrival = retry_switch && Self::switch_dead_on_arrival(descriptor);
                // SAFETY: this call solely owns the duplicated descriptor.
                unsafe { libc::close(descriptor) };
                if !dead_on_arrival {
                    status = SocketConnectStatus::Connected;
                    break;
                }
                status = SocketConnectStatus::Failed(SocketConnectError::Refused);
                if attempt + 1 != SWITCH_CONNECT_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    continue;
                }
                break;
            }
            // SAFETY: this call solely owns the duplicated descriptor.
            unsafe { libc::close(descriptor) };
            if error == libc::EINPROGRESS {
                status = SocketConnectStatus::Pending;
                break;
            }
            if retry_switch && nonblocking && matches!(error, libc::ENOENT | libc::ECONNREFUSED) {
                status = SocketConnectStatus::Pending;
                connect_failure = Some(SocketConnectError::Refused);
                break;
            }
            if retry_switch && !nonblocking && matches!(error, libc::ENOENT | libc::ECONNREFUSED) {
                status = SocketConnectStatus::Failed(SocketConnectError::Refused);
                if attempt + 1 != SWITCH_CONNECT_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    continue;
                }
            } else {
                status = Self::connect_error(error);
            }
            break;
        }
        (status, connect_failure)
    }

    pub(super) fn take_loopback_switch(&self, token: u64) -> Option<(Vec<u8>, SocketAddress)> {
        self.shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&token)?
            .loopback_switch
            .take()
    }

    /// Retries a refused loopback connect against the wildcard listener's bridge rendezvous.
    /// A fresh switch socket replaces the INET one, and a failed retry restores it so the guest
    /// still observes the original loopback failure.
    pub(super) fn loopback_switch_connect(
        &self,
        token: u64,
        path: Vec<u8>,
        peer: SocketAddress,
        nonblocking: bool,
    ) -> Option<(SocketConnectStatus, Option<SocketConnectError>)> {
        self.switch_socket(token, libc::SOCK_STREAM).ok()?;
        let outcome = self.attempt_connect(token, &SocketAddress::Unix(path), nonblocking, false);
        if matches!(outcome.0, SocketConnectStatus::Failed(_)) {
            let _ = self.restore_inet_socket(token, libc::SOCK_STREAM);
            return None;
        }
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sockets.get_mut(&token)?.guest_peer = Some(peer);
        Some(outcome)
    }

    fn switch_dead_on_arrival(descriptor: i32) -> bool {
        let mut poll = libc::pollfd {
            fd: descriptor,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll references one initialized pollfd and retains no pointer after the bounded wait.
        let result = unsafe { libc::poll(&raw mut poll, 1, SWITCH_CONNECT_PROBE_MILLIS) };
        if result <= 0 {
            return false;
        }
        let mut byte = 0_u8;
        // SAFETY: byte is writable for one byte and MSG_PEEK does not consume pending guest data.
        let received = unsafe {
            libc::recv(
                descriptor,
                (&raw mut byte).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if received == 0 {
            return true;
        }
        if received < 0 {
            let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return error != libc::EAGAIN && error != libc::EWOULDBLOCK;
        }
        false
    }
}
