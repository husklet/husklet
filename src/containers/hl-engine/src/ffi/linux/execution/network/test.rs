use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use hl_descriptor::ReadinessObserver;
use hl_network::{
    AddressFamily, BindRoute, EgressInterface, EgressRoute, SocketAddress, SocketConnectError, SocketConnectStatus,
    SocketHostError, SocketHostIo, SocketProtocol, SocketType,
};
use hl_runtime::{HostControl, HostSend, RuntimeNetworkHost};

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
                std::thread::yield_now();
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
    let alias = EgressInterface {
        bridge: format!("{bridge}-second").into_bytes(),
        index: 3,
        ipv4: [192, 0, 2, 2],
    };
    let alias_address = SocketAddress::Inet4 {
        address: alias.ipv4,
        port,
    };
    let path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}/10.93.0.2:{port}"));
    let alias_path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}-second/192.0.2.2:{port}"));
    let host = Native::new();
    let listener = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    assert_eq!(
        host.bind_route(
            listener.token,
            BindRoute {
                address: SocketAddress::Inet4 { address: [0; 4], port },
                interface: Some(interface.clone()),
                aliases: vec![alias.clone()],
            },
        )
        .unwrap(),
        address
    );
    host.listen(listener.token, 4).unwrap();
    assert!(path.exists());
    assert!(alias_path.exists());
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
            address: alias_address.clone(),
            interface: Some(alias),
        },
    )
    .unwrap();
    let started = host.start_connect(client.token, false);
    assert!(matches!(
        started,
        SocketConnectStatus::Connected | SocketConnectStatus::Pending
    ));
    assert_eq!(await_connect(&host, client.token), SocketConnectStatus::Connected);
    assert_eq!(host.peer_address(client.token), Ok(alias_address));

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
    assert!(alias_path.exists());
    host.close(duplicate_token);
    assert!(!path.exists());
    assert!(!alias_path.exists());
}

#[test]
fn dual_stack_wildcard_listener_owns_the_ipv4_switch_path() {
    let bridge = format!("dual-stack-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 2,
        ipv4: [10, 99, 0, 2],
    };
    let port = 34_000 + (std::process::id() % 20_000) as u16;
    let path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}/10.99.0.2:{port}"));
    let host = Native::new();
    let listener = host
        .create(AddressFamily::Inet6, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    assert_eq!(
        host.bind_route(
            listener.token,
            BindRoute {
                address: SocketAddress::Inet6 {
                    address: [0; 16],
                    port,
                    scope: 0,
                },
                interface: Some(interface),
                aliases: Vec::new(),
            },
        ),
        Ok(SocketAddress::Inet4 {
            address: [10, 99, 0, 2],
            port,
        })
    );
    assert!(path.exists());
    host.close(listener.token);
    assert!(!path.exists());
}

#[test]
fn selected_interface_stream_connect_retries_until_listener_arrives() {
    let bridge = format!("retry-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 3,
        ipv4: [10, 94, 0, 3],
    };
    let port = 36_000 + (std::process::id() % 20_000) as u16;
    let address = SocketAddress::Inet4 {
        address: interface.ipv4,
        port,
    };
    let host = Arc::new(Native::new());
    let client = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    host.prepare_connect_route(
        client.token,
        EgressRoute {
            address: address.clone(),
            interface: Some(interface.clone()),
        },
    )
    .unwrap();
    let connecting_host = Arc::clone(&host);
    let started_at = Instant::now();
    let connecting = std::thread::spawn(move || connecting_host.start_connect(client.token, false));
    std::thread::sleep(Duration::from_millis(80));
    let listener = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    host.bind_route(
        listener.token,
        BindRoute {
            address: SocketAddress::Inet4 { address: [0; 4], port },
            interface: Some(interface),
            aliases: Vec::new(),
        },
    )
    .unwrap();
    host.listen(listener.token, 4).unwrap();
    let status = connecting.join().unwrap();
    assert!(matches!(
        status,
        SocketConnectStatus::Connected | SocketConnectStatus::Pending
    ));
    assert_eq!(await_connect(&host, client.token), SocketConnectStatus::Connected);
    assert!(started_at.elapsed() >= Duration::from_millis(60));
    assert!(started_at.elapsed() < Duration::from_secs(2));
    host.close(client.token);
    host.close(listener.token);
}

