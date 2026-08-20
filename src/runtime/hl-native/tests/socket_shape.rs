//! Host-shape assumptions at the AF_UNIX boundary, measured rather than read out of a header.
//!
//! Every assertion here is a place where Linux and Darwin answer the SAME question with a different
//! shape, so a value that is a faithful proxy for a property on one host is not one on the other.
//! Measured on both hosts with a standalone probe before these tests were written:
//!
//! | question                                              | Linux                     | Darwin                    |
//! |-------------------------------------------------------|---------------------------|---------------------------|
//! | `getsockname` on an unbound AF_UNIX socket             | len 2 (`offsetof sun_path`) | len 16, `sun_path` all NUL |
//! | `getpeername` on a socket accepted from an unnamed peer| len 2                     | len 16, `sun_path` all NUL |
//! | `getsockname` on a socket bound to a pathname          | len 2 + strlen + 1        | len 106 (whole `sockaddr_un`) |
//! | three `SCM_RIGHTS` fds into a one-fd control buffer     | controllen 24, cmsg_len 24, 2 fds, all valid | controllen 16, cmsg_len 24, claims 3, ONE valid |
//! | `send` of 2049 bytes on an AF_UNIX datagram pair        | ok (SO_SNDBUF 229376)     | EMSGSIZE (SO_SNDBUF 2048) |
//!
//! Both ISA arms of every hook are exercised: the aarch64 and x86_64 engines are separate translation
//! units of the same C, so a fix present in one and missing in the other is a real shape here.

#![allow(unsafe_code)]

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const GETSOCKNAME: u32 = 0;
const GETPEERNAME: u32 = 1;
const RECVMSG: u32 = 2;
const DATAGRAM_BUFFERS: u32 = 3;

const ISAS: [(u32, &str); 2] = [(1, "aarch64"), (2, "x86_64")];

fn socket(kind: i32) -> OwnedFd {
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, kind, 0) };
    assert!(descriptor >= 0, "socket: {}", std::io::Error::last_os_error());
    unsafe { OwnedFd::from_raw_fd(descriptor) }
}

fn socket_pair(kind: i32) -> (OwnedFd, OwnedFd) {
    let mut descriptors = [-1; 2];
    let status = unsafe { libc::socketpair(libc::AF_UNIX, kind, 0, descriptors.as_mut_ptr()) };
    assert_eq!(status, 0, "socketpair: {}", std::io::Error::last_os_error());
    unsafe {
        (
            OwnedFd::from_raw_fd(descriptors[0]),
            OwnedFd::from_raw_fd(descriptors[1]),
        )
    }
}

