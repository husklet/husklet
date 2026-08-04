use super::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Barrier};
use std::thread;

struct PortModel;

impl PortModel {
    fn first_available(model: &BTreeMap<u16, SocketId>) -> Option<u16> {
        for port in 50_000..=50_031 {
            if !model.contains_key(&port) {
                return Some(port);
            }
        }
        None
    }

    fn releasing(state: u64, expected: SocketId, other: SocketId) -> SocketId {
        if state & 8 == 0 { expected } else { other }
    }

    fn release(model: &mut BTreeMap<u16, SocketId>, port: u16, releasing: SocketId, expected: SocketId) {
        if releasing == expected {
            model.remove(&port);
        }
    }

    fn expected_claim(model: &BTreeMap<u16, SocketId>, requested: u16) -> Option<u16> {
        if requested == 0 {
            return Self::first_available(model);
        }
        if model.contains_key(&requested) {
            None
        } else {
            Some(requested)
        }
    }

    fn record_claim(model: &mut BTreeMap<u16, SocketId>, port: Option<u16>, owner: SocketId) {
        if let Some(port) = port {
            model.insert(port, owner);
        }
    }
}

struct SocketStress;

impl SocketStress {
    fn run(namespace: &SocketNamespace) -> Vec<SocketId> {
        let mut stale = Vec::new();
        for _ in 0..500 {
            let id = namespace
                .create(AddressFamily::Inet6, SocketType::Datagram, SocketProtocol::Udp, false)
                .unwrap();
            assert!(namespace.snapshot(id).is_some());
            namespace.close(id).unwrap();
            stale.push(id);
        }
        stale
    }
}

#[test]
fn lifecycle_and_generation() {
    let namespace = SocketNamespace::new();
    let first = namespace
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp, true)
        .unwrap();
    namespace
        .update(
            first,
            SocketOperation::Bind(SocketAddress::Inet4 {
                address: [127, 0, 0, 1],
                port: 8080,
            }),
        )
        .unwrap();
    namespace.update(first, SocketOperation::Listen(16)).unwrap();
    assert!(matches!(
        namespace.snapshot(first).unwrap().state,
        SocketState::Listening { backlog: 16 }
    ));
    namespace.close(first).unwrap();
    let second = namespace
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp, false)
        .unwrap();
    assert_eq!(first.slot, second.slot);
    assert!(second.generation > first.generation);
    assert_eq!(namespace.close(first), Err(SocketError::Stale));
}

#[test]
fn ephemeral_ports_allocate() {
    let ports = PortRegistry::new(40_000, 40_001);
    let a = SocketId { slot: 1, generation: 1 };
    let b = SocketId { slot: 2, generation: 1 };
    assert_eq!(ports.claim(AddressFamily::Inet4, 0, a), Ok(40_000));
    assert_eq!(ports.claim(AddressFamily::Inet4, 0, b), Ok(40_001));
    assert_eq!(ports.claim(AddressFamily::Inet4, 0, b), Err(SocketError::Capacity));
    ports.release(AddressFamily::Inet4, 40_000, a);
    assert_eq!(ports.claim(AddressFamily::Inet4, 0, b), Ok(40_000));
}

#[test]
fn route_and_dns() {
    let route = Route {
        family: AddressFamily::Inet6,
        destination: [0; 16],
        prefix_bits: 129,
        gateway: None,
        interface: 1,
        metric: 0,
    };
    assert_eq!(
        NetworkConfiguration::new(vec![route], vec![], vec![]),
        Err(SocketError::Capacity)
    );
}

#[test]
fn routes_use_longest() {
    let mut subnet = [0; 16];
    subnet[..4].copy_from_slice(&[10, 1, 0, 0]);
    let routes = vec![
        Route {
            family: AddressFamily::Inet4,
            destination: [0; 16],
            prefix_bits: 0,
            gateway: None,
            interface: 1,
            metric: 0,
        },
        Route {
            family: AddressFamily::Inet4,
            destination: subnet,
            prefix_bits: 16,
            gateway: None,
            interface: 2,
            metric: 20,
        },
        Route {
            family: AddressFamily::Inet4,
            destination: subnet,
            prefix_bits: 16,
            gateway: None,
            interface: 3,
            metric: 10,
        },
        Route {
            family: AddressFamily::Inet4,
            destination: subnet,
            prefix_bits: 16,
            gateway: None,
            interface: 4,
            metric: 10,
        },
    ];
    let table = RouteTable::new(routes).unwrap();
    let mut address = [0; 16];
    address[..4].copy_from_slice(&[10, 1, 2, 3]);
    assert_eq!(table.lookup(AddressFamily::Inet4, address).unwrap().interface, 3);
    assert!(table.lookup(AddressFamily::Inet6, address).is_none());
}

