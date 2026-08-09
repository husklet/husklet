//! Host listeners for published container ports (`docker run -p HOST:CONTAINER`).
//!
//! A guest server binds the virtual switch, so nothing on the host owns an `AF_INET` socket for the
//! host port and a host client dialing it is refused. Each published port that the guest listens on
//! gets a real host listener here; every accepted connection dials the guest's switch rendezvous and
//! relays bytes both ways, exactly as the retained engine's `fwd_listen_thread` does. Container to
//! container traffic, egress and the switch itself are untouched: this only adds the inbound path.

use std::io::copy;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use hl_network::SocketAddress;

use super::{Native, Reactor};

/// One `[IP:]HOST:CONTAINER` publication rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Publication {
    pub(crate) address: [u8; 4],
    pub(crate) host: u16,
    pub(crate) guest: u16,
}

impl Publication {
    /// Parses the launch option's comma-separated `[IP:]HOST:CONTAINER` records. A malformed or
    /// zero-ported record names no reachable endpoint and is dropped rather than failing the launch.
    pub(crate) fn parse(records: &[u8]) -> Vec<Self> {
        std::str::from_utf8(records)
            .unwrap_or_default()
            .split(',')
            .filter(|record| !record.is_empty())
            .filter_map(Self::record)
            .collect()
    }

    fn record(record: &str) -> Option<Self> {
        let mut fields = record.rsplitn(3, ':');
        let guest = fields.next()?.parse().ok()?;
        let host = fields.next()?.parse().ok()?;
        let address = match fields.next() {
            Some(text) => {
                let parsed: std::net::Ipv4Addr = text.parse().ok()?;
                parsed.octets()
            }
            None => [0; 4],
        };
        if guest == 0 || host == 0 {
            return None;
        }
        Some(Self { address, host, guest })
    }
}

impl Native {
    /// Records the container's publication rules before any guest task runs.
    pub(crate) fn set_publications(&self, rules: Vec<Publication>) {
        *self
            .shared
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = rules;
    }

    /// Starts this listening socket's host forwarder when its guest port is published. A socket that
    /// bound a real host address instead of the switch is already reachable and is left alone.
    pub(super) fn start_publication(&self, token: u64) {
        let Some((path, port)) = self.publication_endpoint(token) else {
            return;
        };
        let Some(rule) = self
            .shared
            .publications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .copied()
            .find(|rule| rule.guest == port)
        else {
            return;
        };
        {
            // Marked before the thread starts so a re-listen cannot open a second listener; the
            // thread unmarks the port when its own bind loses the race.
            let mut started = self
                .shared
                .forwarded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if started.contains(&rule.host) {
                return;
            }
            started.push(rule.host);
        }
        // The thread holds only a weak reference, so the forwarder cannot outlive the container
        // whose port it publishes: this engine may host several sessions in one host process.
        let shared = Arc::downgrade(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("hl-publish".into())
            .spawn(move || listen(&shared, rule, &path));
        if spawned.is_err() {
            unmark(&self.shared, rule.host);
        }
    }

    fn publication_endpoint(&self, token: u64) -> Option<(Vec<u8>, u16)> {
        let sockets = self
            .shared
            .sockets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = sockets.get(&token)?;
        let path = entry.switch_path.as_ref()?.names().first()?.clone();
        match entry.guest_local {
            Some(SocketAddress::Inet4 { port, .. }) => Some((path, port)),
            _ => None,
        }
    }
}

fn unmark(shared: &Reactor, host: u16) {
    shared
        .forwarded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|port| *port != host);
}

/// Concurrent relays one published port will carry. A host client that opens connections and never
/// speaks would otherwise cost this process a thread each; past the ceiling a new connection is
/// dropped, which is what a real backlog overflow looks like to the client.
const RELAY_CEILING: usize = 256;

fn listen(shared: &Weak<Reactor>, rule: Publication, path: &[u8]) {
    let Some(listener) = claim(shared, rule) else {
        // The host port belongs to someone else; let a later listen() retry it.
        if let Some(shared) = shared.upgrade() {
            unmark(&shared, rule.host);
        }
        return;
    };
    let relays = Arc::new(AtomicUsize::new(0));
    while let Some(owner) = shared.upgrade() {
        drop(owner);
        match accept(&listener) {
            Accepted::Idle => continue,
            Accepted::Closed => break,
            Accepted::Connection(host) => serve(host, path, &relays),
        }
    }
    if let Some(shared) = shared.upgrade() {
        unmark(&shared, rule.host);
    }
}

