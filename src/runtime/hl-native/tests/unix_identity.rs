#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::mem::{offset_of, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;

const PREPARE: u32 = 0;
const IDENTIFY: u32 = 1;
const FAILED: u32 = 2;
const RESET: u32 = 3;

fn socket() -> OwnedFd {
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(descriptor >= 0, "socket: {}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(descriptor) }
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
    assert_ne!(reserved_peer, 0);
    assert_ne!(reserved_peer, client_object);
    assert_eq!(hidden, 1);
    assert_eq!(connect(client.as_raw_fd(), address, length), 0);

    let server = accept(listener.as_raw_fd());
    let (server_object, server_peer, server_hidden) = identity(isa, IDENTIFY, server.as_raw_fd(), 7);
    assert_eq!(server_object, reserved_peer);
    assert_eq!(server_peer, client_object);
    assert_eq!(server_hidden, 2);

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

    identity(isa, RESET, client.as_raw_fd(), 0);
    identity(isa, RESET, server.as_raw_fd(), 0);
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
        assert_ne!(peer, 0);
        assert_eq!(hidden, 1);
        assert_eq!(connect(client.as_raw_fd(), &address, length), -1);
        let (local, peer, hidden) = identity(isa, FAILED, client.as_raw_fd(), 0);
        assert_eq!(local, object);
        assert_eq!(peer, 0);
        assert_eq!(hidden, 1);
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
        assert_eq!(identity(isa, PREPARE, client.as_raw_fd(), object), (object, 0, 0));
        assert_eq!(connect(client.as_raw_fd(), &listener_address, listener_length), 0);
        let server = accept(listener.as_raw_fd());
        assert_eq!(identity(isa, IDENTIFY, server.as_raw_fd(), 9), (9, 0, 0));

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