#[test]
fn selected_interface_stream_connect_retries_a_dead_on_arrival_listener() {
    let bridge = format!("dead-arrival-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 5,
        ipv4: [10, 96, 0, 5],
    };
    let port = 38_000 + (std::process::id() % 20_000) as u16;
    let address = SocketAddress::Inet4 {
        address: interface.ipv4,
        port,
    };
    let host = Arc::new(Native::new());
    let listener = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    host.bind_route(
        listener.token,
        BindRoute {
            address: SocketAddress::Inet4 { address: [0; 4], port },
            interface: Some(interface.clone()),
            aliases: Vec::new(),
        },
    )
    .unwrap();
    host.listen(listener.token, 4).unwrap();
    let server_host = Arc::clone(&host);
    let server_interface = interface.clone();
    let server = std::thread::spawn(move || {
        let accept = |host: &Native, token| {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match host.accept(token) {
                    Ok(accepted) => return accepted,
                    Err(hl_runtime::RuntimeNetworkError::WouldBlock) if Instant::now() < deadline => {
                        std::thread::yield_now();
                    }
                    Err(error) => panic!("switch accept failed: {error:?}"),
                }
            }
        };
        let stale = accept(&server_host, listener.token);
        server_host.close(stale.token);
        server_host.close(listener.token);
        let replacement = server_host
            .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
            .unwrap();
        server_host
            .bind_route(
                replacement.token,
                BindRoute {
                    address: SocketAddress::Inet4 { address: [0; 4], port },
                    interface: Some(server_interface),
                    aliases: Vec::new(),
                },
            )
            .unwrap();
        server_host.listen(replacement.token, 4).unwrap();
        let live = accept(&server_host, replacement.token);
        assert_eq!(server_host.write(live.token, b"L", false), Ok(1));
        server_host.close(live.token);
        server_host.close(replacement.token);
    });
    let client = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    host.prepare_connect_route(
        client.token,
        EgressRoute {
            address,
            interface: Some(interface),
        },
    )
    .unwrap();
    assert_eq!(host.start_connect(client.token, false), SocketConnectStatus::Connected);
    let mut byte = [0_u8; 1];
    assert_eq!(host.read(client.token, &mut byte, false), Ok(1));
    assert_eq!(byte, *b"L");
    host.close(client.token);
    server.join().unwrap();
}

