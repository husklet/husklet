use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use hl_descriptor::ReadinessObserver;
use hl_network::{
    AddressFamily, EgressInterface, EgressRoute, SocketAddress, SocketConnectStatus, SocketHostError, SocketHostIo,
    SocketProtocol, SocketType,
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
fn selected_interface_streams_rendezvous_and_release_their_path() {
    let bridge = format!("adapter-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 2,
        ipv4: [10, 93, 0, 2],
    };
    let port = 35_000 + (std::process::id() % 20_000) as u16;
    let address = SocketAddress::Inet4 {
        address: interface.ipv4,
        port,
    };
    let path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}/10.93.0.2:{port}"));
    let host = Native::new();
    let listener = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    assert_eq!(
        host.bind_route(
            listener.token,
            EgressRoute {
                address: SocketAddress::Inet4 { address: [0; 4], port },
                interface: Some(interface.clone()),
            },
        )
        .unwrap(),
        address
    );
    host.listen(listener.token, 4).unwrap();
    assert!(path.exists());
    // SAFETY: descriptor is live and dup returns an independently owned reference to the same socket.
    let duplicated = unsafe { libc::dup(host.descriptor(listener.token).unwrap()) };
    assert!(duplicated >= 0);
    let duplicate_token = host.insert(duplicated).unwrap();

    let client = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    host.prepare_connect_route(
        client.token,
        EgressRoute {
            address: address.clone(),
            interface: Some(interface),
        },
    )
    .unwrap();
    let started = host.start_connect(client.token, false);
    assert!(matches!(
        started,
        SocketConnectStatus::Connected | SocketConnectStatus::Pending
    ));
    assert_eq!(await_connect(&host, client.token), SocketConnectStatus::Connected);
    assert_eq!(host.peer_address(client.token), Ok(address.clone()));

    let deadline = Instant::now() + Duration::from_secs(2);
    let accepted = loop {
        match host.accept(listener.token) {
            Ok(value) => break value,
            Err(hl_runtime::RuntimeNetworkError::WouldBlock) if Instant::now() < deadline => std::thread::yield_now(),
            Err(error) => panic!("switch accept failed: {error:?}"),
        }
    };
    assert_eq!(accepted.local, address);
    assert_eq!(accepted.peer, address);
    assert_eq!(host.local_address(accepted.token), Ok(address.clone()));
    assert_eq!(host.peer_address(accepted.token), Ok(address.clone()));
    assert_eq!(host.write(client.token, b"switch", false), Ok(6));
    let mut input = [0_u8; 6];
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match host.read(accepted.token, &mut input, false) {
            Ok(6) => break,
            Err(SocketHostError::WouldBlock) if Instant::now() < deadline => std::thread::yield_now(),
            result => panic!("switch read failed: {result:?}"),
        }
    }
    assert_eq!(&input, b"switch");
    host.close(accepted.token);
    assert!(path.exists());
    host.close(client.token);
    assert!(path.exists());
    host.close(listener.token);
    assert!(path.exists());
    host.close(duplicate_token);
    assert!(!path.exists());
}

#[test]
fn invalid_switch_identity_is_transactional_and_direct_route_falls_back() {
    let host = Native::new();
    let socket = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    let loopback = SocketAddress::Inet4 {
        address: Ipv4Addr::LOCALHOST.octets(),
        port: 0,
    };
    assert_eq!(
        host.bind_route(
            socket.token,
            EgressRoute {
                address: loopback.clone(),
                interface: Some(EgressInterface {
                    bridge: b"../escape".to_vec(),
                    index: 2,
                    ipv4: [10, 0, 0, 2],
                }),
            },
        ),
        Err(hl_runtime::RuntimeNetworkError::Invalid)
    );
    assert!(matches!(
        host.bind_route(
            socket.token,
            EgressRoute {
                address: loopback,
                interface: None,
            },
        ),
        Ok(SocketAddress::Inet4 {
            address: [127, 0, 0, 1],
            port: 1..
        })
    ));
    host.close(socket.token);
}

#[test]
fn urgent_data_peek_mark_and_readiness() {
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = Native::new();
    let socket = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    host.prepare_connect(
        socket.token,
        SocketAddress::Inet4 {
            address: Ipv4Addr::LOCALHOST.octets(),
            port,
        },
    )
    .unwrap();
    let status = host.start_connect(socket.token, false);
    assert!(matches!(
        status,
        SocketConnectStatus::Connected | SocketConnectStatus::Pending
    ));
    assert_eq!(await_connect(&host, socket.token), SocketConnectStatus::Connected);
    let (accepted, _) = listener.accept().unwrap();
    // SAFETY: accepted owns a connected TCP descriptor and the byte is readable.
    assert_eq!(
        unsafe { libc::send(accepted.as_raw_fd(), b"Z".as_ptr().cast(), 1, libc::MSG_OOB) },
        1
    );
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