#[test]
fn network_configuration_roundtrips() {
    let configuration = NetworkConfiguration::new(
        Vec::new(),
        vec![SocketAddress::Inet4 {
            address: [1, 1, 1, 1],
            port: 53,
        }],
        vec!["example.test".to_owned()],
    )
    .unwrap();
    assert_eq!(NetworkConfiguration::restore(&configuration), Ok(configuration.clone()));
    assert!(NetworkConfiguration::new(Vec::new(), vec![SocketAddress::Unix(b"dns".to_vec())], Vec::new(),).is_err());
    assert!(NetworkConfiguration::new(Vec::new(), Vec::new(), vec!["bad..domain".to_owned()],).is_err());
    assert!(
        NetworkConfiguration::new(
            Vec::new(),
            (0..9)
                .map(|_| SocketAddress::Inet4 {
                    address: [8, 8, 8, 8],
                    port: 53,
                })
                .collect(),
            Vec::new(),
        )
        .is_err()
    );
}

#[test]
fn restore_validates_state() {
    let namespace = SocketNamespace::new();
    let id = namespace
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp, false)
        .unwrap();
    let snapshots = vec![namespace.snapshot(id).unwrap()];
    let restored = SocketNamespace::restore(&snapshots).unwrap();
    assert_eq!(restored.snapshot(id), Some(snapshots[0].clone()));
    restored.close(id).unwrap();
    let reused = restored
        .create(AddressFamily::Inet4, SocketType::Stream, SocketProtocol::Tcp, false)
        .unwrap();
    assert!(reused.generation > id.generation);

    let mut invalid = snapshots[0].clone();
    invalid.id.generation = 0;
    assert!(matches!(
        SocketNamespace::restore(&[invalid]),
        Err(SocketError::InvalidTransition)
    ));
    assert!(matches!(
        SocketNamespace::restore(&[snapshots[0].clone(), snapshots[0].clone()]),
        Err(SocketError::InvalidTransition)
    ));
}

#[test]
fn restore_rejects_invalid() {
    let namespace = SocketNamespace::new();
    let id = namespace
        .create(AddressFamily::Inet4, SocketType::Datagram, SocketProtocol::Udp, false)
        .unwrap();
    let mut snapshot = namespace.snapshot(id).unwrap();
    snapshot.state = SocketState::Listening { backlog: 1 };
    assert!(matches!(
        SocketNamespace::restore(&[snapshot]),
        Err(SocketError::InvalidTransition)
    ));
}

#[test]
fn stale_owner_cannot() {
    let ports = PortRegistry::new(40_000, 40_001);
    let live = SocketId { slot: 1, generation: 2 };
    let stale = SocketId { slot: 1, generation: 1 };
    assert_eq!(ports.claim(AddressFamily::Inet4, 8080, live), Ok(8080));
    ports.release(AddressFamily::Inet4, 8080, stale);
    assert_eq!(ports.owner(AddressFamily::Inet4, 8080), Some(live));
    assert_eq!(
        ports.claim(AddressFamily::Inet4, 8080, stale),
        Err(SocketError::AddressInUse)
    );
    assert_eq!(ports.owner(AddressFamily::Inet4, 8080), Some(live));
}

#[test]
fn same_port_contention() {
    let ports = Arc::new(PortRegistry::new(40_000, 40_010));
    let barrier = Arc::new(Barrier::new(9));
    let mut workers = Vec::new();
    for slot in 1..=8 {
        let ports = Arc::clone(&ports);
        let barrier = Arc::clone(&barrier);
        workers.push(thread::spawn(move || {
            let owner = SocketId { slot, generation: 1 };
            barrier.wait();
            (owner, ports.claim(AddressFamily::Inet4, 8080, owner))
        }));
    }
    barrier.wait();
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    let winners: Vec<_> = results.iter().filter(|(_, result)| *result == Ok(8080)).collect();
    assert_eq!(winners.len(), 1);
    assert_eq!(ports.owner(AddressFamily::Inet4, 8080), Some(winners[0].0));
    winners.iter().for_each(|(owner, _)| {
        ports.release(AddressFamily::Inet4, 8080, *owner);
    });
    assert_eq!(ports.owner(AddressFamily::Inet4, 8080), None);
}