#[test]
fn selected_interface_stream_connect_exhaustion_is_bounded_and_nonblocking_is_deferred() {
    let bridge = format!("refused-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 4,
        ipv4: [10, 95, 0, 4],
    };
    let address = SocketAddress::Inet4 {
        address: interface.ipv4,
        port: 37_000 + (std::process::id() % 20_000) as u16,
    };
    let host = Native::new();
    let blocking = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    host.prepare_connect_route(
        blocking.token,
        EgressRoute {
            address: address.clone(),
            interface: Some(interface.clone()),
        },
    )
    .unwrap();
    let started_at = Instant::now();
    assert_eq!(
        host.start_connect(blocking.token, false),
        SocketConnectStatus::Failed(SocketConnectError::Refused)
    );
    assert!(started_at.elapsed() >= Duration::from_millis(1_000));
    assert!(started_at.elapsed() < Duration::from_millis(2_500));

    let nonblocking = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    host.prepare_connect_route(
        nonblocking.token,
        EgressRoute {
            address,
            interface: Some(interface),
        },
    )
    .unwrap();
    let started_at = Instant::now();
    assert_eq!(
        host.start_connect(nonblocking.token, true),
        SocketConnectStatus::Pending
    );
    assert!(started_at.elapsed() < Duration::from_millis(100));
    assert_eq!(
        host.poll_connect(nonblocking.token),
        SocketConnectStatus::Failed(SocketConnectError::Refused)
    );
    host.close(nonblocking.token);
    host.close(blocking.token);
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
            BindRoute {
                address: loopback.clone(),
                interface: Some(EgressInterface {
                    bridge: b"../escape".to_vec(),
                    index: 2,
                    ipv4: [10, 0, 0, 2],
                }),
                aliases: Vec::new(),
            },
        ),
        Err(hl_runtime::RuntimeNetworkError::Invalid)
    );
    assert!(matches!(
        host.bind_route(
            socket.token,
            BindRoute {
                address: loopback,
                interface: None,
                aliases: Vec::new(),
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
fn wildcard_alias_failure_removes_partial_paths_and_resets_the_socket() {
    let bridge = format!("alias-rollback-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 2,
        ipv4: [10, 97, 0, 2],
    };
    let valid_alias = EgressInterface {
        bridge: format!("{bridge}-valid").into_bytes(),
        index: 3,
        ipv4: [10, 98, 0, 2],
    };
    let invalid_alias = EgressInterface {
        bridge: b"../escape".to_vec(),
        index: 4,
        ipv4: [10, 99, 0, 2],
    };
    let port = 39_000 + (std::process::id() % 20_000) as u16;
    let primary_path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}/10.97.0.2:{port}"));
    let alias_path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}-valid/10.98.0.2:{port}"));
    let host = Native::new();
    let socket = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    assert_eq!(
        host.bind_route(
            socket.token,
            BindRoute {
                address: SocketAddress::Inet4 { address: [0; 4], port },
                interface: Some(interface),
                aliases: vec![valid_alias, invalid_alias],
            },
        ),
        Err(hl_runtime::RuntimeNetworkError::Invalid)
    );
    assert!(!primary_path.exists());
    assert!(!alias_path.exists());
    assert_eq!(
        host.local_address(socket.token),
        Ok(SocketAddress::Inet4 {
            address: [0; 4],
            port: 0,
        })
    );
    assert!(
        host.bind(
            socket.token,
            SocketAddress::Inet4 {
                address: Ipv4Addr::LOCALHOST.octets(),
                port: 0,
            },
        )
        .is_ok()
    );
    host.close(socket.token);
}

/// Publishes one switch pathname, then renames its directory away and puts a foreign entry of the
/// same name in a replacement directory before the owner releases the socket.
fn published_switch_path_under_a_replaced_parent(
    label: &str,
    ipv4: [u8; 4],
    port: u16,
    aliased: bool,
) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let bridge = format!("{label}-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 2,
        ipv4: [10, 100, 0, 2],
    };
    let alias = EgressInterface {
        bridge: format!("{bridge}-alias").into_bytes(),
        index: 3,
        ipv4,
    };
    let published = if aliased { &alias } else { &interface };
    let directory = std::path::PathBuf::from(format!(
        "/tmp/.hl-bridge-{}",
        String::from_utf8(published.bridge.clone()).unwrap()
    ));
    let moved = std::path::PathBuf::from(format!("{}-moved", directory.display()));
    let name = format!("{}.{}.{}.{}:{port}", ipv4[0], ipv4[1], ipv4[2], ipv4[3]);
    let host = Native::new();
    let socket = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    host.bind_route(
        socket.token,
        BindRoute {
            address: SocketAddress::Inet4 { address: [0; 4], port },
            interface: Some(interface),
            aliases: if aliased { vec![alias] } else { Vec::new() },
        },
    )
    .unwrap();
    assert!(directory.join(&name).exists(), "the transaction publishes the name");

    std::fs::rename(&directory, &moved).unwrap();
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join(&name), b"foreign").unwrap();

    host.close(socket.token);
    (directory, moved, name)
}

