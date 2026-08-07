use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;

use crate::{
    AcceptedSocketCheckpoint, AddressFamily, NETWORK_CHECKPOINT_SOCKET_MAXIMUM, NetworkCatalog, NetworkCatalogError,
    NetworkCatalogRestore, NetworkCheckpointError, NetworkConfiguration, NetworkResourceKey, NetworkSocketResource,
    NetworkSocketState, ShutdownState, SocketAddress, SocketId, SocketProtocol, SocketSnapshot, SocketState,
    SocketType, UnixSocketPair,
};

struct StandaloneRestore;

impl NetworkCatalogRestore for StandaloneRestore {
    fn host_socket(
        &mut self,
        _: &SocketSnapshot,
        _: NetworkResourceKey,
    ) -> Result<Arc<dyn NetworkSocketResource>, NetworkCheckpointError> {
        Err(NetworkCheckpointError::InvalidImage)
    }

    fn accepted_socket(&mut self, _: &AcceptedSocketCheckpoint) -> Result<(), NetworkCheckpointError> {
        Err(NetworkCheckpointError::InvalidImage)
    }

    fn descriptor(&mut self, _: u64) -> Result<hl_descriptor::DescriptionRef, NetworkCheckpointError> {
        Err(NetworkCheckpointError::InvalidImage)
    }

    fn commit(&mut self) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }

    fn rollback(&mut self) {}

    fn resume(&mut self) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }
}

struct CatalogScenario;

impl CatalogScenario {
    fn socket(state: SocketState, local: Option<SocketAddress>) -> SocketSnapshot {
        SocketSnapshot {
            id: SocketId { slot: 1, generation: 1 },
            family: AddressFamily::Inet4,
            socket_type: SocketType::Stream,
            protocol: SocketProtocol::Tcp,
            state,
            local,
            peer: None,
            connect_error: None,
            nonblocking: true,
            shutdown: ShutdownState::default(),
        }
    }

    fn unix_socket() -> SocketSnapshot {
        SocketSnapshot {
            id: SocketId { slot: 1, generation: 1 },
            family: AddressFamily::Unix,
            socket_type: SocketType::Stream,
            protocol: SocketProtocol::Default,
            state: SocketState::Connected,
            local: Some(SocketAddress::Unix(Vec::new())),
            peer: Some(SocketAddress::Unix(Vec::new())),
            connect_error: None,
            nonblocking: false,
            shutdown: ShutdownState::default(),
        }
    }

    fn standalone_unix_socket() -> SocketSnapshot {
        SocketSnapshot {
            state: SocketState::Created,
            local: None,
            peer: None,
            ..Self::unix_socket()
        }
    }

    fn unix_listener() -> SocketSnapshot {
        SocketSnapshot {
            state: SocketState::Listening { backlog: 8 },
            local: Some(SocketAddress::Unix(Vec::new())),
            peer: None,
            ..Self::unix_socket()
        }
    }
}

fn catalog() -> NetworkCatalog {
    NetworkCatalog::new(NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap())
}

#[test]
fn namespace_view_is_coherent_and_generation_qualified() {
    let catalog = catalog();
    let initial = catalog.namespace_view();
    assert_eq!(initial.generation, 0);
    assert!(initial.unix.is_empty());

    let path = b"/run/hl/listener.sock".to_vec();
    let mut listener = CatalogScenario::unix_listener();
    listener.local = Some(SocketAddress::Unix(path.clone()));
    let id = catalog.insert_unix(listener.clone()).unwrap();

    let bound = catalog.namespace_view();
    assert!(bound.generation > initial.generation);
    assert_eq!(bound.unix.len(), 1);
    assert_eq!(bound.unix[0].id, id);
    assert_eq!(bound.unix[0].inode, id.generation.wrapping_shl(16) | u64::from(id.slot));
    assert_eq!(bound.unix[0].socket_type, SocketType::Stream);
    assert_eq!(bound.unix[0].state, SocketState::Listening { backlog: 8 });
    assert_eq!(bound.unix[0].path.as_deref(), Some(path.as_slice()));

    listener.id = id;
    listener.state = SocketState::Bound;
    catalog.replace_snapshot(id, listener).unwrap();
    let replaced = catalog.namespace_view();
    assert!(replaced.generation > bound.generation);
    assert_eq!(replaced.unix[0].state, SocketState::Bound);

    catalog.remove(id).unwrap();
    let removed = catalog.namespace_view();
    assert!(removed.generation > replaced.generation);
    assert!(removed.unix.is_empty());
}