#[test]
fn deterministic_port_model() {
    let ports = PortRegistry::new(50_000, 50_031);
    let mut model = BTreeMap::<u16, SocketId>::new();
    let mut state = 0x1234_5678_u64;
    for step in 0..2_000_u64 {
        state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let owner = SocketId {
            slot: (step % 64 + 1) as u16,
            generation: step / 64 + 1,
        };
        let requested = if state & 3 == 0 {
            0
        } else {
            50_000 + ((state >> 8) % 32) as u16
        };
        if state & 4 == 0 && !model.is_empty() {
            let port = *model.keys().nth((state as usize) % model.len()).unwrap();
            let expected = model.get(&port).copied().unwrap();
            let releasing = PortModel::releasing(state, expected, owner);
            ports.release(AddressFamily::Inet4, port, releasing);
            PortModel::release(&mut model, port, releasing, expected);
        } else {
            let result = ports.claim(AddressFamily::Inet4, requested, owner);
            let expected = PortModel::expected_claim(&model, requested);
            assert_eq!(result.ok(), expected);
            PortModel::record_claim(&mut model, expected, owner);
        }
        for port in 50_000..=50_031 {
            assert_eq!(ports.owner(AddressFamily::Inet4, port), model.get(&port).copied());
        }
    }
}

#[test]
fn deterministic_socket_lifecycle() {
    let namespace = SocketNamespace::new();
    let mut current = None;
    let mut stale = Vec::new();
    for step in 0..2_000_u32 {
        if current.is_none() {
            current = Some(
                namespace
                    .create(
                        AddressFamily::Inet4,
                        SocketType::Stream,
                        SocketProtocol::Tcp,
                        step & 1 != 0,
                    )
                    .unwrap(),
            );
        }
        let id = current.unwrap();
        let snapshot = namespace.snapshot(id).unwrap();
        match snapshot.state {
            SocketState::Created if step % 5 == 0 => {
                namespace
                    .update(
                        id,
                        SocketOperation::Bind(SocketAddress::Inet4 {
                            address: [127, 0, 0, 1],
                            port: 10_000 + (step % 100) as u16,
                        }),
                    )
                    .unwrap();
            }
            SocketState::Created => {
                namespace
                    .update(
                        id,
                        SocketOperation::BeginConnect(SocketAddress::Inet4 {
                            address: [127, 0, 0, 1],
                            port: 443,
                        }),
                    )
                    .unwrap();
            }
            SocketState::Bound if step & 1 == 0 => {
                namespace.update(id, SocketOperation::Listen(32)).unwrap();
            }
            SocketState::Bound => {
                namespace
                    .update(
                        id,
                        SocketOperation::BeginConnect(SocketAddress::Inet4 {
                            address: [127, 0, 0, 1],
                            port: 443,
                        }),
                    )
                    .unwrap();
            }
            SocketState::Connecting => {
                namespace.update(id, SocketOperation::FinishConnect).unwrap();
            }
            SocketState::Connected if step % 3 != 0 => {
                namespace
                    .update(
                        id,
                        SocketOperation::Shutdown(ShutdownState {
                            read: true,
                            write: step & 1 == 0,
                        }),
                    )
                    .unwrap();
                namespace
                    .update(id, SocketOperation::SetNonblocking(step & 1 == 0))
                    .unwrap();
            }
            SocketState::Connected | SocketState::Listening { .. } => {
                namespace.close(id).unwrap();
                stale.push(id);
                current = None;
            }
            _ => unreachable!("closed sockets are unpublished"),
        }
        if let Some(live) = current {
            let snapshot = namespace.snapshot(live).unwrap();
            assert_eq!(snapshot.id, live);
            assert_ne!(snapshot.state, SocketState::Closed);
        }
        for old in stale.iter().rev().take(4) {
            assert!(namespace.snapshot(*old).is_none());
            assert_eq!(
                namespace.update(*old, SocketOperation::SetNonblocking(true)),
                Err(SocketError::Stale)
            );
        }
    }
}

#[test]
fn concurrent_create_close() {
    let namespace = Arc::new(SocketNamespace::new());
    let mut workers = Vec::new();
    for _ in 0..8 {
        let namespace = Arc::clone(&namespace);
        workers.push(thread::spawn(move || SocketStress::run(&namespace)));
    }
    let stale: Vec<_> = workers.into_iter().flat_map(|worker| worker.join().unwrap()).collect();
    for id in stale {
        assert!(namespace.snapshot(id).is_none());
        assert_eq!(namespace.close(id), Err(SocketError::Stale));
    }
}