#[test]
fn switch_release_after_a_parent_rename_frees_the_held_directory_not_the_replacement() {
    let port = 43_000 + (std::process::id() % 20_000) as u16;
    let (directory, moved, name) =
        published_switch_path_under_a_replaced_parent("parent-rename", [10, 100, 0, 2], port, false);

    assert!(!moved.join(&name).exists(), "the owner releases the entry it published");
    assert_eq!(
        std::fs::read(directory.join(&name)).unwrap(),
        b"foreign",
        "an entry in a replacement directory is never released"
    );
    std::fs::remove_dir_all(&directory).unwrap();
    std::fs::remove_dir_all(&moved).unwrap();
}

#[test]
fn wildcard_alias_release_after_a_parent_rename_frees_the_held_directory_not_the_replacement() {
    let port = 44_000 + (std::process::id() % 20_000) as u16;
    let (directory, moved, name) =
        published_switch_path_under_a_replaced_parent("alias-parent-rename", [10, 105, 0, 2], port, true);

    assert!(!moved.join(&name).exists(), "the owner releases the alias it published");
    assert_eq!(
        std::fs::read(directory.join(&name)).unwrap(),
        b"foreign",
        "an alias name in a replacement directory is never released"
    );
    std::fs::remove_dir_all(&directory).unwrap();
    std::fs::remove_dir_all(&moved).unwrap();
    let primary = std::path::PathBuf::from(format!("/tmp/.hl-bridge-alias-parent-rename-{}", std::process::id()));
    std::fs::remove_dir_all(&primary).unwrap();
}

#[test]
fn wildcard_alias_collision_preserves_foreign_path_and_resets_the_socket() {
    let bridge = format!("alias-owner-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 2,
        ipv4: [10, 101, 0, 2],
    };
    let alias = EgressInterface {
        bridge: format!("{bridge}-foreign").into_bytes(),
        index: 3,
        ipv4: [10, 102, 0, 2],
    };
    let port = 41_000 + (std::process::id() % 20_000) as u16;
    let primary_path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}/10.101.0.2:{port}"));
    let alias_path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}-foreign/10.102.0.2:{port}"));
    std::fs::create_dir_all(alias_path.parent().unwrap()).unwrap();
    std::fs::write(&alias_path, b"foreign").unwrap();
    let host = Native::new();
    let socket = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    assert_eq!(
        host.bind_route(
            socket.token,
            BindRoute {
                address: SocketAddress::Inet4 { address: [0; 4], port },
                interface: Some(interface),
                aliases: vec![alias],
            },
        ),
        Err(hl_runtime::RuntimeNetworkError::AddressInUse)
    );
    assert!(!primary_path.exists());
    assert_eq!(std::fs::read(&alias_path).unwrap(), b"foreign");
    assert_eq!(
        host.local_address(socket.token),
        Ok(SocketAddress::Inet4 {
            address: [0; 4],
            port: 0
        })
    );
    assert!(
        host.bind(
            socket.token,
            SocketAddress::Inet4 {
                address: Ipv4Addr::LOCALHOST.octets(),
                port: 0,
            },
        )
        .is_ok()
    );
    host.close(socket.token);
    std::fs::remove_file(&alias_path).unwrap();
    std::fs::remove_dir(alias_path.parent().unwrap()).unwrap();
}