/// Relays one accepted host connection. A relay cannot outlive its container: when the session ends
/// the guest's switch socket closes, the guest side reads EOF and both directions retire.
fn serve(host: TcpStream, path: &[u8], relays: &Arc<AtomicUsize>) {
    if relays.load(Ordering::Relaxed) >= RELAY_CEILING {
        return;
    }
    let Some(guest) = dial(path) else {
        return;
    };
    relays.fetch_add(1, Ordering::Relaxed);
    let counted = Arc::clone(relays);
    if std::thread::Builder::new()
        .name("hl-publish-relay".into())
        .spawn(move || {
            relay(host, guest);
            counted.fetch_sub(1, Ordering::Relaxed);
        })
        .is_err()
    {
        relays.fetch_sub(1, Ordering::Relaxed);
    }
}

enum Accepted {
    Connection(TcpStream),
    Idle,
    Closed,
}

/// Waits briefly for one host connection. The bounded wait is what lets the loop notice that the
/// container it forwards for is gone instead of blocking in accept forever.
///
/// The retained engine leaves its accept loop on any error but `EINTR`, which it can afford because
/// that forwarder dies with its guest process anyway. This one is the container's only inbound path
/// for its whole life, so retiring it on a peer that reset mid-handshake, or on momentary descriptor
/// or memory pressure, would silently strand the published port with no error anywhere.
fn accept(listener: &TcpListener) -> Accepted {
    let mut poller = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd is passed with a matching count.
    match unsafe { libc::poll(&raw mut poller, 1, 250) } {
        ready if ready < 0 => return classify(&std::io::Error::last_os_error()),
        // The wait expired with nothing pending; returning here is what bounds the step. Falling
        // into accept() would park this thread on a blocking listener and keep the host port bound
        // long after the container that published it is gone.
        0 => return Accepted::Idle,
        _ => {}
    }
    match listener.accept() {
        Ok((stream, _)) => Accepted::Connection(stream),
        Err(error) => classify(&error),
    }
}

/// Transient failures keep the listener; only a genuinely broken listening socket retires it.
fn classify(error: &std::io::Error) -> Accepted {
    let transient = matches!(
        error.kind(),
        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock | std::io::ErrorKind::ConnectionAborted
    ) || matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM | libc::EPROTO)
    );
    if !transient {
        return Accepted::Closed;
    }
    if error.kind() != std::io::ErrorKind::Interrupted {
        // Resource pressure leaves the connection pending, so poll would report it ready again at
        // once; pause instead of spinning on a descriptor table that is momentarily full.
        std::thread::sleep(Duration::from_millis(20));
    }
    Accepted::Idle
}

