#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::ReadinessObserver;
use hl_network::{SocketAddress, SocketConnectError, SocketConnectStatus, SocketHostError};
use hl_runtime::RuntimeNetworkError;

mod icmp;
mod io;
mod message;
mod resolver;
mod runtime;

const SOCKET_LIMIT: usize = 4096;

pub(super) struct Entry {
    pub(super) descriptor: i32,
    pub(super) pending: Option<SocketAddress>,
    pub(super) wants_read: bool,
    pub(super) connecting: bool,
    pub(super) connect_failure: Option<SocketConnectError>,
    pub(super) wants_write: bool,
    pub(super) resolver: bool,
    pub(super) resolver_packets: VecDeque<Vec<u8>>,
    pub(super) resolver_bytes: usize,
    pub(super) icmp: bool,
    pub(super) icmp_peer: Option<SocketAddress>,
    pub(super) icmp_packets: VecDeque<(Vec<u8>, SocketAddress)>,
    pub(super) icmp_bytes: usize,
    pub(super) guest_local: Option<SocketAddress>,
    pub(super) guest_peer: Option<SocketAddress>,
    pub(super) switch_path: Option<Arc<SwitchPath>>,
    pub(super) switch_interface: Option<hl_network::EgressInterface>,
    pub(super) datagram_peer: Option<Vec<u8>>,
    pub(super) switched: bool,
}

pub(super) struct SwitchPath(Vec<u8>);

impl Drop for SwitchPath {
    fn drop(&mut self) {
        if let Ok(path) = std::ffi::CString::new(self.0.clone()) {
            // SAFETY: path is a live NUL-terminated pathname and unlink retains no pointer.
            unsafe {
                libc::unlink(path.as_ptr());
            }
        }
    }
}

pub(crate) struct Native {
    shared: Arc<Reactor>,
    pub(super) authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
}

pub(super) struct Reactor {
    pub(super) next: AtomicU64,
    pub(super) sockets: Mutex<BTreeMap<u64, Entry>>,
    pub(super) observers: Mutex<BTreeMap<u64, Weak<dyn ReadinessObserver>>>,
    pub(super) bindings: Mutex<Vec<(SocketAddress, SocketAddress)>>,
    pub(super) switch_paths: Mutex<BTreeMap<Vec<u8>, Weak<SwitchPath>>>,
    pub(super) wake_read: i32,
    pub(super) wake_write: i32,
}

impl std::fmt::Debug for Native {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.debug_struct("Native").finish_non_exhaustive()
    }
}

impl Native {
    pub(crate) fn new() -> Self {
        Self::with_authority(None)
    }

    pub(crate) fn authorized(authority: Arc<Mutex<crate::native::AuthorityWorker>>) -> Self {
        Self::with_authority(Some(authority))
    }

