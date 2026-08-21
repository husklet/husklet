//! Connection-identity behaviour at the `AF_UNIX` boundary.
//!
//! The raw calls that remain are the three the engine's contract depends on: a socket that exists but
//! is not yet named or connected, the `sockaddr_un` that names it -- including the abstract, NUL-leading
//! form no `std` address type can express -- and the two syscalls that apply one to a descriptor that
//! already exists. `UnixStream::connect` and `UnixListener::bind` mint their own socket, and every test
//! here reserves an identity ON the descriptor before it dials, so that ordering is not theirs to take.
//! Everything downstream of the connect -- accepting, duplicating, reading, writing -- is safe `std`.

#![allow(unsafe_code)]

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::mem::{offset_of, size_of};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
const PREPARE: u32 = 0;
#[cfg(unix)]
const IDENTIFY: u32 = 1;
#[cfg(unix)]
const FAILED: u32 = 2;
#[cfg(unix)]
const RESET: u32 = 3;
#[cfg(unix)]
const PRIVATE_PREPARE: u32 = 4;
#[cfg(unix)]
const IN_PROGRESS: u32 = 5;
#[cfg(unix)]
const ASYNC_FAILED: u32 = 6;
#[cfg(unix)]
const CHECKPOINT_ADMIT: u32 = 8;
#[cfg(unix)]
const INITIALIZE_ALIAS: u32 = 9;
#[cfg(unix)]
const SNAPSHOT: u32 = 10;
#[cfg(unix)]
const INITIALIZE_PAIR_END: u32 = 11;
#[cfg(unix)]
const SHUTDOWN_READ: u32 = 12;
#[cfg(unix)]
const SHUTDOWN_WRITE: u32 = 13;
#[cfg(unix)]
const SHUTDOWN_BOTH: u32 = 14;
#[cfg(unix)]
const CONNECTED: u32 = 7;
#[cfg(unix)]
const PREPARE_MINTED: u32 = 15;
#[cfg(unix)]
const IDENTIFY_MINTED: u32 = 16;

#[cfg(unix)]
const LOCAL_HIDDEN: u32 = 1;
#[cfg(unix)]
const PEER_HIDDEN: u32 = 2;
#[cfg(unix)]
const RECIPROCITY_REQUIRED: u32 = 4;
#[cfg(unix)]
const CONNECTING: u32 = 8;
#[cfg(unix)]
const READ_CLOSED: u32 = 32;
#[cfg(unix)]
const WRITE_CLOSED: u32 = 64;

/// An `AF_UNIX` stream socket that exists and is not yet named, connected, or listening.
#[cfg(unix)]
fn socket() -> UnixStream {
    // SAFETY: `socket(2)` reads no caller memory and returns either a descriptor this process is the sole
    // owner of or -1, which the assertion rejects before the integer reaches `from_raw_fd`. Ownership
    // then moves straight into `UnixStream`, which closes it exactly once; no other binding keeps a copy.
    let descriptor = unsafe {
        let descriptor = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        assert!(descriptor >= 0, "socket: {}", std::io::Error::last_os_error());
        OwnedFd::from_raw_fd(descriptor)
    };
    UnixStream::from(descriptor)
}

#[cfg(unix)]
fn address(name: &[u8], abstract_name: bool) -> (libc::sockaddr_un, libc::socklen_t) {
    // SAFETY: `sockaddr_un` is a C aggregate of an integer family, an integer array, and -- on Darwin
    // only -- a `sun_len` byte. It holds no pointer and no field whose zero value is invalid, so all-zero
    // is a valid inhabitant; it is zeroed rather than written field by field because the hosts disagree
    // about which fields exist. Nothing but the family and the name below is ever read out of it.
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let start = usize::from(abstract_name);
    assert!(name.len() + start < address.sun_path.len());
    for (destination, source) in address.sun_path[start..].iter_mut().zip(name) {
        *destination = *source as libc::c_char;
    }
    let length = if abstract_name {
        offset_of!(libc::sockaddr_un, sun_path) + 1 + name.len()
    } else {
        size_of::<libc::sockaddr_un>()
    };
    (address, length as libc::socklen_t)
}

/// The two syscalls that hand an existing descriptor a `sockaddr_un`. They take one argument list and
/// carry one safety obligation between them, so they are called through one place rather than through
/// two blocks whose rationale would be the same sentence twice.
#[cfg(unix)]
type AddressCall = unsafe extern "C" fn(libc::c_int, *const libc::sockaddr, libc::socklen_t) -> libc::c_int;

