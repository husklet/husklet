#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::mem::{offset_of, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;

const PREPARE: u32 = 0;
const IDENTIFY: u32 = 1;
const FAILED: u32 = 2;
const RESET: u32 = 3;
const PRIVATE_PREPARE: u32 = 4;
const IN_PROGRESS: u32 = 5;
const ASYNC_FAILED: u32 = 6;
const CHECKPOINT_ADMIT: u32 = 8;
const INITIALIZE_ALIAS: u32 = 9;
const SNAPSHOT: u32 = 10;
const INITIALIZE_PAIR_END: u32 = 11;
const SHUTDOWN_READ: u32 = 12;
const SHUTDOWN_WRITE: u32 = 13;
const SHUTDOWN_BOTH: u32 = 14;
const CONNECTED: u32 = 7;
const PREPARE_MINTED: u32 = 15;
const IDENTIFY_MINTED: u32 = 16;

const LOCAL_HIDDEN: u32 = 1;
const PEER_HIDDEN: u32 = 2;
const RECIPROCITY_REQUIRED: u32 = 4;
const CONNECTING: u32 = 8;
const READ_CLOSED: u32 = 32;
const WRITE_CLOSED: u32 = 64;

fn socket() -> OwnedFd {
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(descriptor >= 0, "socket: {}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(descriptor) }
}

fn socket_pair() -> (OwnedFd, OwnedFd) {
    let mut descriptors = [-1; 2];
    let status = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, descriptors.as_mut_ptr()) };
    assert_eq!(status, 0, "socketpair: {}", std::io::Error::last_os_error());
    unsafe {
        (
            OwnedFd::from_raw_fd(descriptors[0]),
            OwnedFd::from_raw_fd(descriptors[1]),
        )
    }
}

fn address(name: &[u8], abstract_name: bool) -> (libc::sockaddr_un, libc::socklen_t) {
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

fn bind(descriptor: i32, address: &libc::sockaddr_un, length: libc::socklen_t) {
    let status = unsafe { libc::bind(descriptor, std::ptr::from_ref(address).cast(), length) };
    assert_eq!(status, 0, "bind: {}", std::io::Error::last_os_error());
}

fn connect(descriptor: i32, address: &libc::sockaddr_un, length: libc::socklen_t) -> i32 {
    unsafe { libc::connect(descriptor, std::ptr::from_ref(address).cast(), length) }
}

fn accept(listener: i32) -> OwnedFd {
    let descriptor = unsafe { libc::accept(listener, std::ptr::null_mut(), std::ptr::null_mut()) };
    assert!(descriptor >= 0, "accept: {}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(descriptor) }
}

fn identity(isa: u32, operation: u32, descriptor: i32, object: u64) -> (u64, u64, u32) {
    hl_native::unix_identity_test(isa, operation, descriptor, object)
        .unwrap_or_else(|status| panic!("ISA {isa} identity operation {operation} failed at {status}"))
}

fn reciprocal_connection(isa: u32, address: &libc::sockaddr_un, length: libc::socklen_t) {
    let listener = socket();
    bind(listener.as_raw_fd(), address, length);
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 4) }, 0);

    let client = socket();
    let client_object = 0x1000_0000_0000_0000_u64 | u64::from(isa);
    let (local, reserved_peer, hidden) = identity(isa, PREPARE, client.as_raw_fd(), client_object);
    assert_eq!(local, client_object);
    // The connector reserves only its OWN half; the peer id belongs to whichever process accepts.
    assert_eq!(reserved_peer, 0);
    assert_eq!(hidden, LOCAL_HIDDEN | RECIPROCITY_REQUIRED);
    assert_eq!(connect(client.as_raw_fd(), address, length), 0);

    let server = accept(listener.as_raw_fd());
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
    assert_eq!(
        unsafe { libc::write(client.as_raw_fd(), byte.as_ptr().cast(), byte.len()) },
        1
    );
    let mut received = [0_u8];
    assert_eq!(
        unsafe { libc::read(server.as_raw_fd(), received.as_mut_ptr().cast(), received.len()) },
        1
    );
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
#[test]
fn an_unreciprocated_pathname_connector_is_refused_capture_on_both_isas() {
    for isa in [1, 2] {
        let path = format!("/tmp/.hl-unreciprocated-{isa}-{}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let (listener_address, listener_length) = address(path.as_bytes(), false);
        let listener = socket();
        bind(listener.as_raw_fd(), &listener_address, listener_length);
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 4) }, 0);

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