fn temporary_path(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("hl-shape-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn pathname_address(path: &std::path::Path) -> libc::sockaddr_un {
    let bytes = path.to_string_lossy().into_owned();
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    assert!(
        bytes.len() < address.sun_path.len(),
        "socket path does not fit sun_path"
    );
    for (destination, source) in address.sun_path.iter_mut().zip(bytes.as_bytes()) {
        *destination = *source as libc::c_char;
    }
    address
}

fn bind_path(descriptor: i32, path: &std::path::Path) {
    let address = pathname_address(path);
    let status = unsafe {
        libc::bind(
            descriptor,
            std::ptr::from_ref(&address).cast(),
            size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    assert_eq!(status, 0, "bind: {}", std::io::Error::last_os_error());
}

/// Guest-visible length only; the buffer is deliberately larger than any address so a translation that
/// over-reports its length is caught by the length rather than by a truncating copy.
fn guest_address(isa: u32, operation: u32, descriptor: i32) -> (u32, Vec<u8>) {
    let mut out = vec![0xAA_u8; 256];
    let length = hl_native::socket_shape_test(isa, operation, descriptor, 0, &mut out)
        .unwrap_or_else(|status| panic!("socket_shape_test({operation}) failed with errno {status}"));
    (length, out)
}

/// An AF_UNIX socket that was never bound has NO name, and the guest learns that from the reported
/// length: Linux reports the bare two-byte family. Darwin reports the whole `sockaddr_un` with the path
/// zero-filled, and reading that padding as a name published a 16-byte abstract-looking address to the
/// guest for an endpoint that has none.
#[test]
fn an_unbound_unix_socket_reports_the_bare_family_on_both_isas() {
    for (isa, name) in ISAS {
        let socket = socket(libc::SOCK_STREAM);
        let (length, bytes) = guest_address(isa, GETSOCKNAME, socket.as_raw_fd());
        assert_eq!(length, 2, "{name}: an unnamed local address is the family alone");
        assert_eq!(
            u16::from_ne_bytes([bytes[0], bytes[1]]),
            libc::AF_UNIX as u16,
            "{name}: the family word must still be AF_UNIX"
        );
    }
}

/// The same property on the far end of an accepted connection, which is where it is load-bearing: a
/// server reads the peer address length to decide whether its client is nameable at all.
#[test]
fn an_accepted_socket_reports_an_unnamed_peer_on_both_isas() {
    for (isa, name) in ISAS {
        // A real listener and a client that never bound a name. A socketpair end cannot stand in for
        // this: with its peer closed, Darwin answers getpeername with EINVAL where Linux still answers.
        let path = temporary_path(&format!("peer-{isa}"));
        let listener = socket(libc::SOCK_STREAM);
        bind_path(listener.as_raw_fd(), &path);
        assert_eq!(
            unsafe { libc::listen(listener.as_raw_fd(), 4) },
            0,
            "listen: {}",
            std::io::Error::last_os_error()
        );
        let client = socket(libc::SOCK_STREAM);
        let address = pathname_address(&path);
        assert_eq!(
            unsafe {
                libc::connect(
                    client.as_raw_fd(),
                    std::ptr::from_ref(&address).cast(),
                    size_of::<libc::sockaddr_un>() as libc::socklen_t,
                )
            },
            0,
            "connect: {}",
            std::io::Error::last_os_error()
        );
        let accepted = unsafe { libc::accept(listener.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut()) };
        assert!(accepted >= 0, "accept: {}", std::io::Error::last_os_error());
        let accepted = unsafe { OwnedFd::from_raw_fd(accepted) };
        let (length, _) = guest_address(isa, GETPEERNAME, accepted.as_raw_fd());
        let _ = std::fs::remove_file(&path);
        assert_eq!(length, 2, "{name}: an unnamed peer is the family alone");
    }
}

/// A bound pathname survives the same translation with the Linux length, not the host's: the fix for the
/// unnamed case must not collapse a real name.
#[test]
fn a_bound_pathname_keeps_its_linux_length_on_both_isas() {
    for (isa, name) in ISAS {
        let bound = temporary_path(&format!("bound-{isa}"));
        let path = bound.to_string_lossy().into_owned();
        let socket = socket(libc::SOCK_STREAM);
        bind_path(socket.as_raw_fd(), &bound);
        let (length, bytes) = guest_address(isa, GETSOCKNAME, socket.as_raw_fd());
        let _ = std::fs::remove_file(&bound);
        assert_eq!(
            length as usize,
            2 + path.len() + 1,
            "{name}: Linux reports family + path + NUL, whatever the host reports"
        );
        assert_eq!(
            &bytes[2..2 + path.len()],
            path.as_bytes(),
            "{name}: the path itself must survive"
        );
    }
}

/// Three descriptors sent into a control buffer with room for one. Darwin reports a `cmsg_len` that
/// counts all three while delivering one, so a translation that trusts `cmsg_len` reads past the control
/// buffer and publishes -- or closes -- integers the kernel never wrote. The guest must be told about
/// exactly the descriptors that arrived.
#[test]
fn a_truncated_scm_rights_record_publishes_only_delivered_descriptors_on_both_isas() {
    for (isa, name) in ISAS {
        let (sender, receiver) = socket_pair(libc::SOCK_STREAM);
        let carried: Vec<OwnedFd> = (0..3)
            .map(|_| {
                let descriptor = unsafe { libc::dup(0) };
                assert!(descriptor >= 0, "dup: {}", std::io::Error::last_os_error());
                unsafe { OwnedFd::from_raw_fd(descriptor) }
            })
            .collect();
        let raw: Vec<i32> = carried.iter().map(AsRawFd::as_raw_fd).collect();
        let space = unsafe { libc::CMSG_SPACE(3 * size_of::<i32>() as u32) } as usize;
        let mut control = vec![0_u8; space];
        let mut payload = *b"z";
        let mut vector = libc::iovec {
            iov_base: payload.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut message = unsafe { std::mem::zeroed::<libc::msghdr>() };
        message.msg_iov = &raw mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = space as _;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(3 * size_of::<i32>() as u32) as _;
            std::ptr::copy_nonoverlapping(raw.as_ptr(), libc::CMSG_DATA(header).cast::<i32>(), 3);
            assert!(
                libc::sendmsg(sender.as_raw_fd(), &raw const message, 0) > 0,
                "sendmsg: {}",
                std::io::Error::last_os_error()
            );
        }
        // A host control buffer with room for exactly one descriptor: the shape that makes the two
        // kernels disagree.
        let capacity = unsafe { libc::CMSG_SPACE(size_of::<i32>() as u32) };
        let mut out = vec![0_u8; 256];
        let written = hl_native::socket_shape_test(isa, RECVMSG, receiver.as_raw_fd(), capacity, &mut out)
            .unwrap_or_else(|status| panic!("{name}: recvmsg translation failed with errno {status}"));
        // Linux control layout: u64 cmsg_len, i32 level, i32 type, then the descriptors.
        let published = if written == 0 {
            0
        } else {
            let record = u64::from_ne_bytes(out[..8].try_into().expect("cmsg_len"));
            assert!(record >= 16, "{name}: a record shorter than its own header");
            (record as usize - 16) / size_of::<i32>()
        };
        let mut delivered = Vec::new();
        for index in 0..published {
            let start = 16 + index * size_of::<i32>();
            let descriptor = i32::from_ne_bytes(out[start..start + 4].try_into().expect("descriptor"));
            assert!(
                unsafe { libc::fcntl(descriptor, libc::F_GETFD) } != -1,
                "{name}: descriptor {descriptor} published to the guest was never delivered by the kernel"
            );
            delivered.push(unsafe { OwnedFd::from_raw_fd(descriptor) });
        }
        assert!(
            published >= 1,
            "{name}: the kernel delivered at least one descriptor and the guest must see it"
        );
        assert!(
            published <= 3,
            "{name}: {published} descriptors published for a three-descriptor send"
        );
    }
}

/// A Linux AF_UNIX datagram is bounded by SO_SNDBUF (~208KB by default); Darwin starts every one at the
/// 2048-byte `net.local.dgram.maxdgram`, so a datagram that is legal on Linux is refused on macOS. The
/// engine's creation policy raises the window; without it the send fails.
#[test]
fn an_unix_datagram_socket_carries_a_linux_sized_message_on_both_isas() {
    for (isa, name) in ISAS {
        let (sender, receiver) = socket_pair(libc::SOCK_DGRAM);
        let mut discard = vec![0_u8; 1];
        let status = hl_native::socket_shape_test(isa, DATAGRAM_BUFFERS, sender.as_raw_fd(), 0, &mut discard);
        assert_eq!(status, Ok(0), "{name}: the datagram buffer policy must apply");
        let status = hl_native::socket_shape_test(isa, DATAGRAM_BUFFERS, receiver.as_raw_fd(), 0, &mut discard);
        assert_eq!(status, Ok(0), "{name}: the datagram buffer policy must apply");
        let message = vec![0_u8; 8192];
        let sent = unsafe { libc::send(sender.as_raw_fd(), message.as_ptr().cast(), message.len(), 0) };
        assert_eq!(
            sent,
            message.len() as isize,
            "{name}: an 8KiB AF_UNIX datagram must send: {}",
            std::io::Error::last_os_error()
        );
    }
}