#[cfg(unix)]
fn apply_address(syscall: AddressCall, descriptor: i32, address: &libc::sockaddr_un, length: libc::socklen_t) -> i32 {
    // SAFETY: `address` is borrowed for the whole call and both syscalls copy out of it rather than
    // retaining it, so nothing outlives the borrow. `length` is the value `address()` computed for THAT
    // struct and is never larger than it, so the kernel's copy cannot run past the object. `descriptor`
    // is read from a `UnixStream` the caller still owns, so it names a live socket for the duration.
    unsafe { syscall(descriptor, std::ptr::from_ref(address).cast(), length) }
}

#[cfg(unix)]
fn bind(descriptor: i32, address: &libc::sockaddr_un, length: libc::socklen_t) {
    let status = apply_address(libc::bind, descriptor, address, length);
    assert_eq!(status, 0, "bind: {}", std::io::Error::last_os_error());
}

#[cfg(unix)]
fn connect(descriptor: i32, address: &libc::sockaddr_un, length: libc::socklen_t) -> i32 {
    apply_address(libc::connect, descriptor, address, length)
}

/// The wait for a connection from ANOTHER PROCESS is bounded, and it is bounded by making the listener
/// non-blocking and re-trying rather than by waiting first and then calling a blocking `accept`: a wait
/// that times out and falls through into `accept` is unbounded again, which is how a previous test in
/// this repository turned a red into an infinite gate. A connector that dies before it dials -- which is
/// exactly what a broken identity assertion in the child looks like -- must fail this test with a
/// message, not hang the whole `cargo test -p hl-native --all-targets` run forever.
#[cfg(unix)]
const CONNECTOR_DEADLINE: Duration = Duration::from_secs(20);