    fn with_authority(authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>) -> Self {
        let mut wake = [0_i32; 2];
        // SAFETY: wake points to two writable integers; successful pipe returns two owned descriptors.
        if unsafe { libc::pipe(wake.as_mut_ptr()) } != 0 {
            panic!("native network wake pipe creation failed");
        }
        // SAFETY: both descriptors are newly owned and valid for fcntl flag mutation.
        unsafe {
            libc::fcntl(wake[0], libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(wake[1], libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(wake[0], libc::F_SETFD, libc::FD_CLOEXEC);
            libc::fcntl(wake[1], libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let shared = Arc::new(Reactor {
            next: AtomicU64::new(1),
            sockets: Mutex::new(BTreeMap::new()),
            observers: Mutex::new(BTreeMap::new()),
            bindings: Mutex::new(Vec::new()),
            switch_paths: Mutex::new(BTreeMap::new()),
            wake_read: wake[0],
            wake_write: wake[1],
        });
        let weak = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("hl-inet-ready".into())
            .spawn(move || Reactor::run(weak))
            .expect("native network reactor creation failed");
        Self { shared, authority }
    }

    pub(super) fn insert(&self, descriptor: i32) -> Result<u64, RuntimeNetworkError> {
        let mut sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
        if sockets.len() >= SOCKET_LIMIT {
            // SAFETY: descriptor is newly owned here and no other owner can close it.
            unsafe {
                libc::close(descriptor);
            }
            return Err(RuntimeNetworkError::NoMemory);
        }
        let token = self.shared.next.fetch_add(1, Ordering::Relaxed);
        if token == 0 {
            // SAFETY: descriptor is newly owned here and no other owner can close it.
            unsafe {
                libc::close(descriptor);
            }
            return Err(RuntimeNetworkError::NoMemory);
        }
        let switch_path = Self::descriptor_unix_path(descriptor).and_then(|path| {
            self.shared
                .switch_paths
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(&path)
                .and_then(Weak::upgrade)
        });
        sockets.insert(
            token,
            Entry {
                descriptor,
                pending: None,
                wants_read: true,
                connecting: false,
                connect_failure: None,
                wants_write: false,
                resolver: false,
                resolver_packets: VecDeque::new(),
                resolver_bytes: 0,
                icmp: false,
                icmp_peer: None,
                icmp_packets: VecDeque::new(),
                icmp_bytes: 0,
                guest_local: None,
                guest_peer: None,
                switch_path,
                switch_interface: None,
                datagram_peer: None,
                switched: false,
            },
        );
        drop(sockets);
        self.wake();
        Ok(token)
    }

    fn descriptor_unix_path(descriptor: i32) -> Option<Vec<u8>> {
        // SAFETY: zero is valid sockaddr_storage initialization.
        let mut storage = unsafe { zeroed::<libc::sockaddr_storage>() };
        let mut length = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        // SAFETY: storage and length are writable and descriptor is live for this non-mutating query.
        if unsafe { libc::getsockname(descriptor, &mut storage as *mut _ as *mut _, &mut length) } != 0
            || i32::from(storage.ss_family) != libc::AF_UNIX
        {
            return None;
        }
        let SocketAddress::Unix(path) = Self::decode_address(&storage, length).ok()? else {
            return None;
        };
        (!path.is_empty()).then_some(path)
    }

    pub(super) fn descriptor(&self, token: u64) -> Result<i32, RuntimeNetworkError> {
        self.shared
            .sockets
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&token)
            .map(|entry| entry.descriptor)
            .ok_or(RuntimeNetworkError::Invalid)
    }

    fn arm_read(&self, token: u64) {
        let mut sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(entry) = sockets.get_mut(&token) {
            entry.wants_read = true;
        }
        drop(sockets);
        self.wake();
    }

    fn socket_address(address: &SocketAddress) -> Result<(libc::sockaddr_storage, u32), RuntimeNetworkError> {
        // SAFETY: zero is a valid initialization for sockaddr storage and concrete sockaddr values.
        let mut storage = unsafe { zeroed::<libc::sockaddr_storage>() };
        match address {
            SocketAddress::Inet4 { address, port } => {
                // SAFETY: storage is aligned and large enough for sockaddr_in.
                let value = unsafe { &mut *(&mut storage as *mut _ as *mut libc::sockaddr_in) };
                value.sin_family = libc::AF_INET as _;
                value.sin_port = port.to_be();
                value.sin_addr.s_addr = u32::from_ne_bytes(*address);
                Ok((storage, size_of::<libc::sockaddr_in>() as u32))
            }
            SocketAddress::Inet6 { address, port, scope } => {
                // SAFETY: storage is aligned and large enough for sockaddr_in6.
                let value = unsafe { &mut *(&mut storage as *mut _ as *mut libc::sockaddr_in6) };
                value.sin6_family = libc::AF_INET6 as _;
                value.sin6_port = port.to_be();
                value.sin6_scope_id = *scope;
                value.sin6_addr.s6_addr = *address;
                Ok((storage, size_of::<libc::sockaddr_in6>() as u32))
            }
            SocketAddress::Unix(path) => {
                if path.len() > size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) {
                    return Err(RuntimeNetworkError::Invalid);
                }
                // SAFETY: storage is aligned and large enough for sockaddr_un.
                let value = unsafe { &mut *(&mut storage as *mut _ as *mut libc::sockaddr_un) };
                value.sun_family = libc::AF_UNIX as _;
                for (target, source) in value.sun_path.iter_mut().zip(path) {
                    *target = *source as libc::c_char;
                }
                let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len();
                Ok((storage, length as u32))
            }
        }
    }

    pub(super) fn decode_address(
        storage: &libc::sockaddr_storage,
        length: u32,
    ) -> Result<SocketAddress, RuntimeNetworkError> {
        match i32::from(storage.ss_family) {
            libc::AF_INET => {
                // SAFETY: family identifies initialized sockaddr_in storage.
                let value = unsafe { &*(storage as *const _ as *const libc::sockaddr_in) };
                Ok(SocketAddress::Inet4 {
                    address: value.sin_addr.s_addr.to_ne_bytes(),
                    port: u16::from_be(value.sin_port),
                })
            }
            libc::AF_INET6 => {
                // SAFETY: family identifies initialized sockaddr_in6 storage.
                let value = unsafe { &*(storage as *const _ as *const libc::sockaddr_in6) };
                Ok(SocketAddress::Inet6 {
                    address: value.sin6_addr.s6_addr,
                    port: u16::from_be(value.sin6_port),
                    scope: value.sin6_scope_id,
                })
            }
            libc::AF_UNIX => {
                // SAFETY: family identifies initialized sockaddr_un storage.
                let value = unsafe { &*(storage as *const _ as *const libc::sockaddr_un) };
                let available = (length as usize)
                    .saturating_sub(std::mem::offset_of!(libc::sockaddr_un, sun_path))
                    .min(value.sun_path.len());
                let mut bytes: Vec<u8> = value.sun_path[..available].iter().map(|byte| *byte as u8).collect();
                if bytes.first() != Some(&0) {
                    Self::trim_unix(&mut bytes);
                }
                Ok(SocketAddress::Unix(bytes))
            }
            _ => Err(RuntimeNetworkError::Invalid),
        }
    }

    fn trim_unix(bytes: &mut Vec<u8>) {
        let length = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
        bytes.truncate(length);
    }

    fn address_of(&self, token: u64, peer: bool) -> Result<SocketAddress, RuntimeNetworkError> {
        {
            let sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
            let projected = sockets.get(&token).and_then(|entry| {
                if peer {
                    entry.guest_peer.clone()
                } else {
                    entry.guest_local.clone()
                }
            });
            if let Some(address) = projected {
                return Ok(address);
            }
        }
        let descriptor = self.descriptor(token)?;
        // SAFETY: zero is a valid sockaddr_storage initialization.
        let mut storage = unsafe { zeroed::<libc::sockaddr_storage>() };
        let mut length = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        // SAFETY: pointers reference writable storage of the supplied length for the duration of the call.
        let result = unsafe {
            if peer {
                libc::getpeername(descriptor, &mut storage as *mut _ as *mut _, &mut length)
            } else {
                libc::getsockname(descriptor, &mut storage as *mut _ as *mut _, &mut length)
            }
        };
        if result == 0 {
            Self::decode_address(&storage, length)
        } else {
            Err(Self::runtime_error())
        }
    }

    fn switch_path(
        interface: &hl_network::EgressInterface,
        address: [u8; 4],
        port: u16,
    ) -> Result<(Vec<u8>, Vec<u8>), RuntimeNetworkError> {
        if interface.bridge.is_empty()
            || interface.bridge.len() > 40
            || interface
                .bridge
                .iter()
                .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(RuntimeNetworkError::Invalid);
        }
        let bridge = std::str::from_utf8(&interface.bridge).map_err(|_| RuntimeNetworkError::Invalid)?;
        let directory = format!("/tmp/.hl-bridge-{bridge}").into_bytes();
        let path = format!(
            "/tmp/.hl-bridge-{bridge}/{}.{}.{}.{}:{port}",
            address[0], address[1], address[2], address[3]
        )
        .into_bytes();
        if path.contains(&0)
            || path.len() >= size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path)
        {
            return Err(RuntimeNetworkError::Invalid);
        }
        Ok((directory, path))
    }

