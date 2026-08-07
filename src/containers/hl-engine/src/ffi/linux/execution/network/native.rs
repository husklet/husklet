#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    pub(super) kind: Option<i32>,
    pub(super) original_family: Option<i32>,
    pub(super) original_protocol: Option<i32>,
    pub(super) options: BTreeMap<(i32, i32), hl_linux::GuestSocketOption>,
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

impl Entry {
    fn arm_read(&mut self) -> bool {
        arm_interest(&mut self.wants_read)
    }

    pub(super) fn arm_write(&mut self) -> bool {
        arm_interest(&mut self.wants_write)
    }
}

fn arm_interest(armed: &mut bool) -> bool {
    let changed = !*armed;
    *armed = true;
    changed
}

/// The rendezvous pathnames this socket owns until its final descriptor closes.
pub(super) struct SwitchPath {
    names: Vec<Vec<u8>>,
    _publication: hl_fs::Publication,
}

impl SwitchPath {
    fn new(publication: hl_fs::Publication) -> Self {
        Self {
            names: publication.paths().map(<[u8]>::to_vec).collect(),
            _publication: publication,
        }
    }

    pub(super) fn names(&self) -> &[Vec<u8>] {
        &self.names
    }
}

pub(crate) struct Native {
    shared: Arc<Reactor>,
    pub(super) authority: Option<Arc<Mutex<crate::native::AuthorityWorker>>>,
}