#[test]
fn namespace_view_lists_each_pair_endpoint_once() {
    let catalog = catalog();
    let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, hl_descriptor::StatusFlags::default()).unwrap());
    let ids = catalog
        .insert_unix_pair([CatalogScenario::unix_socket(), CatalogScenario::unix_socket()], pair)
        .unwrap();

    let view = catalog.namespace_view();
    assert_eq!(view.unix.iter().map(|socket| socket.id).collect::<Vec<_>>(), ids);
}

#[test]
fn generation_reuse_rejects() {
    let catalog = catalog();
    let stale = catalog
        .insert_host(
            CatalogScenario::socket(SocketState::Created, None),
            NetworkResourceKey::new(1).unwrap(),
            Arc::new(()),
            Vec::new(),
        )
        .unwrap();
    catalog.remove(stale).unwrap();
    let current = catalog
        .insert_host(
            CatalogScenario::socket(SocketState::Created, None),
            NetworkResourceKey::new(2).unwrap(),
            Arc::new(()),
            Vec::new(),
        )
        .unwrap();
    assert_ne!(stale.generation, current.generation);
    assert_eq!(catalog.snapshot(stale), Err(NetworkCatalogError::Stale));
    catalog.freeze_checkpoint();
    let image = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();
    assert_eq!(image.generations, vec![current.generation]);
    assert_eq!(image.sockets.len(), 1);
}

#[test]
fn port_owner_must() {
    let catalog = catalog();
    let owner = catalog
        .insert_host(
            CatalogScenario::socket(
                SocketState::Bound,
                Some(SocketAddress::Inet4 {
                    address: [127, 0, 0, 1],
                    port: 8080,
                }),
            ),
            NetworkResourceKey::new(1).unwrap(),
            Arc::new(()),
            Vec::new(),
        )
        .unwrap();
    catalog
        .claim_port(crate::PortCheckpoint {
            family: AddressFamily::Inet4,
            port: 8080,
            owner,
        })
        .unwrap();
    catalog.freeze_checkpoint();
    assert!(catalog.checkpoint_image().is_ok());
    catalog.thaw_checkpoint();
}

#[test]
fn host_bind_transaction_rolls_back_once_and_commits_once() {
    let catalog = catalog();
    let owner = catalog
        .insert_host(
            CatalogScenario::socket(SocketState::Created, None),
            NetworkResourceKey::new(1).unwrap(),
            Arc::new(()),
            Vec::new(),
        )
        .unwrap();
    let mut bound = catalog.snapshot(owner).unwrap();
    bound.local = Some(SocketAddress::Inet4 {
        address: [192, 0, 2, 1],
        port: 8080,
    });
    bound.state = SocketState::Bound;
    let port = crate::PortCheckpoint {
        family: AddressFamily::Inet4,
        port: 8080,
        owner,
    };
    let rollback = catalog.prepare_host_bind(bound.clone(), port.clone()).unwrap();
    assert_eq!(catalog.snapshot(owner).unwrap().state, SocketState::Created);
    rollback.rollback().unwrap();
    assert_eq!(catalog.snapshot(owner).unwrap().state, SocketState::Created);

    let commit = catalog.prepare_host_bind(bound.clone(), port).unwrap();
    commit.commit().unwrap();
    assert_eq!(catalog.snapshot(owner).unwrap(), bound);
}