#[test]
fn pathname_connect_and_accept_have_reciprocal_identity_on_both_isas() {
    for isa in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(format!("socket-{isa}"));
        let (address, length) = address(OsStr::new(&path).as_bytes(), false);
        reciprocal_connection(isa, &address, length);
        std::fs::remove_file(path).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn abstract_connect_and_accept_have_reciprocal_identity_on_both_isas() {
    for isa in [1, 2] {
        let name = format!("hl-identity-{}-{isa}", std::process::id());
        let (address, length) = address(name.as_bytes(), true);
        reciprocal_connection(isa, &address, length);
    }
}

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

#[test]
fn guest_bound_client_keeps_its_name_and_fails_closed_without_a_false_peer() {
    for isa in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let listener_path = root.path().join(format!("listener-{isa}"));
        let client_path = root.path().join(format!("client-{isa}"));
        let (listener_address, listener_length) = address(OsStr::new(&listener_path).as_bytes(), false);
        let (client_address, client_length) = address(OsStr::new(&client_path).as_bytes(), false);
        let listener = socket();
        bind(listener.as_raw_fd(), &listener_address, listener_length);
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 4) }, 0);
        let client = socket();
        bind(client.as_raw_fd(), &client_address, client_length);

        let object = 0x3000_0000_0000_0000_u64 | u64::from(isa);
        assert_eq!(
            identity(isa, PREPARE, client.as_raw_fd(), object),
            (object, 0, RECIPROCITY_REQUIRED)
        );
        assert_eq!(connect(client.as_raw_fd(), &listener_address, listener_length), 0);
        let server = accept(listener.as_raw_fd());
        assert_eq!(
            identity(isa, IDENTIFY, server.as_raw_fd(), 9),
            (9, 0, RECIPROCITY_REQUIRED)
        );

        let mut observed = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
        let mut observed_length = size_of::<libc::sockaddr_un>() as libc::socklen_t;
        assert_eq!(
            unsafe {
                libc::getsockname(
                    client.as_raw_fd(),
                    std::ptr::from_mut(&mut observed).cast(),
                    &raw mut observed_length,
                )
            },
            0
        );
        let observed = observed
            .sun_path
            .iter()
            .map(|byte| byte.to_ne_bytes()[0])
            .take_while(|byte| *byte != 0)
            .collect::<Vec<_>>();
        assert_eq!(observed, OsStr::new(&client_path).as_bytes());

        identity(isa, RESET, client.as_raw_fd(), 0);
        identity(isa, RESET, server.as_raw_fd(), 0);
        drop(server);
        drop(client);
        drop(listener);
        std::fs::remove_file(client_path).unwrap();
        std::fs::remove_file(listener_path).unwrap();
    }
}