pub(super) struct Reactor {
    pub(super) started: AtomicBool,
    pub(super) next: AtomicU64,
    pub(super) sockets: Mutex<BTreeMap<u64, Entry>>,
    pub(super) observers: Mutex<BTreeMap<u64, Weak<dyn ReadinessObserver>>>,
    pub(super) bindings: Mutex<Vec<(SocketAddress, SocketAddress)>>,
    pub(super) switch_paths: Mutex<BTreeMap<Vec<u8>, Weak<SwitchPath>>>,
    pub(super) wake: i32,
    #[cfg(test)]
    pub(super) wake_writes: AtomicU64,
    #[cfg(test)]
    pub(super) pollset_builds: AtomicU64,
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
        // SAFETY: eventfd2 takes scalar arguments only and returns one owned descriptor.
        // Worker confinement admits exactly this call shape, unlike pipe plus fcntl.
        let wake = unsafe { libc::syscall(libc::SYS_eventfd2, 0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) } as i32;
        assert!(wake >= 0, "native network wake eventfd creation failed");
        let shared = Arc::new(Reactor {
            started: AtomicBool::new(false),
            next: AtomicU64::new(1),
            sockets: Mutex::new(BTreeMap::new()),
            observers: Mutex::new(BTreeMap::new()),
            bindings: Mutex::new(Vec::new()),
            switch_paths: Mutex::new(BTreeMap::new()),
            wake,
            #[cfg(test)]
            wake_writes: AtomicU64::new(0),
            #[cfg(test)]
            pollset_builds: AtomicU64::new(0),
        });
        Self { shared, authority }
    }

    pub(super) fn insert(&self, descriptor: i32) -> Result<u64, RuntimeNetworkError> {
        let kind = Self::descriptor_type(descriptor).ok();
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&path)
                .and_then(Weak::upgrade)
        });
        sockets.insert(
            token,
            Entry {
                descriptor,
                kind,
                original_family: None,
                original_protocol: None,
                options: BTreeMap::new(),
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
        Reactor::start(&self.shared);
        self.wake();
        Ok(token)
    }

    fn descriptor_unix_path(descriptor: i32) -> Option<Vec<u8>> {
        // SAFETY: zero is valid sockaddr_storage initialization.
        let mut storage = unsafe { zeroed::<libc::sockaddr_storage>() };
        let mut length = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        // SAFETY: storage and length are writable and descriptor is live for this non-mutating query.
        if unsafe { libc::getsockname(descriptor, (&raw mut storage).cast(), &raw mut length) } != 0
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&token)
            .map(|entry| entry.descriptor)
            .ok_or(RuntimeNetworkError::Invalid)
    }

    fn arm_read(&self, token: u64) {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = sockets.get_mut(&token).is_some_and(Entry::arm_read);
        drop(sockets);
        if changed {
            self.wake();
        }
    }

    fn socket_address(address: &SocketAddress) -> Result<(libc::sockaddr_storage, u32), RuntimeNetworkError> {
        // SAFETY: zero is a valid initialization for sockaddr storage and concrete sockaddr values.
        let mut storage = unsafe { zeroed::<libc::sockaddr_storage>() };
        match address {
            SocketAddress::Inet4 { address, port } => {
                // SAFETY: storage is aligned and large enough for sockaddr_in.
                let value = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_in>() };
                value.sin_family = libc::AF_INET as _;
                value.sin_port = port.to_be();
                value.sin_addr.s_addr = u32::from_ne_bytes(*address);
                Ok((storage, size_of::<libc::sockaddr_in>() as u32))
            }
            SocketAddress::Inet6 { address, port, scope } => {
                // SAFETY: storage is aligned and large enough for sockaddr_in6.
                let value = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_in6>() };
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
                let value = unsafe { &mut *(&raw mut storage).cast::<libc::sockaddr_un>() };
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
                let value = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_in>() };
                Ok(SocketAddress::Inet4 {
                    address: value.sin_addr.s_addr.to_ne_bytes(),
                    port: u16::from_be(value.sin_port),
                })
            }
            libc::AF_INET6 => {
                // SAFETY: family identifies initialized sockaddr_in6 storage.
                let value = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_in6>() };
                Ok(SocketAddress::Inet6 {
                    address: value.sin6_addr.s6_addr,
                    port: u16::from_be(value.sin6_port),
                    scope: value.sin6_scope_id,
                })
            }
            libc::AF_UNIX => {
                // SAFETY: family identifies initialized sockaddr_un storage.
                let value = unsafe { &*std::ptr::from_ref(storage).cast::<libc::sockaddr_un>() };
                let available = (length as usize)
                    .saturating_sub(std::mem::offset_of!(libc::sockaddr_un, sun_path))
                    .min(value.sun_path.len());
                let mut bytes: Vec<u8> = value.sun_path[..available].to_vec();
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
            let sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                libc::getpeername(descriptor, (&raw mut storage).cast(), &raw mut length)
            } else {
                libc::getsockname(descriptor, (&raw mut storage).cast(), &raw mut length)
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
        let bridge = Self::switch_bridge(interface)?;
        let directory = format!("/tmp/.hl-bridge-{bridge}").into_bytes();
        let path = Self::switch_destination_path_for_bridge(bridge, address, port)?;
        Ok((directory, path))
    }

    fn switch_destination_path(
        interface: &hl_network::EgressInterface,
        address: [u8; 4],
        port: u16,
    ) -> Result<Vec<u8>, RuntimeNetworkError> {
        let bridge = Self::switch_bridge(interface)?;
        Self::switch_destination_path_for_bridge(bridge, address, port)
    }

    fn switch_bridge(interface: &hl_network::EgressInterface) -> Result<&str, RuntimeNetworkError> {
        if interface.bridge.is_empty()
            || interface.bridge.len() > 40
            || interface
                .bridge
                .iter()
                .any(|byte| !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(RuntimeNetworkError::Invalid);
        }
        std::str::from_utf8(&interface.bridge).map_err(|_| RuntimeNetworkError::Invalid)
    }

    fn switch_destination_path_for_bridge(
        bridge: &str,
        address: [u8; 4],
        port: u16,
    ) -> Result<Vec<u8>, RuntimeNetworkError> {
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
        Ok(path)
    }

    /// Holds this interface's switch directory and returns it with the rendezvous pathname it owns.
    fn switch_reservation(
        interface: &hl_network::EgressInterface,
        port: u16,
        ipv6_only: bool,
    ) -> Result<(Arc<hl_fs::Anchor>, Vec<u8>), RuntimeNetworkError> {
        let (directory, mut path) = Self::switch_path(interface, interface.ipv4, port)?;
        if ipv6_only {
            path.extend_from_slice(b".v6only");
        }
        Ok((Self::switch_anchor(&directory)?, path))
    }

    /// Creates the switch directory when absent and holds it, so a later rename of its pathname
    /// cannot redirect any operation on the rendezvous names inside it.
    fn switch_anchor(directory: &[u8]) -> Result<Arc<hl_fs::Anchor>, RuntimeNetworkError> {
        use std::os::unix::ffi::OsStrExt;
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(directory));
        hl_fs::Anchor::create(path, 0o700).map_err(Self::publication_error)
    }

    fn switch_name(path: &[u8]) -> Result<&[u8], RuntimeNetworkError> {
        let separator = path
            .iter()
            .rposition(|byte| *byte == b'/')
            .ok_or(RuntimeNetworkError::Invalid)?;
        let name = &path[separator + 1..];
        if name.is_empty() {
            return Err(RuntimeNetworkError::Invalid);
        }
        Ok(name)
    }

    fn publication_error(error: std::io::Error) -> RuntimeNetworkError {
        match error.raw_os_error() {
            Some(code) => Self::error_for(code),
            None => RuntimeNetworkError::Failed,
        }
    }

    fn socket_type(&self, token: u64) -> Result<i32, RuntimeNetworkError> {
        let descriptor = {
            let sockets = self
                .shared
                .sockets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = sockets.get(&token).ok_or(RuntimeNetworkError::Invalid)?;
            if let Some(kind) = entry.kind {
                return Ok(kind);
            }
            entry.descriptor
        };
        let kind = Self::descriptor_type(descriptor)?;
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        entry.kind = Some(kind);
        Ok(kind)
    }

    fn descriptor_type(descriptor: i32) -> Result<i32, RuntimeNetworkError> {
        let mut kind = 0_i32;
        let mut kind_length = size_of::<i32>() as libc::socklen_t;
        // SAFETY: kind is writable and the table retains the live descriptor.
        if unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut kind_length,
            )
        } == 0
        {
            Ok(kind)
        } else {
            Err(Self::runtime_error())
        }
    }

    fn switch_socket(&self, token: u64, expected: i32) -> Result<i32, RuntimeNetworkError> {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        if entry.switched {
            return Ok(entry.descriptor);
        }
        let kind = match entry.kind {
            Some(kind) => kind,
            None => Self::descriptor_type(entry.descriptor)?,
        };
        if kind != expected {
            return Err(RuntimeNetworkError::OperationNotSupported);
        }
        entry.kind = Some(kind);
        Self::install_replacement(entry, libc::AF_UNIX, expected, 0, true)?;
        entry.switched = true;
        let descriptor = entry.descriptor;
        drop(sockets);
        self.wake();
        Ok(descriptor)
    }

    fn reset_switch_socket(&self, token: u64, expected: i32) -> Result<(), RuntimeNetworkError> {
        self.replace_socket(token, expected, libc::AF_UNIX, true)
    }

    fn restore_inet_socket(&self, token: u64, expected: i32) -> Result<(), RuntimeNetworkError> {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        if !entry.switched {
            return Err(RuntimeNetworkError::Invalid);
        }
        let family = entry.original_family.unwrap_or(libc::AF_INET);
        let protocol = entry.original_protocol.unwrap_or(0);
        Self::install_replacement(entry, family, expected, protocol, false)?;
        entry.guest_local = None;
        entry.guest_peer = None;
        entry.switch_path = None;
        entry.switch_interface = None;
        entry.datagram_peer = None;
        entry.switched = false;
        drop(sockets);
        self.wake();
        Ok(())
    }

    fn replace_socket(
        &self,
        token: u64,
        expected: i32,
        family: i32,
        switched: bool,
    ) -> Result<(), RuntimeNetworkError> {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        if !entry.switched {
            return Err(RuntimeNetworkError::Invalid);
        }
        Self::install_replacement(entry, family, expected, 0, switched)?;
        if !switched {
            entry.guest_local = None;
            entry.guest_peer = None;
            entry.switch_path = None;
            entry.switch_interface = None;
            entry.datagram_peer = None;
        }
        entry.switched = switched;
        drop(sockets);
        self.wake();
        Ok(())
    }

    fn install_replacement(
        entry: &mut Entry,
        family: i32,
        kind: i32,
        protocol: i32,
        switch_transport: bool,
    ) -> Result<(), RuntimeNetworkError> {
        // SAFETY: socket returns a newly owned descriptor or a negative errno result.
        let replacement = unsafe { libc::socket(family, kind, protocol) };
        if replacement < 0 {
            return Err(Self::runtime_error());
        }
        // Snapshot table-owned state before replacement. Options are typed and
        // bounded at the Linux ABI boundary; no unbounded host buffer is retained.
        // SAFETY: entry.descriptor is owned by the locked socket table, so it stays open
        // across this call; F_GETFL takes no pointer argument and only reads fd flags.
        let flags = unsafe { libc::fcntl(entry.descriptor, libc::F_GETFL) };
        // SAFETY: same live table-owned descriptor under the same lock; F_GETFD is a
        // pointer-free query that cannot alias or free anything.
        let descriptor_flags = unsafe { libc::fcntl(entry.descriptor, libc::F_GETFD) };
        // Configure the unshared replacement completely before dup2. Failure
        // therefore leaves the original descriptor and projected state intact.
        // SAFETY: replacement is the descriptor just returned by socket() above and is
        // solely owned here; F_SETFL/F_SETFD pass only integer flags, no pointers.
        unsafe {
            if flags >= 0 {
                libc::fcntl(replacement, libc::F_SETFL, flags);
            }
            if descriptor_flags >= 0 {
                libc::fcntl(replacement, libc::F_SETFD, descriptor_flags);
            }
        }
        for ((level, option), value) in &entry.options {
            if (!switch_transport || Self::switch_option(*level, *option))
                && let Err(error) = super::socket_option::set(replacement, *level, *option, value.clone())
            {
                if switch_transport {
                    continue;
                }
                // SAFETY: replacement is still solely owned here.
                unsafe { libc::close(replacement) };
                return Err(error);
            }
        }
        // SAFETY: dup2 atomically replaces the table descriptor. replacement
        // remains solely owned here and is closed on both outcomes.
        let replaced = unsafe { libc::dup2(replacement, entry.descriptor) };
        unsafe { libc::close(replacement) };
        if replaced < 0 {
            return Err(Self::runtime_error());
        }
        Ok(())
    }

    const fn switch_option(level: i32, option: i32) -> bool {
        level == 1 && matches!(option, 2 | 6 | 9 | 10 | 13 | 15 | 20 | 21)
    }

    fn set_socket_option(
        &self,
        token: u64,
        level: i32,
        option: i32,
        value: hl_linux::GuestSocketOption,
    ) -> Result<(), RuntimeNetworkError> {
        let mut sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get_mut(&token).ok_or(RuntimeNetworkError::Invalid)?;
        super::socket_option::set(entry.descriptor, level, option, value.clone())?;
        if (level, option) == (1, 27) {
            entry.options.remove(&(1, 26));
        } else {
            entry.options.insert((level, option), value);
        }
        Ok(())
    }

    fn duplicate_descriptor(&self, token: u64) -> Result<i32, RuntimeNetworkError> {
        let sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        Self::error_for(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }

    pub(super) fn error_for(errno: i32) -> RuntimeNetworkError {
        match errno {
            value if value == libc::EAGAIN || value == libc::EWOULDBLOCK => RuntimeNetworkError::WouldBlock,
            libc::EINTR => RuntimeNetworkError::Interrupted,
            libc::EINPROGRESS => RuntimeNetworkError::InProgress,
            libc::EALREADY => RuntimeNetworkError::AlreadyPending,
            libc::EISCONN => RuntimeNetworkError::AlreadyConnected,
            libc::EADDRINUSE | libc::EEXIST => RuntimeNetworkError::AddressInUse,
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
        #[cfg(test)]
        self.shared.wake_writes.fetch_add(1, Ordering::Relaxed);
        let count = 1_u64.to_ne_bytes();
        // SAFETY: wake remains owned by Shared and count is readable for one eventfd counter.
        unsafe {
            libc::write(self.shared.wake, count.as_ptr().cast(), count.len());
        }
    }

    pub(super) fn notify(&self, token: u64) {
        let observer = self
            .shared
            .observers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        // SAFETY: Reactor owns the wake descriptor and drops after the thread releases its last upgrade.
        unsafe {
            libc::close(self.wake);
        }
    }
}

#[cfg(test)]
mod kind_cache_test {
    use super::{Native, arm_interest};
    use hl_network::SocketHostIo;
    use std::sync::atomic::Ordering;

    #[derive(Debug)]
    struct LoopbackMeasurement {
        kind: i32,
        iterations: u64,
        elapsed_ns: u128,
        wake_writes: u64,
        pollset_builds: u64,
        checksum: u64,
    }

    fn measure_loopback(kind: i32) -> LoopbackMeasurement {
        const ITERATIONS: u64 = 20_000;
        let host = Native::new();
        let mut descriptors = [-1_i32; 2];
        // SAFETY: descriptors names two writable integers and successful
        // socketpair transfers unique ownership of both descriptors.
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, kind, 0, descriptors.as_mut_ptr()) },
            0
        );
        let sender = host.insert(descriptors[0]).expect("insert sender");
        let receiver = host.insert(descriptors[1]).expect("insert receiver");
        std::thread::sleep(std::time::Duration::from_millis(10));
        let wake_before = host.shared.wake_writes.load(Ordering::Relaxed);
        let poll_before = host.shared.pollset_builds.load(Ordering::Relaxed);
        let input = [0x5a_u8; 64];
        let mut output = [0_u8; 64];
        let mut checksum = 0_u64;
        let started = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            assert_eq!(SocketHostIo::write(&host, sender, &input, false), Ok(input.len()));
            assert_eq!(
                SocketHostIo::read(&host, receiver, &mut output, false),
                Ok(output.len())
            );
            checksum = checksum.wrapping_add(u64::from(output[0]));
        }
        let elapsed_ns = started.elapsed().as_nanos();
        let measurement = LoopbackMeasurement {
            kind,
            iterations: ITERATIONS,
            elapsed_ns,
            wake_writes: host.shared.wake_writes.load(Ordering::Relaxed) - wake_before,
            pollset_builds: host.shared.pollset_builds.load(Ordering::Relaxed) - poll_before,
            checksum,
        };
        SocketHostIo::close(&host, sender);
        SocketHostIo::close(&host, receiver);
        measurement
    }

    #[test]
    fn readiness_interest_wakes_only_on_disarmed_to_armed_transition() {
        let mut armed = false;
        assert!(arm_interest(&mut armed));
        assert!(!arm_interest(&mut armed));
        armed = false;
        assert!(arm_interest(&mut armed));
    }

    #[test]
    #[ignore = "performance diagnostic; run pinned in the coordinated quiet window"]
    fn loopback_stream_and_datagram_data_path() {
        let stream = measure_loopback(libc::SOCK_STREAM);
        let datagram = measure_loopback(libc::SOCK_DGRAM);
        assert_eq!(stream.checksum, stream.iterations * 0x5a);
        assert_eq!(datagram.checksum, datagram.iterations * 0x5a);
        assert_eq!(stream.kind, libc::SOCK_STREAM);
        assert_eq!(datagram.kind, libc::SOCK_DGRAM);
        assert!(stream.elapsed_ns > 0 && datagram.elapsed_ns > 0);
        assert!(stream.wake_writes <= stream.iterations + 2);
        assert!(datagram.wake_writes <= datagram.iterations + 2);
        assert!(stream.pollset_builds > 0 && datagram.pollset_builds > 0);
        eprintln!("network-loopback stream={stream:?} datagram={datagram:?}");
    }

    #[test]
    fn readiness_worker_starts_with_first_socket() {
        let host = Native::new();
        assert!(!host.shared.started.load(Ordering::Acquire));
        // SAFETY: socket returns a newly owned descriptor which insert consumes.
        let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0) };
        assert!(descriptor >= 0);
        let token = host.insert(descriptor).unwrap();
        assert!(host.shared.started.load(Ordering::Acquire));
        hl_network::SocketHostIo::close(&host, token);
    }

    #[test]
    fn socket_kind_is_sampled_once_when_descriptor_enters_the_table() {
        let host = Native::new();
        // SAFETY: socket returns a newly owned descriptor which insert consumes.
        let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0) };
        assert!(descriptor >= 0);
        let token = host.insert(descriptor).unwrap();
        let sockets = host.shared.sockets.lock().unwrap();
        assert_eq!(sockets.get(&token).unwrap().kind, Some(libc::SOCK_DGRAM));
        drop(sockets);
        for _ in 0..1_024 {
            assert_eq!(host.socket_type(token), Ok(libc::SOCK_DGRAM));
        }
        hl_network::SocketHostIo::close(&host, token);
    }

    #[test]
    fn destination_only_path_matches_binding_path() {
        let interface = hl_network::EgressInterface {
            bridge: b"allocation-check".to_vec(),
            index: 2,
            ipv4: [10, 0, 0, 2],
        };
        let (_, binding_path) = Native::switch_path(&interface, [10, 0, 0, 9], 8080).unwrap();
        let destination_path = Native::switch_destination_path(&interface, [10, 0, 0, 9], 8080).unwrap();
        assert_eq!(destination_path, binding_path);
    }
}
