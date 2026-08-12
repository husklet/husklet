#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use hl_descriptor::ReadinessObserver;
use hl_network::{SocketAddress, SocketConnectError, SocketConnectStatus, SocketHostError};
use hl_runtime::RuntimeNetworkError;

mod address;
mod connect;
mod icmp;
mod io;
mod message;
pub(super) mod publish;
mod resolver;
mod runtime;
mod socket;
mod switch;

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
    /// Bridge rendezvous and guest peer to retry a refused loopback connect against.
    pub(super) loopback_switch: Option<(Vec<u8>, SocketAddress)>,
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
    pub(super) publications: Mutex<Vec<publish::Publication>>,
    /// Host ports whose forwarder is live, so a re-listen cannot open a second one.
    pub(super) forwarded: Mutex<Vec<u16>>,
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
            publications: Mutex::new(Vec::new()),
            forwarded: Mutex::new(Vec::new()),
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
                loopback_switch: None,
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