#[test]
fn concurrent_switch_publication_has_one_owner_and_one_final_cleanup() {
    let bridge = format!("publication-race-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 2,
        ipv4: [10, 103, 0, 2],
    };
    let port = 42_000 + (std::process::id() % 20_000) as u16;
    let path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}/10.103.0.2:{port}"));
    let host = Arc::new(Native::new());
    let tokens = (0..2)
        .map(|_| {
            host.create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
                .unwrap()
                .token
        })
        .collect::<Vec<_>>();
    let barrier = Arc::new(Barrier::new(3));
    let threads = tokens
        .iter()
        .map(|token| {
            let host = Arc::clone(&host);
            let barrier = Arc::clone(&barrier);
            let interface = interface.clone();
            let token = *token;
            std::thread::spawn(move || {
                barrier.wait();
                host.bind_route(
                    token,
                    BindRoute {
                        address: SocketAddress::Inet4 { address: [0; 4], port },
                        interface: Some(interface),
                        aliases: Vec::new(),
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(hl_runtime::RuntimeNetworkError::AddressInUse))
            .count(),
        1
    );
    assert!(path.exists());
    for token in tokens {
        host.close(token);
    }
    assert!(!path.exists());
}

#[test]
fn switch_bind_failure_restores_the_original_inet_socket() {
    use std::os::unix::net::UnixListener;

    let bridge = format!("bind-rollback-{}", std::process::id());
    let interface = EgressInterface {
        bridge: bridge.as_bytes().to_vec(),
        index: 2,
        ipv4: [10, 96, 0, 2],
    };
    let port = 38_000 + (std::process::id() % 20_000) as u16;
    let path = std::path::PathBuf::from(format!("/tmp/.hl-bridge-{bridge}/10.96.0.2:{port}"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let collision = UnixListener::bind(&path).unwrap();
    let host = Native::new();
    let socket = host
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp)
        .unwrap();
    let options = [
        (1, 2, hl_linux::GuestSocketOption::Scalar(1)),
        (1, 5, hl_linux::GuestSocketOption::Scalar(1)),
        (1, 6, hl_linux::GuestSocketOption::Scalar(1)),
        (1, 7, hl_linux::GuestSocketOption::Scalar(32_768)),
        (1, 8, hl_linux::GuestSocketOption::Scalar(32_768)),
        (1, 9, hl_linux::GuestSocketOption::Scalar(1)),
        (1, 10, hl_linux::GuestSocketOption::Scalar(1)),
        (1, 13, hl_linux::GuestSocketOption::Linger { enabled: 1, seconds: 7 }),
        (1, 15, hl_linux::GuestSocketOption::Scalar(1)),
        (
            1,
            20,
            hl_linux::GuestSocketOption::Timeval {
                seconds: 0,
                microseconds: 25_000,
            },
        ),
        (
            1,
            21,
            hl_linux::GuestSocketOption::Timeval {
                seconds: 0,
                microseconds: 30_000,
            },
        ),
        (0, 1, hl_linux::GuestSocketOption::Scalar(0x20)),
        (0, 2, hl_linux::GuestSocketOption::Scalar(42)),
        (6, 1, hl_linux::GuestSocketOption::Scalar(1)),
    ];
    for (level, option, value) in &options {
        host.set_option(socket.token, *level, *option, value.clone()).unwrap();
    }
    let expected = options
        .iter()
        .map(|(level, option, _)| (*level, *option, host.get_option(socket.token, *level, *option).unwrap()))
        .collect::<Vec<_>>();

    assert_eq!(
        host.bind_route(
            socket.token,
            BindRoute {
                address: SocketAddress::Inet4 {
                    address: interface.ipv4,
                    port,
                },
                interface: Some(interface),
                aliases: Vec::new(),
            },
        ),
        Err(hl_runtime::RuntimeNetworkError::AddressInUse)
    );
    assert_eq!(
        host.local_address(socket.token),
        Ok(SocketAddress::Inet4 {
            address: [0; 4],
            port: 0,
        })
    );
    for (level, option, value) in expected {
        assert_eq!(host.get_option(socket.token, level, option), Ok(value));
    }
    assert!(
        host.bind(
            socket.token,
            SocketAddress::Inet4 {
                address: Ipv4Addr::LOCALHOST.octets(),
                port: 0,
            },
        )
        .is_ok()
    );

    host.close(socket.token);
    drop(collision);
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(path.parent().unwrap()).unwrap();
}

#[test]
fn selected_interface_datagrams_preserve_source_and_connected_peer() {
    let interface = EgressInterface {
        bridge: format!("udp-adapter-{}", std::process::id()).into_bytes(),
        index: 2,
        ipv4: [10, 94, 0, 2],
    };
    let server_address = SocketAddress::Inet4 {
        address: interface.ipv4,
        port: 36_000 + (std::process::id() % 20_000) as u16,
    };
    let host = Native::new();
    let server = host
        .create(AddressFamily::Inet4, SocketType::Datagram, SocketProtocol::Udp)
        .unwrap();
    assert_eq!(
        host.bind_route(
            server.token,
            BindRoute {
                address: server_address.clone(),
                interface: Some(interface.clone()),
                aliases: Vec::new(),
            },
        ),
        Ok(server_address.clone())
    );
    let client = host
        .create(AddressFamily::Inet4, SocketType::Datagram, SocketProtocol::Udp)
        .unwrap();
    assert_eq!(
        host.send_to_route(
            client.token,
            b"one",
            EgressRoute {
                address: server_address.clone(),
                interface: Some(interface.clone()),
            },
            false,
        ),
        Ok(3)
    );
    let receive = |token, output: &mut [u8]| {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match host.receive_from(token, output, false, false) {
                Ok(value) => break value,
                Err(hl_runtime::RuntimeNetworkError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                result => panic!("switch datagram receive failed: {result:?}"),
            }
        }
    };
    let mut input = [0_u8; 8];
    let first = receive(server.token, &mut input);
    assert_eq!(&input[..first.count], b"one");
    let SocketAddress::Inet4 { address, port } = first.source.clone() else {
        panic!("switch source was not IPv4")
    };
    assert_eq!(address, interface.ipv4);
    assert!(port > 0);
    assert_eq!(
        host.send_to_route(
            server.token,
            b"reply",
            EgressRoute {
                address: first.source,
                interface: Some(interface.clone()),
            },
            false,
        ),
        Ok(5)
    );
    let reply = receive(client.token, &mut input);
    assert_eq!(&input[..reply.count], b"reply");
    assert_eq!(reply.source, server_address);

    let messaged = host
        .create(AddressFamily::Inet4, SocketType::Datagram, SocketProtocol::Udp)
        .unwrap();
    let attachment: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
    let sent = host
        .send_message(
            messaged.token,
            HostSend {
                payload: b"msg".to_vec(),
                route: Some(EgressRoute {
                    address: server_address.clone(),
                    interface: Some(interface.clone()),
                }),
                controls: vec![HostControl::Rights(vec![attachment])],
                nonblocking: true,
                record: true,
            },
        )
        .unwrap();
    assert_eq!(sent.count, 3);
    assert!(sent.rights_consumed);
    let message = host.receive_message(server.token, 3, 1024, false, false).unwrap();
    assert_eq!(message.payload, b"msg");
    assert_eq!(message.source, Some(host.local_address(messaged.token).unwrap()));
    let HostControl::Rights(rights) = &message.controls[0] else {
        panic!("routed SCM_RIGHTS missing");
    };
    assert_eq!(rights.len(), 1);

    let connected = host
        .create(AddressFamily::Inet4, SocketType::Datagram, SocketProtocol::Udp)
        .unwrap();
    host.prepare_connect_route(
        connected.token,
        EgressRoute {
            address: server_address.clone(),
            interface: Some(interface),
        },
    )
    .unwrap();
    assert_eq!(
        host.start_connect(connected.token, false),
        SocketConnectStatus::Connected
    );
    assert_eq!(host.peer_address(connected.token), Ok(server_address));
    assert_eq!(host.write(connected.token, b"two", false), Ok(3));
    let second = receive(server.token, &mut input);
    assert_eq!(&input[..second.count], b"two");

    host.close(connected.token);
    host.close(messaged.token);
    host.close(client.token);
    host.close(server.token);
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