#[test]
fn host_bind_transaction_rejects_collision_without_snapshot_mutation() {
    let catalog = catalog();
    let create = |resource| {
        catalog
            .insert_host(
                CatalogScenario::socket(SocketState::Created, None),
                NetworkResourceKey::new(resource).unwrap(),
                Arc::new(()),
                Vec::new(),
            )
            .unwrap()
    };
    let first = create(1);
    let second = create(2);
    let bind = |owner| {
        let mut snapshot = catalog.snapshot(owner).unwrap();
        snapshot.local = Some(SocketAddress::Inet4 {
            address: [0; 4],
            port: 9000,
        });
        snapshot.state = SocketState::Bound;
        catalog.prepare_host_bind(
            snapshot,
            crate::PortCheckpoint {
                family: AddressFamily::Inet4,
                port: 9000,
                owner,
            },
        )
    };
    bind(first).unwrap().commit().unwrap();
    assert!(matches!(bind(second), Err(NetworkCatalogError::Invalid)));
    assert_eq!(catalog.snapshot(second).unwrap().state, SocketState::Created);
}

#[test]
fn stale_bind_rollback_does_not_overwrite_newer_snapshot() {
    let catalog = catalog();
    let owner = catalog
        .insert_host(
            CatalogScenario::socket(SocketState::Created, None),
            NetworkResourceKey::new(1).unwrap(),
            Arc::new(()),
            Vec::new(),
        )
        .unwrap();
    let mut bound = catalog.snapshot(owner).unwrap();
    bound.local = Some(SocketAddress::Inet4 {
        address: [127, 0, 0, 1],
        port: 7000,
    });
    bound.state = SocketState::Bound;
    let prepared = catalog
        .prepare_host_bind(
            bound.clone(),
            crate::PortCheckpoint {
                family: AddressFamily::Inet4,
                port: 7000,
                owner,
            },
        )
        .unwrap();
    let mut newer = bound;
    newer.state = SocketState::Listening { backlog: 4 };
    catalog.replace_host_snapshot(owner, newer.clone()).unwrap();
    assert_eq!(prepared.commit(), Err(NetworkCatalogError::Stale));
    assert_eq!(catalog.snapshot(owner).unwrap(), newer);
}

