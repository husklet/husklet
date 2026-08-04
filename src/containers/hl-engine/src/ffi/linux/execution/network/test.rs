use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use hl_descriptor::ReadinessObserver;
use hl_network::{
    AddressFamily, SocketAddress, SocketConnectStatus, SocketHostError, SocketHostIo, SocketProtocol, SocketType,
};
use hl_runtime::RuntimeNetworkHost;

use super::Native;

struct Observer(AtomicUsize);

impl ReadinessObserver for Observer {
    fn readiness_changed(&self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

fn await_connect(host: &Native, token: u64) -> SocketConnectStatus {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let status = host.poll_connect(token);
        if status != SocketConnectStatus::Pending || Instant::now() >= deadline {
            return status;
        }
        std::thread::yield_now();
    }
}

#[test]
fn local_service() {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = Native::new();
    let socket = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    let observer = Arc::new(Observer(AtomicUsize::new(0)));
    let observer_trait: Arc<dyn ReadinessObserver> = observer.clone();
    host.attach_readiness(socket.token, Arc::downgrade(&observer_trait));
    host.prepare_connect(
        socket.token,
        SocketAddress::Inet4 {
            address: Ipv4Addr::LOCALHOST.octets(),
            port,
        },
    )
    .unwrap();
    let started = host.start_connect(socket.token, false);
    assert!(matches!(
        started,
        SocketConnectStatus::Connected | SocketConnectStatus::Pending
    ));
    assert_eq!(await_connect(&host, socket.token), SocketConnectStatus::Connected);

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut accepted = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::yield_now()
            }
            Err(error) => panic!("local accept failed: {error}"),
        }
    };
    accepted.write_all(b"host").unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while observer.0.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(observer.0.load(Ordering::Acquire) > 0);
    let mut input = [0_u8; 4];
    assert_eq!(host.read(socket.token, &mut input, false), Ok(4));
    assert_eq!(&input, b"host");
    assert_eq!(host.write(socket.token, b"guest", false), Ok(5));
    let mut output = [0_u8; 5];
    accepted.read_exact(&mut output).unwrap();
    assert_eq!(&output, b"guest");
    host.cancel(socket.token);
    host.close(socket.token);
    assert_eq!(
        host.read(socket.token, &mut input, false),
        Err(SocketHostError::Canceled)
    );
    host.close(socket.token);
}

#[test]
fn loopback_ephemeral_bind_reports_allocated_port() {
    let host = Native::new();
    let socket = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    let local = host
        .bind(
            socket.token,
            SocketAddress::Inet4 {
                address: Ipv4Addr::LOCALHOST.octets(),
                port: 0,
            },
        )
        .unwrap();
    assert!(matches!(
        local,
        SocketAddress::Inet4 {
            address: [127, 0, 0, 1],
            port: 1..
        }
    ));
    host.close(socket.token);
}

#[test]
fn loopback_fixed_bind_preserves_guest_port() {
    let host = Native::new();
    let socket = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    let local = host
        .bind(
            socket.token,
            SocketAddress::Inet4 {
                address: Ipv4Addr::LOCALHOST.octets(),
                port: 47_251,
            },
        )
        .unwrap();
    assert_eq!(
        local,
        SocketAddress::Inet4 {
            address: Ipv4Addr::LOCALHOST.octets(),
            port: 47_251,
        }
    );
    host.close(socket.token);
}

#[test]
fn urgent_data_peek_mark_and_readiness() {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = Native::new();
    let socket = host.create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp).unwrap();
    host.prepare_connect(
        socket.token,
        SocketAddress::Inet4 { address: Ipv4Addr::LOCALHOST.octets(), port },
    ).unwrap();
    let status = host.start_connect(socket.token, false);
    assert!(matches!(status, SocketConnectStatus::Connected | SocketConnectStatus::Pending));
    assert_eq!(await_connect(&host, socket.token), SocketConnectStatus::Connected);
    let (accepted, _) = listener.accept().unwrap();
    // SAFETY: accepted owns a connected TCP descriptor and the byte is readable.
    assert_eq!(unsafe { libc::send(accepted.as_raw_fd(), b"Z".as_ptr().cast(), 1, libc::MSG_OOB) }, 1);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !host.readiness(socket.token).priority && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(host.readiness(socket.token).priority);
    assert!(host.at_urgent_mark(socket.token).unwrap());
    let mut byte = [0];
    assert_eq!(host.receive_urgent(socket.token, &mut byte, true).unwrap(), 1);
    assert_eq!(host.receive_urgent(socket.token, &mut byte, false).unwrap(), 1);
    assert_eq!(byte, *b"Z");
    assert!(!host.readiness(socket.token).priority);
}