/// Takes the host port, waiting out the previous owner. Replacing a container that published the
/// same port is ordinary: the daemon frees the port the instant the old container leaves the active
/// set, while its forwarder still needs its current accept step to observe that its session is gone.
fn claim(shared: &Weak<Reactor>, rule: Publication) -> Option<TcpListener> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        shared.upgrade()?;
        if let Some(listener) = bind(rule) {
            return Some(listener);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Binds the published host address with `SO_REUSEADDR`, so the next container to publish the port
/// is not blocked by connections the previous one left in `TIME_WAIT`.
fn bind(rule: Publication) -> Option<TcpListener> {
    // SAFETY: socket takes scalar arguments and returns one owned descriptor.
    // Non-blocking, so a peer that resets between poll() and accept() cannot park the loop either.
    let descriptor = unsafe {
        libc::socket(
            libc::AF_INET,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if descriptor < 0 {
        return None;
    }
    // SAFETY: descriptor is owned here and the listener adopts it on every path below.
    let listener = unsafe { TcpListener::from_raw_fd(descriptor) };
    let reuse: libc::c_int = 1;
    let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    address.sin_family = libc::AF_INET as libc::sa_family_t;
    address.sin_port = rule.host.to_be();
    address.sin_addr.s_addr = u32::from_ne_bytes(rule.address);
    // SAFETY: both option and address storage match the lengths passed with them.
    let bound = unsafe {
        libc::setsockopt(
            descriptor,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            (&raw const reuse).cast(),
            size_of::<libc::c_int>() as libc::socklen_t,
        ) == 0
            && libc::bind(
                descriptor,
                (&raw const address).cast(),
                size_of::<libc::sockaddr_in>() as libc::socklen_t,
            ) == 0
            && libc::listen(descriptor, 128) == 0
    };
    bound.then_some(listener)
}

/// Dials the guest's switch rendezvous, tolerating a re-listen gap: a server looping `nc -l -w N`
/// unbinds and rebinds the inode between connections, so a dial landing in that window sees ENOENT
/// or ECONNREFUSED, and a listener whose accept window just closed hangs up with nothing to serve.
/// Retrying for ~600ms mirrors TCP SYN retransmission; a genuinely dead guest still fails.
fn dial(path: &[u8]) -> Option<UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::path::Path::new(std::ffi::OsStr::from_bytes(path));
    let deadline = Instant::now() + Duration::from_millis(600);
    loop {
        if let Ok(stream) = UnixStream::connect(name)
            && !dead_on_arrival(&stream)
        {
            return Some(stream);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A peer that hangs up without data is mid-exit. Only POLLHUP or POLLERR without POLLIN is dead:
/// an idle but live socket is a server waiting for the client's request.
fn dead_on_arrival(stream: &UnixStream) -> bool {
    let mut poller = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd is passed with a matching count.
    if unsafe { libc::poll(&raw mut poller, 1, 40) } <= 0 {
        return false;
    }
    if poller.revents & libc::POLLIN == 0 {
        return true;
    }
    let mut byte = 0_u8;
    // SAFETY: the peek reads at most one byte into an owned local and consumes nothing.
    unsafe { libc::recv(poller.fd, (&raw mut byte).cast(), 1, libc::MSG_PEEK) == 0 }
}

/// Pumps both directions until each side has closed, half-closing the peer on EOF so a server that
/// replies after reading the whole request still gets its bytes through.
fn relay(host: TcpStream, guest: UnixStream) {
    let (Ok(mut host_reader), Ok(mut guest_writer)) = (host.try_clone(), guest.try_clone()) else {
        return;
    };
    let inbound = std::thread::Builder::new().name("hl-publish-in".into()).spawn(move || {
        if let Err(error) = copy(&mut host_reader, &mut guest_writer) {
            hl_log::hl_debug!(hl_log::tag::NET, "published relay inbound ended early: {error}");
        }
        let _ = guest_writer.shutdown(Shutdown::Write);
    });
    let (mut guest_reader, mut host_writer) = (guest, host);
    if let Err(error) = copy(&mut guest_reader, &mut host_writer) {
        hl_log::hl_debug!(hl_log::tag::NET, "published relay outbound ended early: {error}");
    }
    let _ = host_writer.shutdown(Shutdown::Write);
    if let Ok(inbound) = inbound {
        let _ = inbound.join();
    }
}

#[cfg(test)]
mod tests {
    use super::{Accepted, Publication, accept, bind, classify};

    #[test]
    fn an_idle_listener_yields_the_accept_loop_instead_of_parking_it() {
        let reservation = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let host = reservation.local_addr().unwrap().port();
        drop(reservation);
        let listener = bind(Publication {
            address: [127, 0, 0, 1],
            host,
            guest: 1,
        })
        .unwrap();
        let started = std::time::Instant::now();
        assert!(matches!(accept(&listener), Accepted::Idle));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "accept did not return"
        );
    }

    #[test]
    fn a_reset_peer_and_resource_pressure_keep_the_listener() {
        for code in [
            libc::ECONNABORTED,
            libc::EMFILE,
            libc::ENFILE,
            libc::ENOBUFS,
            libc::ENOMEM,
            libc::EINTR,
        ] {
            assert!(
                matches!(classify(&std::io::Error::from_raw_os_error(code)), Accepted::Idle),
                "errno {code} retired the published port"
            );
        }
    }

    #[test]
    fn a_broken_listening_socket_retires_the_forwarder() {
        assert!(matches!(
            classify(&std::io::Error::from_raw_os_error(libc::EBADF)),
            Accepted::Closed
        ));
    }

    #[test]
    fn parses_addressed_and_bare_publication_records() {
        let rules = Publication::parse(b"127.0.0.1:8080:80,9090:90");
        assert_eq!(
            rules,
            vec![
                Publication {
                    address: [127, 0, 0, 1],
                    host: 8_080,
                    guest: 80
                },
                Publication {
                    address: [0; 4],
                    host: 9_090,
                    guest: 90
                }
            ]
        );
    }

    #[test]
    fn unreachable_and_malformed_records_are_dropped() {
        assert!(Publication::parse(b"0:80,8080:0,nonsense,8080").is_empty());
    }
}