#[test]
fn concurrent_bind_reserves_one_owner() {
    let catalog = Arc::new(catalog());
    let mut candidates = Vec::new();
    for resource in 1..=2 {
        let owner = catalog
            .insert_host(
                CatalogScenario::socket(SocketState::Created, None),
                NetworkResourceKey::new(resource).unwrap(),
                Arc::new(()),
                Vec::new(),
            )
            .unwrap();
        let mut snapshot = catalog.snapshot(owner).unwrap();
        snapshot.local = Some(SocketAddress::Inet4 {
            address: [0; 4],
            port: 9100,
        });
        snapshot.state = SocketState::Bound;
        candidates.push((owner, snapshot));
    }
    let barrier = Arc::new(Barrier::new(3));
    let observed = Arc::new(Barrier::new(3));
    let workers = candidates
        .into_iter()
        .map(|(owner, snapshot)| {
            let catalog = catalog.clone();
            let barrier = barrier.clone();
            let observed = observed.clone();
            thread::spawn(move || {
                barrier.wait();
                let prepared = catalog.prepare_host_bind(
                    snapshot,
                    crate::PortCheckpoint {
                        family: AddressFamily::Inet4,
                        port: 9100,
                        owner,
                    },
                );
                observed.wait();
                prepared.is_ok()
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    observed.wait();
    assert_eq!(
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .filter(|won| *won)
            .count(),
        1
    );
}

#[test]
fn checkpoint_freeze_waits_for_prepared_bind() {
    let catalog = Arc::new(catalog());
    let owner = catalog
        .insert_host(
            CatalogScenario::socket(SocketState::Created, None),
            NetworkResourceKey::new(1).unwrap(),
            Arc::new(()),
            Vec::new(),
        )
        .unwrap();
    let mut bound = catalog.snapshot(owner).unwrap();
    bound.local = Some(SocketAddress::Inet4 {
        address: [127, 0, 0, 1],
        port: 9200,
    });
    bound.state = SocketState::Bound;
    let prepared = catalog
        .prepare_host_bind(
            bound,
            crate::PortCheckpoint {
                family: AddressFamily::Inet4,
                port: 9200,
                owner,
            },
        )
        .unwrap();
    let freezer_catalog = catalog.clone();
    let (frozen_send, frozen) = mpsc::channel();
    let freezer = thread::spawn(move || {
        freezer_catalog.freeze_checkpoint();
        frozen_send.send(()).unwrap();
        freezer_catalog.thaw_checkpoint();
    });
    assert!(frozen.recv_timeout(Duration::from_millis(20)).is_err());
    prepared.rollback().unwrap();
    frozen.recv_timeout(Duration::from_secs(1)).unwrap();
    freezer.join().unwrap();
}

#[test]
fn freeze_drains_admitted() {
    let catalog = Arc::new(catalog());
    let id = catalog
        .insert_host(
            CatalogScenario::socket(SocketState::Created, None),
            NetworkResourceKey::new(1).unwrap(),
            Arc::new(()),
            Vec::new(),
        )
        .unwrap();
    let (entered_send, entered) = mpsc::channel();
    let (release_send, release) = mpsc::channel();
    let worker_catalog = catalog.clone();
    let worker = thread::spawn(move || {
        worker_catalog
            .with_snapshot(id, |_| {
                entered_send.send(()).unwrap();
                release.recv().unwrap();
            })
            .unwrap();
    });
    entered.recv().unwrap();
    let freeze_catalog = catalog.clone();
    let (frozen_send, frozen) = mpsc::channel();
    let freezer = thread::spawn(move || {
        freeze_catalog.freeze_checkpoint();
        frozen_send.send(()).unwrap();
    });
    assert!(frozen.recv_timeout(Duration::from_millis(20)).is_err());
    release_send.send(()).unwrap();
    frozen.recv_timeout(Duration::from_secs(1)).unwrap();
    catalog.thaw_checkpoint();
    worker.join().unwrap();
    freezer.join().unwrap();
}

#[test]
fn pair_slots_transactional() {
    let pair_catalog = catalog();
    let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, hl_descriptor::StatusFlags::default()).unwrap());
    let ids = pair_catalog
        .insert_unix_pair([CatalogScenario::unix_socket(), CatalogScenario::unix_socket()], pair)
        .unwrap();
    assert_ne!(ids[0].slot, ids[1].slot);
    pair_catalog.freeze_checkpoint();
    let image = pair_catalog.checkpoint_image().unwrap();
    pair_catalog.thaw_checkpoint();
    assert_eq!(image.generations, vec![ids[0].generation, ids[1].generation]);

    let full = catalog();
    for key in 1..NETWORK_CHECKPOINT_SOCKET_MAXIMUM {
        full.insert_host(
            CatalogScenario::socket(SocketState::Created, None),
            NetworkResourceKey::new(key as u64).unwrap(),
            Arc::new(()),
            Vec::new(),
        )
        .unwrap();
    }
    full.freeze_checkpoint();
    let before = full.checkpoint_image().unwrap();
    full.thaw_checkpoint();
    let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, hl_descriptor::StatusFlags::default()).unwrap());
    assert_eq!(
        full.insert_unix_pair([CatalogScenario::unix_socket(), CatalogScenario::unix_socket()], pair,),
        Err(NetworkCatalogError::Capacity),
    );
    full.freeze_checkpoint();
    let after = full.checkpoint_image().unwrap();
    full.thaw_checkpoint();
    assert_eq!(after, before);
}

#[test]
fn standalone_generation() {
    let catalog = catalog();
    let stale = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
    catalog.remove(stale).unwrap();
    let current = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
    assert_eq!(current.slot, stale.slot);
    assert_ne!(current.generation, stale.generation);

    catalog.freeze_checkpoint();
    let image = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();
    assert_eq!(image.generations, vec![current.generation]);
    assert_eq!(
        image.sockets,
        vec![crate::NetworkSocketState::Unix {
            snapshot: catalog.snapshot(current).unwrap(),
            pending: Vec::new(),
            datagram: None,
        }]
    );
    let restored = NetworkCatalog::restore_checkpoint(&image, &mut StandaloneRestore).unwrap();
    assert_eq!(restored.snapshot(current), catalog.snapshot(current));
    assert_eq!(restored.snapshot(stale), Err(NetworkCatalogError::Stale));
}