#[cfg(unix)]
fn accept_within(listener: &UnixListener, deadline: Duration) -> Result<UnixStream, String> {
    listener
        .set_nonblocking(true)
        .unwrap_or_else(|error| panic!("set_nonblocking: {error}"));
    let expiry = Instant::now() + deadline;
    let outcome = loop {
        match listener.accept() {
            Ok((accepted, _)) => break Ok(accepted),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => break Err(format!("accept: {error}")),
        }
        if Instant::now() >= expiry {
            break Err(format!("no connection arrived within {deadline:?}"));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // The listener is handed back in the state it was found in whether or not a connection arrived.
    let _ = listener.set_nonblocking(false);
    outcome
}

/// A connector child whose stdout the parent must drain and whose exit the parent must reap on EVERY
/// path. Leaving it to `Child`'s drop closes the read end of its stdout pipe while the child is still
/// printing, and the child then dies of `failed printing to stdout: Broken pipe` -- destroying the
/// diagnostic that says why the parent's own assertion failed. Observed on both connectors here.
#[cfg(unix)]
struct Connector(Option<std::process::Child>);

#[cfg(unix)]
impl Connector {
    fn spawn(mut command: std::process::Command) -> Self {
        Self(Some(
            command
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("spawn the connector process"),
        ))
    }

    /// Waits up to `deadline` for the child to exit, kills it if it does not, and then reads everything
    /// it printed. The wait is bounded for the same reason the accept is: a child that never exits must
    /// fail this test, not suspend the gate. `None` for the status means it had to be killed.
    fn finish(&mut self, deadline: Duration) -> (Option<std::process::ExitStatus>, String) {
        let Some(mut child) = self.0.take() else {
            return (None, String::new());
        };
        let expiry = Instant::now() + deadline;
        let mut status = None;
        loop {
            match child.try_wait() {
                Ok(Some(exit)) => {
                    status = Some(exit);
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
            if Instant::now() >= expiry {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let stdout = child.wait_with_output().map_or_else(
            |error| format!("<connector output unavailable: {error}>"),
            |output| String::from_utf8_lossy(&output.stdout).into_owned(),
        );
        (status, stdout)
    }
}

#[cfg(unix)]
impl Drop for Connector {
    fn drop(&mut self) {
        let _ = self.finish(Duration::ZERO);
    }
}

#[cfg(unix)]
fn identity(isa: u32, operation: u32, descriptor: i32, object: u64) -> (u64, u64, u32) {
    hl_native::unix_identity_test(isa, operation, descriptor, object)
        .unwrap_or_else(|status| panic!("ISA {isa} identity operation {operation} failed at {status}"))
}

#[cfg(unix)]
fn reciprocal_connection(isa: u32, listener: &UnixListener, address: &libc::sockaddr_un, length: libc::socklen_t) {
    let client = socket();
    let client_object = 0x1000_0000_0000_0000_u64 | u64::from(isa);
    let (local, reserved_peer, hidden) = identity(isa, PREPARE, client.as_raw_fd(), client_object);
    assert_eq!(local, client_object);
    // The connector reserves only its OWN half; the peer id belongs to whichever process accepts.
    assert_eq!(reserved_peer, 0);
    assert_eq!(hidden, LOCAL_HIDDEN | RECIPROCITY_REQUIRED);
    assert_eq!(connect(client.as_raw_fd(), address, length), 0);

    let (server, _) = listener.accept().expect("accept");
    let server_allocated = 0x2200_0000_0000_0000_u64 | u64::from(isa);
    let (server_object, server_peer, server_hidden) = identity(isa, IDENTIFY, server.as_raw_fd(), server_allocated);
    assert_eq!(server_object, server_allocated, "the acceptor kept its own object id");
    assert_eq!(server_peer, client_object);
    assert_eq!(server_hidden, PEER_HIDDEN | RECIPROCITY_REQUIRED);
    let (_, collected, _) = identity(isa, CONNECTED, client.as_raw_fd(), 0);
    assert_eq!(
        collected, server_allocated,
        "the connector collected the acceptor-minted peer id"
    );

    let byte = [0x5a_u8];
    assert_eq!((&client).write(&byte).expect("write"), 1);
    let mut received = [0_u8];
    assert_eq!((&server).read(&mut received).expect("read"), 1);
    assert_eq!(received, byte);

    // Both halves of an honest, complete pair are admissible to capture. The cross-process half of the
    // obligation -- that the peer object is owned by another sealed member of the same freeze -- is
    // discharged by the broker's reciprocity join over the committed inventories, not here.
    for (name, descriptor) in [("connector", client.as_raw_fd()), ("acceptor", server.as_raw_fd())] {
        assert!(
            hl_native::unix_identity_test(isa, CHECKPOINT_ADMIT, descriptor, 0).is_ok(),
            "ISA {isa} refused the {name} end of a complete reciprocal pair"
        );
    }

    identity(isa, RESET, client.as_raw_fd(), 0);
    identity(isa, RESET, server.as_raw_fd(), 0);
}

/// A connector that has published its half and has not yet collected the acceptor's names no reciprocal
/// peer at all, so no downstream join could ever discharge its obligation. It must fail closed here.
#[cfg(unix)]
#[test]
fn an_unreciprocated_pathname_connector_is_refused_capture_on_both_isas() {
    for isa in [1, 2] {
        let path = format!("/tmp/.hl-unreciprocated-{isa}-{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let (listener_address, listener_length) = address(path.as_bytes(), false);
        let _listener = UnixListener::bind(&path).expect("listener");

        let client = socket();
        let object = 0x3300_0000_0000_0000_u64 | u64::from(isa);
        let (_, peer, flags) = identity(isa, PREPARE, client.as_raw_fd(), object);
        assert_eq!(peer, 0, "the connector reserves only its own half");
        assert_eq!(flags, LOCAL_HIDDEN | RECIPROCITY_REQUIRED);
        assert_eq!(connect(client.as_raw_fd(), &listener_address, listener_length), 0);
        assert_eq!(
            hl_native::unix_identity_test(isa, CHECKPOINT_ADMIT, client.as_raw_fd(), 0),
            Err(libc::ENOTSUP),
            "ISA {isa} admitted a connection whose reciprocal peer is unnamed"
        );

        identity(isa, RESET, client.as_raw_fd(), 0);
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(unix)]
#[test]
fn pathname_connect_and_accept_have_reciprocal_identity_on_both_isas() {
    for isa in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(format!("socket-{isa}"));
        let listener = UnixListener::bind(&path).expect("listener");
        let (address, length) = address(OsStr::new(&path).as_bytes(), false);
        reciprocal_connection(isa, &listener, &address, length);
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn abstract_connect_and_accept_have_reciprocal_identity_on_both_isas() {
    use std::os::linux::net::SocketAddrExt;

    for isa in [1, 2] {
        let name = format!("hl-identity-{}-{isa}", std::process::id());
        let bound = std::os::unix::net::SocketAddr::from_abstract_name(&name).expect("abstract name");
        let listener = UnixListener::bind_addr(&bound).expect("listener");
        let (address, length) = address(name.as_bytes(), true);
        reciprocal_connection(isa, &listener, &address, length);
    }
}

#[cfg(unix)]
#[test]
fn refused_connect_withdraws_the_unaccepted_peer_on_both_isas() {
    for isa in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join(format!("missing-{isa}"));
        let (address, length) = address(OsStr::new(&missing).as_bytes(), false);
        let client = socket();
        let object = 0x2000_0000_0000_0000_u64 | u64::from(isa);
        let (_, peer, hidden) = identity(isa, PREPARE, client.as_raw_fd(), object);
        assert_eq!(peer, 0);
        assert_eq!(hidden, LOCAL_HIDDEN | RECIPROCITY_REQUIRED);
        assert_eq!(connect(client.as_raw_fd(), &address, length), -1);
        let (local, peer, hidden) = identity(isa, FAILED, client.as_raw_fd(), 0);
        assert_eq!(local, object);
        assert_eq!(peer, 0);
        assert_eq!(hidden, LOCAL_HIDDEN | RECIPROCITY_REQUIRED);
        identity(isa, RESET, client.as_raw_fd(), 0);
    }
}

#[cfg(unix)]
#[test]
fn guest_bound_client_keeps_its_name_and_fails_closed_without_a_false_peer() {
    for isa in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let listener_path = root.path().join(format!("listener-{isa}"));
        let client_path = root.path().join(format!("client-{isa}"));
        let (listener_address, listener_length) = address(OsStr::new(&listener_path).as_bytes(), false);
        let (client_address, client_length) = address(OsStr::new(&client_path).as_bytes(), false);
        let listener = UnixListener::bind(&listener_path).expect("listener");
        let client = socket();
        bind(client.as_raw_fd(), &client_address, client_length);

        let object = 0x3000_0000_0000_0000_u64 | u64::from(isa);
        assert_eq!(
            identity(isa, PREPARE, client.as_raw_fd(), object),
            (object, 0, RECIPROCITY_REQUIRED)
        );
        assert_eq!(connect(client.as_raw_fd(), &listener_address, listener_length), 0);
        let (server, _) = listener.accept().expect("accept");
        assert_eq!(
            identity(isa, IDENTIFY, server.as_raw_fd(), 9),
            (9, 0, RECIPROCITY_REQUIRED)
        );

        let observed = client.local_addr().expect("the client's own bound name");
        assert_eq!(observed.as_pathname().expect("a pathname address"), client_path);

        identity(isa, RESET, client.as_raw_fd(), 0);
        identity(isa, RESET, server.as_raw_fd(), 0);
        drop(server);
        drop(client);
        drop(listener);
        std::fs::remove_file(client_path).unwrap();
        std::fs::remove_file(listener_path).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn dup_before_connect_shares_identity_and_capture_refusal_on_both_isas() {
    for isa in [1, 2] {
        let client = socket();
        let alias = client.try_clone().expect("dup");
        let object = 0x4000_0000_0000_0000_u64 | u64::from(isa);
        identity(isa, INITIALIZE_ALIAS, client.as_raw_fd(), object);
        identity(isa, INITIALIZE_ALIAS, alias.as_raw_fd(), object);

        let original = identity(isa, PREPARE, client.as_raw_fd(), object);
        let duplicate = identity(isa, SNAPSHOT, alias.as_raw_fd(), 0);
        assert_eq!(duplicate, original);
        assert_eq!(original.1, 0);
        assert_eq!(original.2, LOCAL_HIDDEN | RECIPROCITY_REQUIRED);
        assert_eq!(
            hl_native::unix_identity_test(isa, CHECKPOINT_ADMIT, client.as_raw_fd(), 0),
            Err(libc::ENOTSUP)
        );
        assert_eq!(
            hl_native::unix_identity_capture_test(isa, client.as_raw_fd()),
            Err(libc::ENOTSUP)
        );
        assert_eq!(
            hl_native::unix_identity_test(isa, CHECKPOINT_ADMIT, alias.as_raw_fd(), 0),
            Err(libc::ENOTSUP)
        );

        identity(isa, RESET, client.as_raw_fd(), 0);
        identity(isa, RESET, alias.as_raw_fd(), 0);
    }
}

#[cfg(unix)]
#[test]
fn async_failure_withdraws_every_alias_transactionally_on_both_isas() {
    for isa in [1, 2] {
        let client = socket();
        let alias = client.try_clone().expect("dup");
        let object = 0x5000_0000_0000_0000_u64 | u64::from(isa);
        identity(isa, INITIALIZE_ALIAS, client.as_raw_fd(), object);
        identity(isa, INITIALIZE_ALIAS, alias.as_raw_fd(), object);
        identity(isa, PREPARE, client.as_raw_fd(), object);

        identity(isa, IN_PROGRESS, alias.as_raw_fd(), 0);
        let expected = LOCAL_HIDDEN | RECIPROCITY_REQUIRED | CONNECTING;
        assert_eq!(identity(isa, SNAPSHOT, client.as_raw_fd(), 0).2, expected);
        assert_eq!(identity(isa, SNAPSHOT, alias.as_raw_fd(), 0).2, expected);
        identity(isa, ASYNC_FAILED, alias.as_raw_fd(), 0);
        for descriptor in [client.as_raw_fd(), alias.as_raw_fd()] {
            let (local, peer, flags) = identity(isa, SNAPSHOT, descriptor, 0);
            assert_eq!(local, object);
            assert_eq!(peer, 0);
            assert_eq!(flags, LOCAL_HIDDEN | RECIPROCITY_REQUIRED);
        }

        identity(isa, RESET, client.as_raw_fd(), 0);
        identity(isa, RESET, alias.as_raw_fd(), 0);
    }
}

#[cfg(unix)]
#[test]
fn capture_gate_distinguishes_private_connects_and_socketpairs_on_both_isas() {
    for isa in [1, 2] {
        let private = socket();
        let private_object = 0x6000_0000_0000_0000_u64 | u64::from(isa);
        let (_, peer, flags) = identity(isa, PRIVATE_PREPARE, private.as_raw_fd(), private_object);
        assert_eq!(peer, 0);
        assert_eq!(flags, LOCAL_HIDDEN);
        assert!(hl_native::unix_identity_test(isa, CHECKPOINT_ADMIT, private.as_raw_fd(), 0).is_ok());
        identity(isa, IN_PROGRESS, private.as_raw_fd(), 0);
        assert_eq!(
            hl_native::unix_identity_test(isa, CHECKPOINT_ADMIT, private.as_raw_fd(), 0),
            Err(libc::EBUSY)
        );
        identity(isa, RESET, private.as_raw_fd(), 0);

        let pair_end = socket();
        let pair_object = 0x7000_0000_0000_0000_u64 | u64::from(isa);
        let (_, peer, flags) = identity(isa, INITIALIZE_PAIR_END, pair_end.as_raw_fd(), pair_object);
        assert_eq!(peer, pair_object + 1);
        assert_eq!(flags, 0);
        assert!(hl_native::unix_identity_test(isa, CHECKPOINT_ADMIT, pair_end.as_raw_fd(), 0).is_ok());
        identity(isa, RESET, pair_end.as_raw_fd(), 0);
    }
}

/// `shutdown(2)` changes the open socket description, so every dup and every fork alias observes it, and
/// Linux exposes it through no `getsockopt`. The shared socket-state arena is therefore the only source
/// for it, and checkpoint capture reads the arena rather than the descriptor.
///
/// Capture used to REFUSE either direction here, because the image had nowhere to put the mask and one
/// refused descriptor fails the whole image. It carries the mask now, so admission is the assertion.
#[cfg(unix)]
#[test]
fn shutdown_state_is_shared_by_aliases_and_is_admissible_on_both_isas() {
    for isa in [1, 2] {
        for (operation, expected) in [
            (SHUTDOWN_READ, READ_CLOSED),
            (SHUTDOWN_WRITE, WRITE_CLOSED),
            (SHUTDOWN_BOTH, READ_CLOSED | WRITE_CLOSED),
        ] {
            let (endpoint, peer) = UnixStream::pair().expect("socketpair");
            let alias = endpoint.try_clone().expect("dup");
            let object = 0x7100_0000_0000_0000_u64 | u64::from(isa) | (u64::from(operation) << 8);
            identity(isa, INITIALIZE_ALIAS, endpoint.as_raw_fd(), object);
            identity(isa, INITIALIZE_ALIAS, alias.as_raw_fd(), object);

            identity(isa, operation, endpoint.as_raw_fd(), 0);
            assert_eq!(identity(isa, SNAPSHOT, endpoint.as_raw_fd(), 0).2, expected);
            assert_eq!(identity(isa, SNAPSHOT, alias.as_raw_fd(), 0).2, expected);
            assert!(hl_native::unix_identity_test(isa, CHECKPOINT_ADMIT, alias.as_raw_fd(), 0).is_ok());
            if operation == SHUTDOWN_WRITE {
                // The endpoint stopped writing; its peer did not, so bytes still arrive and the guest can
                // still read them. Admitting must not consume them: the queue belongs to pass 2.
                let payload = b"queue-survives-admission";
                assert_eq!((&peer).write(payload).expect("send"), payload.len());
                let mut received = [0_u8; 24];
                assert_eq!((&endpoint).read(&mut received).expect("recv"), payload.len());
                assert_eq!(&received[..payload.len()], payload);
            }

            identity(isa, RESET, endpoint.as_raw_fd(), 0);
            identity(isa, RESET, alias.as_raw_fd(), 0);
        }
    }
}

/// A guest that pre-creates a name in the engine's connection-identity namespace and connects with it
/// must NOT have its chosen object ids adopted. The name is a random one-shot ticket nonce, resolved out
/// of an engine-private table; a forged or replayed one resolves to nothing, so the accepted socket keeps
/// the identity the engine allocated for it and stays peerless (and therefore capture-refused).
///
/// The predecessor encoded `(client, server)` object ids directly into a world-writable
/// `/tmp/.hl-ci-<client>-<server>` name and adopted whatever `getpeername()` parsed back out, which let a
/// guest choose the identity that checkpoint restore topology is about to key on.
#[cfg(unix)]
#[test]
fn a_forged_identity_name_is_never_adopted_on_both_isas() {
    let forged_client = 0x0bad_0000_0000_0001_u64;
    let forged_server = 0x0bad_0000_0000_0002_u64;
    let namespaces = [
        // the engine's own directory, populated by a guest that can name it
        format!("/tmp/.hl-ci-default/{forged_client:016x}{forged_server:016x}"),
        // the retired literal encoding, in case anything still honours it
        format!("/tmp/.hl-ci-{forged_client:016x}-{forged_server:016x}"),
    ];

    for isa in [1, 2] {
        for (index, forged) in namespaces.iter().enumerate() {
            let directory = std::path::Path::new(forged).parent().expect("forged name has a parent");
            std::fs::create_dir_all(directory).expect("forged namespace directory");

            let listener_path = format!("/tmp/.hl-forge-listen-{isa}-{index}-{}", std::process::id());
            let _ = std::fs::remove_file(&listener_path);
            let (listener_address, listener_length) = address(listener_path.as_bytes(), false);
            let listener = UnixListener::bind(&listener_path).expect("listener");

            let _ = std::fs::remove_file(forged);
            let (forged_address, forged_length) = address(forged.as_bytes(), false);
            let attacker = socket();
            bind(attacker.as_raw_fd(), &forged_address, forged_length);
            assert_eq!(connect(attacker.as_raw_fd(), &listener_address, listener_length), 0);

            let (accepted, _) = listener.accept().expect("accept");
            let allocated = 0x7000_0000_0000_0000_u64 | u64::from(isa);
            let (local, peer, hidden) = identity(isa, IDENTIFY, accepted.as_raw_fd(), allocated);
            assert_eq!(local, allocated, "ISA {isa} adopted a forged object id from {forged}");
            assert_ne!(
                local, forged_server,
                "ISA {isa} adopted the forged server id from {forged}"
            );
            assert_eq!(peer, 0, "ISA {isa} adopted a forged peer id from {forged}");
            assert_ne!(
                peer, forged_client,
                "ISA {isa} adopted the forged client id from {forged}"
            );
            assert_eq!(
                hidden, RECIPROCITY_REQUIRED,
                "ISA {isa} marked a forged peer identity hidden"
            );
            assert_eq!(
                hl_native::unix_identity_test(isa, CHECKPOINT_ADMIT, accepted.as_raw_fd(), 0),
                Err(libc::ENOTSUP),
                "ISA {isa} admitted a forged-identity socket to capture"
            );

            identity(isa, RESET, accepted.as_raw_fd(), 0);
            let _ = std::fs::remove_file(forged);
            let _ = std::fs::remove_file(&listener_path);
        }
    }
}

/// The connector and the acceptor of an ordinary pathname `AF_UNIX` connection are routinely UNRELATED
/// host processes -- a container exec (`psql`) is forked from the hl-container daemon, not from the
/// worker running the listener (`postgres`), and inherits none of its memory. Identity has to survive
/// that, and for a while it did not: the ticket table was a `MAP_SHARED` memfd created once per worker at
/// the ISA seam, so each process published into a private table and every cross-process claim missed.
/// Every accepted postgres backend socket then carried peer=0, which no reciprocity join can ever match.
///
/// The existing suite could not see it: `reciprocal_connection` runs both ends in one process, where a
/// private table is still the same table. So this test re-executes the test binary as the connector and
/// keeps only the acceptor here. The child shares nothing but the namespace key.
#[cfg(unix)]
const CROSS_PROCESS_SOCKET: &str = "HL_UNIX_IDENTITY_CROSS_PROCESS_SOCKET";
#[cfg(unix)]
const CROSS_PROCESS_ISA: &str = "HL_UNIX_IDENTITY_CROSS_PROCESS_ISA";
#[cfg(unix)]
const CROSS_PROCESS_CLIENT_OBJECT: u64 = 0x5a5a_0000_0000_0011;

#[cfg(unix)]
#[test]
#[ignore = "re-executed as the connector by an_honest_cross_process_connection_carries_reciprocal_identity"]
fn cross_process_identity_connector() {
    let path = std::env::var(CROSS_PROCESS_SOCKET).expect("connector needs a listener path");
    let isa: u32 = std::env::var(CROSS_PROCESS_ISA)
        .expect("connector needs an ISA")
        .parse()
        .unwrap();
    let (listener_address, listener_length) = address(path.as_bytes(), false);
    let client = socket();
    let (local, reserved_peer, hidden) = identity(isa, PREPARE, client.as_raw_fd(), CROSS_PROCESS_CLIENT_OBJECT);
    assert_eq!(local, CROSS_PROCESS_CLIENT_OBJECT);
    assert_eq!(reserved_peer, 0);
    assert_eq!(hidden, LOCAL_HIDDEN | RECIPROCITY_REQUIRED);
    assert_eq!(
        connect(client.as_raw_fd(), &listener_address, listener_length),
        0,
        "connect: {}",
        std::io::Error::last_os_error()
    );
    // Hold the connection open until the acceptor has identified it. The acceptor mints the peer half and
    // writes it back into the ticket; reading one byte proves it has, so collect and report it then.
    let mut acknowledgement = [0_u8];
    let _ = (&client).read(&mut acknowledgement);
    let (_, collected, _) = identity(isa, CONNECTED, client.as_raw_fd(), 0);
    println!("collected-peer={collected:016x}");
}

#[cfg(unix)]
#[test]
fn an_honest_cross_process_connection_carries_reciprocal_identity_on_both_isas() {
    for isa in [1, 2] {
        let path = format!("/tmp/.hl-xproc-{}-{isa}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("listener");

        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "cross_process_identity_connector",
                "--ignored",
                "--nocapture",
            ])
            .env(CROSS_PROCESS_SOCKET, &path)
            .env(CROSS_PROCESS_ISA, isa.to_string());
        let mut connector = Connector::spawn(command);

        let accepted = match accept_within(&listener, CONNECTOR_DEADLINE) {
            Ok(accepted) => accepted,
            Err(reason) => {
                let (status, stdout) = connector.finish(Duration::ZERO);
                panic!("ISA {isa} connector never dialled ({reason}); it exited {status:?}:\n{stdout}");
            }
        };
        let allocated = 0x7a00_0000_0000_0000_u64 | u64::from(isa);
        let (local, peer, hidden) = identity(isa, IDENTIFY, accepted.as_raw_fd(), allocated);

        let byte = [0x5a_u8];
        assert_eq!((&accepted).write(&byte).expect("write"), 1);
        // Drained and reaped BEFORE the first assertion that can panic, so the connector's own output
        // survives to explain a failure instead of being cut off by the parent's dropped pipe.
        let (status, stdout) = connector.finish(CONNECTOR_DEADLINE);
        assert!(
            status.is_some_and(|status| status.success()),
            "ISA {isa} connector exited {status:?}:\n{stdout}"
        );
        assert_eq!(
            peer, CROSS_PROCESS_CLIENT_OBJECT,
            "ISA {isa} accepted socket has no cross-process peer identity"
        );
        assert_eq!(
            local, allocated,
            "ISA {isa} adopted a connector-minted object instead of keeping its own"
        );
        assert_eq!(
            hidden,
            PEER_HIDDEN | RECIPROCITY_REQUIRED,
            "ISA {isa} peer identity is not hidden"
        );
        let reported = stdout
            .lines()
            .find_map(|line| line.strip_prefix("collected-peer=").map(str::to_owned))
            .unwrap_or_else(|| panic!("ISA {isa} connector reported no collected peer:\n{stdout}"));
        assert_eq!(
            format!("{local:016x}"),
            reported,
            "ISA {isa} connector did not collect the acceptor-minted peer id"
        );

        identity(isa, RESET, accepted.as_raw_fd(), 0);
        let _ = std::fs::remove_file(&path);
    }
}

/// An endpoint's object id is `(owner pid << 32) | sequence`, so the pair on a connection names its two
/// owners -- but only if each half was minted by the process that owns that half. It was not: the
/// connector minted BOTH ids and published them together, and the acceptor adopted the pair wholesale, so
/// an accepted postgres backend socket reported `object=00351d4400000002 peer=00351d4400000001` -- the
/// same high 32 bits, the connector's pid, twice. A checkpoint reciprocity join then has no computable
/// second owner: the accepting process is simply absent from its own connection's identity.
///
/// Both ends mint through the production allocator here (`PREPARE_MINTED/IDENTIFY_MINTED`) rather than
/// taking a test-chosen object, because the property under test is which process the id encodes.
#[cfg(unix)]
const OWNER_PID_SOCKET: &str = "HL_UNIX_IDENTITY_OWNER_PID_SOCKET";

#[cfg(unix)]
#[test]
#[ignore = "re-executed as the connector by an_accepted_socket_encodes_its_own_owners_pid"]
fn owner_pid_identity_connector() {
    let path = std::env::var(OWNER_PID_SOCKET).expect("connector needs a listener path");
    let isa: u32 = std::env::var(CROSS_PROCESS_ISA)
        .expect("connector needs an ISA")
        .parse()
        .unwrap();
    let (listener_address, listener_length) = address(path.as_bytes(), false);
    let client = socket();
    let (local, peer, _) = identity(isa, PREPARE_MINTED, client.as_raw_fd(), 0);
    assert_eq!(peer, 0, "the connector must not mint the acceptor's half");
    println!("connector-pid={}", std::process::id());
    println!("client-object={local:016x}");
    assert_eq!(
        connect(client.as_raw_fd(), &listener_address, listener_length),
        0,
        "connect: {}",
        std::io::Error::last_os_error()
    );
    let mut acknowledgement = [0_u8];
    let _ = (&client).read(&mut acknowledgement);
    let (_, collected, _) = identity(isa, CONNECTED, client.as_raw_fd(), 0);
    println!("collected-peer={collected:016x}");
}

#[cfg(unix)]
#[test]
fn an_accepted_socket_encodes_its_own_owners_pid_on_both_isas() {
    for isa in [1, 2] {
        let path = format!("/tmp/.hl-owner-{}-{isa}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("listener");

        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "owner_pid_identity_connector", "--ignored", "--nocapture"])
            .env(OWNER_PID_SOCKET, &path)
            .env(CROSS_PROCESS_ISA, isa.to_string());
        let mut connector = Connector::spawn(command);

        let accepted = match accept_within(&listener, CONNECTOR_DEADLINE) {
            Ok(accepted) => accepted,
            Err(reason) => {
                let (status, stdout) = connector.finish(Duration::ZERO);
                panic!("ISA {isa} connector never dialled ({reason}); it exited {status:?}:\n{stdout}");
            }
        };
        let (local, peer, hidden) = identity(isa, IDENTIFY_MINTED, accepted.as_raw_fd(), 0);

        let byte = [0x5a_u8];
        assert_eq!((&accepted).write(&byte).expect("write"), 1);
        // Drained and reaped before the first assertion that can panic; see `Connector`.
        let (status, stdout) = connector.finish(CONNECTOR_DEADLINE);
        assert!(
            status.is_some_and(|status| status.success()),
            "ISA {isa} connector exited {status:?}:\n{stdout}"
        );
        assert_eq!(
            hidden,
            PEER_HIDDEN | RECIPROCITY_REQUIRED,
            "ISA {isa} identity is not reciprocal"
        );
        assert_eq!(
            u32::try_from(local >> 32).unwrap(),
            std::process::id(),
            "ISA {isa} accepted socket's own object id does not encode the ACCEPTOR's pid"
        );
        assert_ne!(
            local >> 32,
            peer >> 32,
            "ISA {isa} both endpoint ids encode the same owner ({local:016x}/{peer:016x})"
        );
        let reported = |key: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(key).map(str::to_owned))
                .unwrap_or_else(|| panic!("connector did not report {key}: {stdout}"))
        };
        assert_eq!(
            format!("{peer:016x}"),
            reported("client-object="),
            "ISA {isa} accepted socket's peer id is not the connector's own object"
        );
        assert_eq!(
            u32::try_from(peer >> 32).unwrap().to_string(),
            reported("connector-pid="),
            "ISA {isa} peer id does not encode the CONNECTOR's pid"
        );
        assert_eq!(
            format!("{local:016x}"),
            reported("collected-peer="),
            "ISA {isa} connector did not collect the acceptor-minted peer id"
        );

        identity(isa, RESET, accepted.as_raw_fd(), 0);
        let _ = std::fs::remove_file(&path);
    }
}

/// A target gated out at item scope is indistinguishable in the harness output from one whose tests all
/// passed, so say which coverage this host does not have. The notice goes to the real stderr descriptor
/// rather than through `eprintln!`, because libtest captures Rust-level output and prints it only for a
/// FAILING test -- the same reason `hl-native`'s `guest_compiler_present` skip notice writes to
/// descriptor 2.
///
/// The gate is per item rather than a file-scope `#![cfg(unix)]` because two of these cases re-execute
/// the test binary with `--exact cross_process_identity_connector` and `--exact
/// owner_pid_identity_connector`; a `mod` wrapper would rename the filter's target, and a file-scope
/// attribute would leave no item outside it to carry this notice.
#[cfg(not(unix))]
#[test]
fn unix_connection_identity_is_uncovered_on_this_host() {
    let notice = "SKIP unix_identity: 12 cases left UNCOVERED -- AF_UNIX connection identity is a \
                  sockaddr_un applied to an already-open descriptor by bind(2)/connect(2), and this \
                  host has neither the address family nor the syscalls.\n";
    // The CRT's _write takes its count as an unsigned int, while POSIX write takes a size_t, so the
    // length is converted at the call rather than the type being assumed.
    #[cfg(windows)]
    let count = notice.len() as libc::c_uint;
    #[cfg(not(windows))]
    let count = notice.len();
    // SAFETY: a write of a `'static` initialized buffer to the process's stderr descriptor. It borrows
    // nothing beyond the call, and a short or failed write is not an error worth acting on.
    unsafe {
        libc::write(2, notice.as_ptr().cast(), count);
    }
}