#[test]
fn dup_before_connect_shares_identity_and_capture_refusal_on_both_isas() {
    for isa in [1, 2] {
        let client = socket();
        let alias = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        assert!(alias >= 0, "dup: {}", std::io::Error::last_os_error());
        let alias = unsafe { OwnedFd::from_raw_fd(alias) };
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

#[test]
fn async_failure_withdraws_every_alias_transactionally_on_both_isas() {
    for isa in [1, 2] {
        let client = socket();
        let alias = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        assert!(alias >= 0, "dup: {}", std::io::Error::last_os_error());
        let alias = unsafe { OwnedFd::from_raw_fd(alias) };
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
#[test]
fn shutdown_state_is_shared_by_aliases_and_is_admissible_on_both_isas() {
    for isa in [1, 2] {
        for (operation, expected) in [
            (SHUTDOWN_READ, READ_CLOSED),
            (SHUTDOWN_WRITE, WRITE_CLOSED),
            (SHUTDOWN_BOTH, READ_CLOSED | WRITE_CLOSED),
        ] {
            let (endpoint, peer) = socket_pair();
            let alias = unsafe { libc::fcntl(endpoint.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            assert!(alias >= 0, "dup: {}", std::io::Error::last_os_error());
            let alias = unsafe { OwnedFd::from_raw_fd(alias) };
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
                assert_eq!(
                    unsafe { libc::send(peer.as_raw_fd(), payload.as_ptr().cast(), payload.len(), 0) },
                    payload.len() as isize
                );
                let mut received = [0_u8; 24];
                assert_eq!(
                    unsafe { libc::recv(endpoint.as_raw_fd(), received.as_mut_ptr().cast(), received.len(), 0) },
                    payload.len() as isize
                );
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
            let listener = socket();
            bind(listener.as_raw_fd(), &listener_address, listener_length);
            assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 4) }, 0);

            let _ = std::fs::remove_file(forged);
            let (forged_address, forged_length) = address(forged.as_bytes(), false);
            let attacker = socket();
            bind(attacker.as_raw_fd(), &forged_address, forged_length);
            assert_eq!(connect(attacker.as_raw_fd(), &listener_address, listener_length), 0);

            let accepted = accept(listener.as_raw_fd());
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
const CROSS_PROCESS_SOCKET: &str = "HL_UNIX_IDENTITY_CROSS_PROCESS_SOCKET";
const CROSS_PROCESS_ISA: &str = "HL_UNIX_IDENTITY_CROSS_PROCESS_ISA";
const CROSS_PROCESS_CLIENT_OBJECT: u64 = 0x5a5a_0000_0000_0011;

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
    unsafe {
        libc::read(
            client.as_raw_fd(),
            acknowledgement.as_mut_ptr().cast(),
            acknowledgement.len(),
        )
    };
    let (_, collected, _) = identity(isa, CONNECTED, client.as_raw_fd(), 0);
    println!("collected-peer={collected:016x}");
}

#[test]
fn an_honest_cross_process_connection_carries_reciprocal_identity_on_both_isas() {
    for isa in [1, 2] {
        let path = format!("/tmp/.hl-xproc-{}-{isa}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let (listener_address, listener_length) = address(path.as_bytes(), false);
        let listener = socket();
        bind(listener.as_raw_fd(), &listener_address, listener_length);
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 4) }, 0);

        let connector = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cross_process_identity_connector",
                "--ignored",
                "--nocapture",
            ])
            .env(CROSS_PROCESS_SOCKET, &path)
            .env(CROSS_PROCESS_ISA, isa.to_string())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the connector process");

        let accepted = accept(listener.as_raw_fd());
        let allocated = 0x7a00_0000_0000_0000_u64 | u64::from(isa);
        let (local, peer, hidden) = identity(isa, IDENTIFY, accepted.as_raw_fd(), allocated);
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

        let byte = [0x5a_u8];
        assert_eq!(
            unsafe { libc::write(accepted.as_raw_fd(), byte.as_ptr().cast(), byte.len()) },
            1
        );
        let output = connector.wait_with_output().expect("connector exit");
        assert!(
            output.status.success(),
            "connector failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let reported = String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.strip_prefix("collected-peer=").map(str::to_owned))
            .expect("connector reported its collected peer");
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
const OWNER_PID_SOCKET: &str = "HL_UNIX_IDENTITY_OWNER_PID_SOCKET";

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
    unsafe {
        libc::read(
            client.as_raw_fd(),
            acknowledgement.as_mut_ptr().cast(),
            acknowledgement.len(),
        )
    };
    let (_, collected, _) = identity(isa, CONNECTED, client.as_raw_fd(), 0);
    println!("collected-peer={collected:016x}");
}

#[test]
fn an_accepted_socket_encodes_its_own_owners_pid_on_both_isas() {
    for isa in [1, 2] {
        let path = format!("/tmp/.hl-owner-{}-{isa}", std::process::id());
        let _ = std::fs::remove_file(&path);
        let (listener_address, listener_length) = address(path.as_bytes(), false);
        let listener = socket();
        bind(listener.as_raw_fd(), &listener_address, listener_length);
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 4) }, 0);

        let connector = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "owner_pid_identity_connector", "--ignored", "--nocapture"])
            .env(OWNER_PID_SOCKET, &path)
            .env(CROSS_PROCESS_ISA, isa.to_string())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn the connector process");

        let accepted = accept(listener.as_raw_fd());
        let (local, peer, hidden) = identity(isa, IDENTIFY_MINTED, accepted.as_raw_fd(), 0);
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

        let byte = [0x5a_u8];
        assert_eq!(
            unsafe { libc::write(accepted.as_raw_fd(), byte.as_ptr().cast(), byte.len()) },
            1
        );
        let output = connector.wait_with_output().expect("connector exit");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(output.status.success(), "connector failed: {stdout}");
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