#[test]
fn standalone_rejection() {
    let catalog = catalog();
    assert_eq!(
        catalog.insert_unix(CatalogScenario::socket(SocketState::Created, None)),
        Err(NetworkCatalogError::Invalid)
    );
    let mut invalid = CatalogScenario::standalone_unix_socket();
    invalid.state = SocketState::Bound;
    assert_eq!(catalog.insert_unix(invalid), Err(NetworkCatalogError::Invalid));
}

#[test]
fn standalone_datagram_checkpoint() {
    let catalog = catalog();
    let mut snapshot = CatalogScenario::standalone_unix_socket();
    snapshot.socket_type = SocketType::Datagram;
    let id = catalog.insert_unix(snapshot).unwrap();
    let datagram = catalog.unix_datagram(id).unwrap();
    datagram
        .enqueue(b"record", crate::UnixAddress::Abstract(b"source".to_vec()))
        .unwrap();
    catalog.freeze_checkpoint();
    let image = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();
    let restored = NetworkCatalog::restore_checkpoint(&image, &mut StandaloneRestore).unwrap();
    let restored = restored.unix_datagram(id).unwrap();
    let mut output = [0; 8];
    let received = restored.receive(&mut output, false).unwrap();
    assert_eq!(&output[..received.count], b"record");
    assert_eq!(received.source, crate::UnixAddress::Abstract(b"source".to_vec()));
}

#[test]
fn standalone_datagram_image_requires_queue() {
    let catalog = catalog();
    let mut snapshot = CatalogScenario::standalone_unix_socket();
    snapshot.socket_type = SocketType::Datagram;
    catalog.insert_unix(snapshot).unwrap();
    catalog.freeze_checkpoint();
    let mut image = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();
    let NetworkSocketState::Unix { datagram, .. } = &mut image.sockets[0] else {
        panic!("standalone datagram checkpoint");
    };
    *datagram = None;
    assert_eq!(image.validate(), Err(NetworkCheckpointError::InvalidImage));
}

#[test]
fn named_pair_conversion() {
    let catalog = catalog();
    let listener = catalog.insert_unix(CatalogScenario::unix_listener()).unwrap();
    let client = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
    let mut client_snapshot = CatalogScenario::unix_socket();
    client_snapshot.id = client;
    let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, hl_descriptor::StatusFlags::default()).unwrap());
    let accepted = catalog
        .connect_unix_pair(
            listener,
            client,
            client_snapshot.clone(),
            CatalogScenario::unix_socket(),
            pair.clone(),
        )
        .unwrap();

    let mut accepted_snapshot = CatalogScenario::unix_socket();
    accepted_snapshot.id = accepted;
    assert_eq!(catalog.snapshot(client), Ok(client_snapshot.clone()));
    assert_eq!(catalog.snapshot(accepted), Ok(accepted_snapshot.clone()));
    catalog.freeze_checkpoint();
    let image = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();
    assert_eq!(
        image.sockets,
        vec![
            NetworkSocketState::Unix {
                snapshot: catalog.snapshot(listener).unwrap(),
                pending: vec![accepted],
                datagram: None,
            },
            NetworkSocketState::UnixPair {
                endpoints: [client_snapshot, accepted_snapshot],
                pair: pair.snapshot(),
            },
        ]
    );
    let restored = NetworkCatalog::restore_checkpoint(&image, &mut StandaloneRestore).unwrap();
    assert_eq!(restored.accept_pending_unix(listener), Ok(accepted));
}

#[test]
fn named_pair_transaction() {
    let catalog = catalog();
    let listener = catalog.insert_unix(CatalogScenario::unix_listener()).unwrap();
    let client = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
    catalog.freeze_checkpoint();
    let before = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();

    let mut client_snapshot = CatalogScenario::unix_socket();
    client_snapshot.id = client;
    let mut invalid_accepted = CatalogScenario::unix_socket();
    invalid_accepted.peer = Some(SocketAddress::Unix(b"different".to_vec()));
    let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, hl_descriptor::StatusFlags::default()).unwrap());
    assert_eq!(
        catalog.connect_unix_pair(listener, client, client_snapshot, invalid_accepted, pair),
        Err(NetworkCatalogError::Invalid)
    );

    catalog.freeze_checkpoint();
    let after = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();
    assert_eq!(after, before);
    let next = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
    assert_eq!(next.slot, 3);
    assert_eq!(next.generation, 1);
}