    fn mkdir_switch(directory: &[u8]) -> Result<(), RuntimeNetworkError> {
        let path = std::ffi::CString::new(directory).map_err(|_| RuntimeNetworkError::Invalid)?;
        // SAFETY: path is a live NUL-terminated pathname and mkdir retains no pointer.
        if unsafe { libc::mkdir(path.as_ptr(), 0o700) } == 0 {
            return Ok(());
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EEXIST) => Ok(()),
            _ => Err(Self::runtime_error()),
        }
    }

    fn socket_type(&self, token: u64) -> Result<i32, RuntimeNetworkError> {
        let descriptor = self.descriptor(token)?;
        let mut kind = 0_i32;
        let mut kind_length = size_of::<i32>() as libc::socklen_t;
        // SAFETY: kind is writable and the table retains the live descriptor.
        if unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut kind as *mut i32).cast(),
                &mut kind_length,
            )
        } == 0
        {
            Ok(kind)
        } else {
            Err(Self::runtime_error())
        }
    }

    fn switch_socket(&self, token: u64, expected: i32) -> Result<i32, RuntimeNetworkError> {
        let mut sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        if entry.switched {
            return Ok(entry.descriptor);
        }
        let mut kind = 0_i32;
        let mut kind_length = size_of::<i32>() as libc::socklen_t;
        // SAFETY: kind is writable and entry retains the live descriptor throughout the call.
        if unsafe {
            libc::getsockopt(
                entry.descriptor,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut kind as *mut i32).cast(),
                &mut kind_length,
            )
        } != 0
        {
            return Err(Self::runtime_error());
        }
        if kind != expected {
            return Err(RuntimeNetworkError::OperationNotSupported);
        }
        // SAFETY: socket returns a newly owned descriptor or a negative errno result.
        let replacement = unsafe { libc::socket(libc::AF_UNIX, expected, 0) };
        if replacement < 0 {
            return Err(Self::runtime_error());
        }
        // SAFETY: fcntl observes flags on live owned descriptors; dup2 atomically replaces entry.descriptor.
        let flags = unsafe { libc::fcntl(entry.descriptor, libc::F_GETFL) };
        let descriptor_flags = unsafe { libc::fcntl(entry.descriptor, libc::F_GETFD) };
        let replaced = unsafe { libc::dup2(replacement, entry.descriptor) };
        // SAFETY: replacement remains solely owned here regardless of dup2's result.
        unsafe { libc::close(replacement) };
        if replaced < 0 {
            return Err(Self::runtime_error());
        }
        // SAFETY: entry.descriptor is the newly installed socket and remains table-owned.
        unsafe {
            if flags >= 0 {
                libc::fcntl(entry.descriptor, libc::F_SETFL, flags);
            }
            if descriptor_flags >= 0 {
                libc::fcntl(entry.descriptor, libc::F_SETFD, descriptor_flags);
            }
        }
        entry.switched = true;
        let descriptor = entry.descriptor;
        drop(sockets);
        self.wake();
        Ok(descriptor)
    }

    fn reset_switch_socket(&self, token: u64, expected: i32) -> Result<(), RuntimeNetworkError> {
        let mut sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        if !entry.switched {
            return Err(RuntimeNetworkError::Invalid);
        }
        // SAFETY: socket returns a newly owned descriptor or a negative errno result.
        let replacement = unsafe { libc::socket(libc::AF_UNIX, expected, 0) };
        if replacement < 0 {
            return Err(Self::runtime_error());
        }
        // SAFETY: fcntl observes live descriptor flags and dup2 atomically replaces the table-owned socket.
        let flags = unsafe { libc::fcntl(entry.descriptor, libc::F_GETFL) };
        let descriptor_flags = unsafe { libc::fcntl(entry.descriptor, libc::F_GETFD) };
        let replaced = unsafe { libc::dup2(replacement, entry.descriptor) };
        // SAFETY: replacement is solely owned here after dup2 duplicates it when successful.
        unsafe { libc::close(replacement) };
        if replaced < 0 {
            return Err(Self::runtime_error());
        }
        // SAFETY: entry.descriptor is the newly installed socket and remains table-owned.
        unsafe {
            if flags >= 0 {
                libc::fcntl(entry.descriptor, libc::F_SETFL, flags);
            }
            if descriptor_flags >= 0 {
                libc::fcntl(entry.descriptor, libc::F_SETFD, descriptor_flags);
            }
        }
        drop(sockets);
        self.wake();
        Ok(())
    }

    fn duplicate_descriptor(&self, token: u64) -> Result<i32, RuntimeNetworkError> {
        let sockets = self.shared.sockets.lock().unwrap_or_else(|error| error.into_inner());
        let descriptor = sockets.get(&token).ok_or(RuntimeNetworkError::Invalid)?.descriptor;
        // SAFETY: descriptor remains live under the table lock and dup returns independent ownership.
        let duplicate = unsafe { libc::dup(descriptor) };
        if duplicate < 0 {
            Err(Self::runtime_error())
        } else {
            Ok(duplicate)
        }
    }

    fn switch_source(path: &[u8]) -> Option<SocketAddress> {
        let name = path.rsplit(|byte| *byte == b'/').next()?;
        let colon = name.iter().rposition(|byte| *byte == b':')?;
        let address = &name[..colon];
        let port = std::str::from_utf8(&name[colon + 1..]).ok()?.parse().ok()?;
        let mut ipv4 = [0_u8; 4];
        let mut octets = address.split(|byte| *byte == b'.');
        for octet in &mut ipv4 {
            *octet = std::str::from_utf8(octets.next()?).ok()?.parse().ok()?;
        }
        if octets.next().is_some() {
            return None;
        }
        Some(SocketAddress::Inet4 { address: ipv4, port })
    }

    fn binding(&self, address: &SocketAddress) -> Option<SocketAddress> {
        self.shared
            .bindings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find(|(guest, _)| match (guest, address) {
                (
                    SocketAddress::Inet4 {
                        address: bound,
                        port: bound_port,
                    },
                    SocketAddress::Inet4 {
                        address: target,
                        port: target_port,
                    },
                ) => bound_port == target_port && (bound == target || *bound == [0; 4]),
                _ => guest == address,
            })
            .map(|(_, host)| host.clone())
    }

    pub(super) fn runtime_error() -> RuntimeNetworkError {
        match std::io::Error::last_os_error().raw_os_error().unwrap_or(0) {
            value if value == libc::EAGAIN || value == libc::EWOULDBLOCK => RuntimeNetworkError::WouldBlock,
            libc::EINTR => RuntimeNetworkError::Interrupted,
            libc::EINPROGRESS => RuntimeNetworkError::InProgress,
            libc::EALREADY => RuntimeNetworkError::AlreadyPending,
            libc::EISCONN => RuntimeNetworkError::AlreadyConnected,
            libc::EADDRINUSE => RuntimeNetworkError::AddressInUse,
            libc::EADDRNOTAVAIL => RuntimeNetworkError::AddressNotAvailable,
            libc::ENOTCONN => RuntimeNetworkError::NotConnected,
            libc::ECONNREFUSED => RuntimeNetworkError::Refused,
            libc::ECONNRESET => RuntimeNetworkError::ConnectionReset,
            libc::ECONNABORTED => RuntimeNetworkError::ConnectionAborted,
            libc::EDESTADDRREQ => RuntimeNetworkError::DestinationRequired,
            libc::EMSGSIZE => RuntimeNetworkError::MessageTooLarge,
            libc::EAFNOSUPPORT => RuntimeNetworkError::FamilyNotSupported,
            libc::EPROTONOSUPPORT => RuntimeNetworkError::ProtocolNotSupported,
            libc::ESOCKTNOSUPPORT => RuntimeNetworkError::TypeNotSupported,
            libc::ENOPROTOOPT => RuntimeNetworkError::OptionNotSupported,
            libc::EPROTOTYPE => RuntimeNetworkError::WrongProtocol,
            libc::ENOTSOCK => RuntimeNetworkError::NotSocket,
            libc::EHOSTUNREACH => RuntimeNetworkError::HostUnreachable,
            libc::ENETUNREACH => RuntimeNetworkError::NetworkUnreachable,
            libc::ENETDOWN => RuntimeNetworkError::NetworkDown,
            libc::ENETRESET => RuntimeNetworkError::NetworkReset,
            libc::ESHUTDOWN => RuntimeNetworkError::ShutDown,
            libc::EPIPE => RuntimeNetworkError::BrokenPipe,
            libc::EOPNOTSUPP => RuntimeNetworkError::OperationNotSupported,
            libc::ETIMEDOUT => RuntimeNetworkError::TimedOut,
            libc::EACCES | libc::EPERM => RuntimeNetworkError::Permission,
            libc::ENOMEM | libc::ENOBUFS => RuntimeNetworkError::NoMemory,
            libc::EINVAL => RuntimeNetworkError::Invalid,
            _ => RuntimeNetworkError::Failed,
        }
    }

    fn host_error() -> SocketHostError {
        match std::io::Error::last_os_error().raw_os_error().unwrap_or(0) {
            value if value == libc::EAGAIN || value == libc::EWOULDBLOCK => SocketHostError::WouldBlock,
            libc::EINTR => SocketHostError::Interrupted,
            libc::EPIPE => SocketHostError::BrokenPipe,
            libc::EDESTADDRREQ => SocketHostError::DestinationRequired,
            libc::EMSGSIZE => SocketHostError::MessageTooLarge,
            libc::ECONNRESET => SocketHostError::ConnectionReset,
            libc::ECONNABORTED => SocketHostError::ConnectionAborted,
            libc::ENOTCONN => SocketHostError::NotConnected,
            libc::ESHUTDOWN => SocketHostError::ShutDown,
            libc::EHOSTUNREACH => SocketHostError::HostUnreachable,
            libc::ENETUNREACH => SocketHostError::NetworkUnreachable,
            libc::ENETDOWN => SocketHostError::NetworkDown,
            libc::ENETRESET => SocketHostError::NetworkReset,
            _ => SocketHostError::Io,
        }
    }

    fn connect_error(error: i32) -> SocketConnectStatus {
        match error {
            0 => SocketConnectStatus::Connected,
            libc::EINPROGRESS | libc::EALREADY => SocketConnectStatus::Pending,
            libc::ECONNREFUSED => SocketConnectStatus::Failed(SocketConnectError::Refused),
            libc::ETIMEDOUT => SocketConnectStatus::Failed(SocketConnectError::TimedOut),
            _ => SocketConnectStatus::Failed(SocketConnectError::Io),
        }
    }

    fn wake(&self) {
        let byte = [1_u8];
        // SAFETY: wake_write remains owned by Shared and byte is readable for one byte.
        unsafe {
            libc::write(self.shared.wake_write, byte.as_ptr().cast(), 1);
        }
    }

    pub(super) fn notify(&self, token: u64) {
        let observer = self
            .shared
            .observers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&token)
            .and_then(Weak::upgrade);
        if let Some(observer) = observer {
            observer.readiness_changed();
        }
        self.wake();
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        // Wake the reactor before this owner's Arc is released. Once the poll returns,
        // its Weak upgrade fails and the thread exits without an idle polling loop.
        self.wake();
    }
}

impl Drop for Reactor {
    fn drop(&mut self) {
        // SAFETY: Reactor owns both pipe descriptors and drops after the thread releases its last upgrade.
        unsafe {
            libc::close(self.wake_read);
            libc::close(self.wake_write);
        }
    }
}