#[test]
fn named_pair_stale() {
    let catalog = catalog();
    let listener = catalog.insert_unix(CatalogScenario::unix_listener()).unwrap();
    let stale = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
    catalog.remove(stale).unwrap();
    let current = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
    let mut client_snapshot = CatalogScenario::unix_socket();
    client_snapshot.id = stale;
    let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, hl_descriptor::StatusFlags::default()).unwrap());
    assert_eq!(
        catalog.connect_unix_pair(listener, stale, client_snapshot, CatalogScenario::unix_socket(), pair),
        Err(NetworkCatalogError::Stale)
    );
    let next = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
    assert_eq!(next.slot, 3);
    assert_eq!(catalog.snapshot(current).unwrap().state, SocketState::Created);
}

#[test]
fn named_pair_capacity() {
    let catalog = catalog();
    let listener = catalog.insert_unix(CatalogScenario::unix_listener()).unwrap();
    let client = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
    for key in 1..(NETWORK_CHECKPOINT_SOCKET_MAXIMUM - 1) {
        catalog
            .insert_host(
                CatalogScenario::socket(SocketState::Created, None),
                NetworkResourceKey::new(key as u64).unwrap(),
                Arc::new(()),
                Vec::new(),
            )
            .unwrap();
    }
    let before = catalog.snapshot(client).unwrap();
    let mut client_snapshot = CatalogScenario::unix_socket();
    client_snapshot.id = client;
    let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, hl_descriptor::StatusFlags::default()).unwrap());
    assert_eq!(
        catalog.connect_unix_pair(listener, client, client_snapshot, CatalogScenario::unix_socket(), pair),
        Err(NetworkCatalogError::Capacity)
    );
    assert_eq!(catalog.snapshot(client), Ok(before));
}

#[test]
fn named_pending_fifo() {
    let catalog = catalog();
    let listener = catalog.insert_unix(CatalogScenario::unix_listener()).unwrap();
    let mut accepted = Vec::new();
    for _ in 0..2 {
        let client = catalog.insert_unix(CatalogScenario::standalone_unix_socket()).unwrap();
        let mut client_snapshot = CatalogScenario::unix_socket();
        client_snapshot.id = client;
        let pair = Arc::new(UnixSocketPair::new(SocketType::Stream, hl_descriptor::StatusFlags::default()).unwrap());
        accepted.push(
            catalog
                .connect_unix_pair(listener, client, client_snapshot, CatalogScenario::unix_socket(), pair)
                .unwrap(),
        );
    }
    assert_eq!(catalog.accept_pending_unix(listener), Ok(accepted[0]));
    assert_eq!(catalog.accept_pending_unix(listener), Ok(accepted[1]));
    assert_eq!(catalog.accept_pending_unix(listener), Err(NetworkCatalogError::Stale));
}

#[test]
fn named_pending_invalid_reference() {
    let catalog = catalog();
    let listener = catalog.insert_unix(CatalogScenario::unix_listener()).unwrap();
    catalog.freeze_checkpoint();
    let mut image = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();
    let NetworkSocketState::Unix { pending, .. } = &mut image.sockets[0] else {
        panic!("listener checkpoint");
    };
    pending.push(SocketId { slot: 2, generation: 1 });
    assert_eq!(image.validate(), Err(NetworkCheckpointError::InvalidImage));
    assert_eq!(
        NetworkCatalog::restore_checkpoint(&image, &mut StandaloneRestore).err(),
        Some(NetworkCatalogError::Checkpoint(NetworkCheckpointError::InvalidImage))
    );
    assert_eq!(catalog.accept_pending_unix(listener), Err(NetworkCatalogError::Stale));
}
