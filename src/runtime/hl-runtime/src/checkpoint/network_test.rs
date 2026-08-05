use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_checkpoint::{Section, SectionKind};
use hl_descriptor::{DescriptionRef, DescriptorFlags, OpenFileDescription, StatusFlags};
use hl_network::{
    AcceptedSocketCheckpoint, AddressFamily, ControlMessage, NETWORK_CHECKPOINT_VERSION, NetworkCatalog,
    NetworkCatalogRestore, NetworkCheckpointError, NetworkCheckpointImage, NetworkCheckpointRebind,
    NetworkConfiguration, NetworkResourceKey, NetworkSocketResource, PortCheckpoint, ShutdownState, SocketAddress,
    SocketConnectStatus, SocketDescription, SocketHostError, SocketHostIo, SocketHostReadiness, SocketId,
    SocketProtocol, SocketSnapshot, SocketState, SocketType, UnixSocketPair,
};

use crate::{
    CheckpointDescriptorTable, CheckpointNetworkCatalog, CheckpointParticipant, DescriptorCheckpointParticipant,
    DescriptorObjectCatalog, NetworkCheckpointCodec, NetworkCheckpointHost, NetworkCheckpointParticipant,
    NetworkObjectBindings, PortableNetworkCodec, RuntimeNetworkError, RuntimeNetworkHost, RuntimeSocket,
    RuntimeSocketKind, RuntimeSocketRegistry,
};

#[derive(Default)]
struct Codec {
    next: AtomicU64,
    images: Mutex<BTreeMap<u64, NetworkCheckpointImage>>,
}

impl NetworkCheckpointCodec for Codec {
    fn encode(&self, image: &NetworkCheckpointImage) -> Result<Vec<u8>, ()> {
        let key = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.images.lock().map_err(|_| ())?.insert(key, image.clone());
        Ok(key.to_le_bytes().to_vec())
    }

    fn decode(&self, bytes: &[u8]) -> Result<NetworkCheckpointImage, ()> {
        let key = u64::from_le_bytes(bytes.try_into().map_err(|_| ())?);
        self.images.lock().map_err(|_| ())?.get(&key).cloned().ok_or(())
    }
}

#[derive(Default)]
struct State {
    failure: AtomicUsize,
    reconnects: AtomicUsize,
    rollbacks: AtomicUsize,
    closed: AtomicUsize,
}

struct Rebind(Arc<State>);
struct Transaction(Arc<State>);

impl NetworkCheckpointRebind for Rebind {
    fn stage(&self, _: &NetworkCheckpointImage) -> Result<Box<dyn NetworkCatalogRestore>, NetworkCheckpointError> {
        if self.0.failure.load(Ordering::Relaxed) == 1 {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        Ok(Box::new(Transaction(self.0.clone())))
    }
}

impl NetworkCatalogRestore for Transaction {
    fn host_socket(
        &mut self,
        _: &SocketSnapshot,
        _: NetworkResourceKey,
    ) -> Result<Arc<dyn NetworkSocketResource>, NetworkCheckpointError> {
        let count = self.0.reconnects.fetch_add(1, Ordering::Relaxed) + 1;
        if self.0.failure.load(Ordering::Relaxed) == 2 && count == 2 {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        Ok(Arc::new(()))
    }

    fn accepted_socket(&mut self, _: &AcceptedSocketCheckpoint) -> Result<(), NetworkCheckpointError> {
        Ok(())
    }

    fn descriptor(&mut self, _: u64) -> Result<DescriptionRef, NetworkCheckpointError> {
        Err(NetworkCheckpointError::InvalidImage)
    }

    fn commit(&mut self) -> Result<(), NetworkCheckpointError> {
        if self.0.failure.load(Ordering::Relaxed) == 3 {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn rollback(&mut self) {
        self.0.rollbacks.fetch_add(1, Ordering::Relaxed);
        let staged = self.0.reconnects.load(Ordering::Relaxed);
        self.0.closed.fetch_add(staged, Ordering::Relaxed);
    }

    fn resume(&mut self) -> Result<(), NetworkCheckpointError> {
        if self.0.failure.load(Ordering::Relaxed) == 4 {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        Ok(())
    }
}

struct NetworkScenario;

impl NetworkScenario {
    fn socket() -> SocketSnapshot {
        SocketSnapshot {
            id: SocketId { slot: 1, generation: 1 },
            family: AddressFamily::Inet4,
            socket_type: SocketType::Stream,
            protocol: SocketProtocol::Tcp,
            state: SocketState::Created,
            local: None,
            peer: None,
            connect_error: None,
            nonblocking: true,
            shutdown: ShutdownState::default(),
        }
    }

    fn fixture(count: usize) -> (Arc<CheckpointNetworkCatalog>, Arc<State>, NetworkCheckpointParticipant) {
        let catalog = Arc::new(NetworkCatalog::new(
            NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
        ));
        for key in 1..=count {
            catalog
                .insert_host(
                    Self::socket(),
                    NetworkResourceKey::new(key as u64).unwrap(),
                    Arc::new(()),
                    Vec::new(),
                )
                .unwrap();
        }
        let handle = Arc::new(CheckpointNetworkCatalog::new(catalog));
        let state = Arc::new(State::default());
        let participant = NetworkCheckpointParticipant::new(
            handle.clone(),
            Arc::new(Rebind(state.clone())),
            Arc::new(Codec::default()),
        );
        (handle, state, participant)
    }

    fn section(participant: &NetworkCheckpointParticipant) -> Section {
        participant.freeze().unwrap();
        let section = Section::new(
            SectionKind::new(6).unwrap(),
            NETWORK_CHECKPOINT_VERSION,
            participant.snapshot().unwrap(),
        );
        participant.thaw().unwrap();
        section
    }

    fn assert_transaction_failure(failure: usize) {
        let (handle, state, participant) = Self::fixture(1);
        let previous = handle.current();
        state.failure.store(failure, Ordering::Relaxed);
        let staged = participant.stage(&Self::section(&participant));
        if let Ok(reservation) = staged {
            let _ = participant
                .commit(reservation)
                .and_then(|()| participant.resume(reservation));
            participant.rollback(reservation);
            participant.rollback(reservation);
            assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
            assert_eq!(state.closed.load(Ordering::Relaxed), 1);
        }
        assert!(Arc::ptr_eq(&handle.current(), &previous));
    }
}

#[test]
fn network_rollback_exact() {
    let (handle, state, participant) = NetworkScenario::fixture(1);
    let previous = handle.current();
    let reservation = participant.stage(&NetworkScenario::section(&participant)).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&handle.current(), &previous));
    assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
}

#[test]
fn partial_back_publication() {
    let (handle, state, participant) = NetworkScenario::fixture(2);
    let previous = handle.current();
    state.failure.store(2, Ordering::Relaxed);
    assert!(participant.stage(&NetworkScenario::section(&participant)).is_err());
    assert!(Arc::ptr_eq(&handle.current(), &previous));
    assert_eq!(state.reconnects.load(Ordering::Relaxed), 2);
    assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
    assert_eq!(state.closed.load(Ordering::Relaxed), 2);
}

#[test]
fn successful_catalog_finish() {
    let (handle, _, participant) = NetworkScenario::fixture(1);
    let previous = handle.current();
    let weak = Arc::downgrade(&previous);
    let reservation = participant.stage(&NetworkScenario::section(&participant)).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    drop(previous);
    assert!(weak.upgrade().is_some());
    participant.finish(reservation);
    assert!(weak.upgrade().is_none());
}

#[test]
fn stage_previous_catalog() {
    for failure in [1, 3, 4] {
        NetworkScenario::assert_transaction_failure(failure);
    }
}

#[derive(Debug, Default)]
struct UnixRestoreHost {
    occupied_ports: Mutex<BTreeSet<(u8, u16)>>,
    staged_ports: Mutex<Vec<(u8, u16)>>,
    fail_reconnect: AtomicBool,
    reserve_calls: AtomicUsize,
    reconnects: AtomicUsize,
    reservation_rollbacks: AtomicUsize,
}

impl SocketHostIo for UnixRestoreHost {
    type Token = u64;

    fn read(&self, _: u64, _: &mut [u8], _: bool) -> Result<usize, SocketHostError> {
        Err(SocketHostError::Canceled)
    }
    fn write(&self, _: u64, _: &[u8], _: bool) -> Result<usize, SocketHostError> {
        Err(SocketHostError::Canceled)
    }
    fn readiness(&self, _: u64) -> SocketHostReadiness {
        SocketHostReadiness::default()
    }
    fn start_connect(&self, _: u64, _: bool) -> hl_network::SocketConnectStatus {
        hl_network::SocketConnectStatus::Idle
    }
    fn poll_connect(&self, _: u64) -> hl_network::SocketConnectStatus {
        hl_network::SocketConnectStatus::Idle
    }
    fn cancel(&self, _: u64) {}
    fn close(&self, _: u64) {}
}

impl RuntimeNetworkHost for UnixRestoreHost {
    type Attachment = ();

    fn create(
        &self,
        _: AddressFamily,
        _: SocketType,
        _: SocketProtocol,
    ) -> Result<crate::CreatedSocket<u64>, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn bind(&self, _: u64, _: SocketAddress) -> Result<SocketAddress, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn prepare_connect(&self, _: u64, _: SocketAddress) -> Result<(), RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn listen(&self, _: u64, _: u32) -> Result<(), RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn accept(&self, _: u64) -> Result<crate::AcceptedSocket<u64>, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn local_address(&self, _: u64) -> Result<SocketAddress, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn peer_address(&self, _: u64) -> Result<SocketAddress, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn send_to(&self, _: u64, _: &[u8], _: SocketAddress, _: bool) -> Result<usize, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn receive_from(
        &self,
        _: u64,
        _: &mut [u8],
        _: bool,
        _: bool,
    ) -> Result<crate::ReceivedDatagram, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn shutdown(&self, _: u64, _: bool, _: bool) -> Result<(), RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn set_option(&self, _: u64, _: i32, _: i32, _: hl_linux::GuestSocketOption) -> Result<(), RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn get_option(&self, _: u64, _: i32, _: i32) -> Result<hl_linux::GuestSocketOption, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
}

impl NetworkCheckpointHost for UnixRestoreHost {
    fn reserve_ports(&self, ports: &[PortCheckpoint]) -> Result<(), NetworkCheckpointError> {
        self.reserve_calls.fetch_add(1, Ordering::Relaxed);
        let occupied = self
            .occupied_ports
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        let candidate = ports
            .iter()
            .map(|port| (port.family as u8, port.port))
            .collect::<Vec<_>>();
        if candidate.iter().any(|port| occupied.contains(port)) {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        drop(occupied);
        let mut staged = self
            .staged_ports
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)?;
        if !staged.is_empty() {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        *staged = candidate;
        Ok(())
    }

    fn reconnect(
        &self,
        _: &SocketSnapshot,
        resource: NetworkResourceKey,
    ) -> Result<crate::ReconnectedSocket<u64>, NetworkCheckpointError> {
        self.reconnects.fetch_add(1, Ordering::Relaxed);
        if self.fail_reconnect.load(Ordering::Relaxed) {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        Ok(crate::ReconnectedSocket {
            token: resource.value(),
            binding: Arc::new(()),
        })
    }
    fn reconnect_accepted(
        &self,
        _: &AcceptedSocketCheckpoint,
    ) -> Result<crate::ReconnectedSocket<u64>, NetworkCheckpointError> {
        Err(NetworkCheckpointError::InvalidImage)
    }

    fn checkpoint_commit(&self) -> Result<(), NetworkCheckpointError> {
        let staged = std::mem::take(
            &mut *self
                .staged_ports
                .lock()
                .map_err(|_| NetworkCheckpointError::InvalidImage)?,
        );
        self.occupied_ports
            .lock()
            .map_err(|_| NetworkCheckpointError::InvalidImage)?
            .extend(staged);
        Ok(())
    }

    fn checkpoint_rollback(&self) {
        self.reservation_rollbacks.fetch_add(1, Ordering::Relaxed);
        self.staged_ports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

fn unix_snapshot() -> SocketSnapshot {
    SocketSnapshot {
        id: SocketId { slot: 1, generation: 1 },
        family: AddressFamily::Unix,
        socket_type: SocketType::Stream,
        protocol: SocketProtocol::Default,
        state: SocketState::Connected,
        local: Some(SocketAddress::Unix(Vec::new())),
        peer: Some(SocketAddress::Unix(Vec::new())),
        connect_error: None,
        nonblocking: true,
        shutdown: ShutdownState::default(),
    }
}

fn capture_sections(
    descriptor: &DescriptorCheckpointParticipant,
    network: &NetworkCheckpointParticipant,
) -> Result<(Section, Section), ()> {
    descriptor.freeze()?;
    if network.freeze().is_err() {
        let _ = descriptor.thaw();
        return Err(());
    }
    let result = (|| {
        let descriptor = Section::new(
            SectionKind::new(2).map_err(|_| ())?,
            descriptor.version(),
            descriptor.snapshot()?,
        );
        let network = Section::new(
            SectionKind::new(6).map_err(|_| ())?,
            network.version(),
            network.snapshot()?,
        );
        Ok((descriptor, network))
    })();
    let network_thaw = network.thaw();
    let descriptor_thaw = descriptor.thaw();
    if network_thaw.is_err() || descriptor_thaw.is_err() {
        return Err(());
    }
    result
}

#[test]
fn unix_pair_restore() {
    let table = Arc::new(hl_descriptor::DescriptorTable::new(16).unwrap());
    let descriptors = Arc::new(CheckpointDescriptorTable::new(table.clone()));
    let catalog = Arc::new(NetworkCatalog::new(
        NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
    ));
    let checkpoint_catalog = Arc::new(CheckpointNetworkCatalog::new(catalog.clone()));
    let sockets = Arc::new(RuntimeSocketRegistry::<UnixRestoreHost>::default());
    let bindings = Arc::new(NetworkObjectBindings::new(descriptors.clone(), sockets.clone(), None));
    let pair =
        Arc::new(UnixSocketPair::new(SocketType::Stream, StatusFlags::from_bits(StatusFlags::NONBLOCKING)).unwrap());
    let mut snapshots = [unix_snapshot(), unix_snapshot()];
    let ids = catalog.insert_unix_pair(snapshots.clone(), pair.clone()).unwrap();
    snapshots[0].id = ids[0];
    snapshots[1].id = ids[1];
    let objects = RuntimeSocket::unix_pair(pair.clone(), ids, snapshots, catalog);
    let opened = table
        .prepare_open_batch(
            0,
            vec![
                (
                    objects[0].clone() as Arc<dyn OpenFileDescription>,
                    StatusFlags::from_bits(StatusFlags::NONBLOCKING),
                    DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
                ),
                (
                    objects[1].clone() as Arc<dyn OpenFileDescription>,
                    StatusFlags::from_bits(StatusFlags::NONBLOCKING),
                    DescriptorFlags::default(),
                ),
            ],
        )
        .unwrap();
    let identities = opened.description_identities();
    sockets.register(identities[0], objects[0].clone()).unwrap();
    sockets.register(identities[1], objects[1].clone()).unwrap();
    let numbers = opened.publish_all();
    let alias = table.duplicate(numbers[0], 2, DescriptorFlags::default()).unwrap();
    pair.endpoints[0].description.write(b"queued-bytes").unwrap();
    pair.endpoints[0]
        .send_message(
            &table,
            b"R".to_vec(),
            vec![ControlMessage::Rights(vec![numbers[0]])],
            None,
            true,
        )
        .unwrap();

    let object_catalog =
        Arc::new(DescriptorObjectCatalog::rejecting().bind(hl_descriptor::ObjectKind::Socket, bindings.clone()));
    let descriptor_participant = DescriptorCheckpointParticipant::new(descriptors.clone(), object_catalog);
    let network_participant =
        NetworkCheckpointParticipant::new(checkpoint_catalog, bindings, Arc::new(PortableNetworkCodec));
    let (descriptor_section, network_section) =
        capture_sections(&descriptor_participant, &network_participant).unwrap();
    let descriptor_reservation = descriptor_participant.stage(&descriptor_section).unwrap();
    let network_reservation = network_participant.stage(&network_section).unwrap();
    descriptor_participant.commit(descriptor_reservation).unwrap();
    network_participant.commit(network_reservation).unwrap();
    descriptor_participant.resume(descriptor_reservation).unwrap();
    network_participant.resume(network_reservation).unwrap();
    network_participant.finish(network_reservation);

    let restored = descriptors.current();
    assert_eq!(
        restored.snapshot(numbers[0]).unwrap().description_identity,
        restored.snapshot(alias).unwrap().description_identity,
    );
    assert!(restored.snapshot(numbers[0]).unwrap().flags.closes_on_exec());
    assert!(!restored.snapshot(alias).unwrap().flags.closes_on_exec());
    let endpoint_identity = restored.snapshot(numbers[1]).unwrap().description_identity;
    let endpoint = sockets
        .get(hl_descriptor::DescriptionIdentity {
            identity: endpoint_identity,
            generation: restored.snapshot(numbers[1]).unwrap().description_generation,
        })
        .unwrap();
    let RuntimeSocketKind::Unix { pair, endpoint } = &endpoint.kind else {
        panic!("restored Unix endpoint expected");
    };
    let mut bytes = [0; 12];
    assert_eq!(restored.pin(numbers[1]).unwrap().read(&mut bytes).unwrap(), 12);
    assert_eq!(&bytes, b"queued-bytes");
    let (_, rights) = pair.endpoints[*endpoint]
        .receive_message(&restored, 1, false)
        .unwrap()
        .unwrap();
    assert_eq!(rights.descriptors.len(), 1);
    assert_eq!(
        restored.snapshot(rights.descriptors[0]).unwrap().description_identity,
        restored.snapshot(numbers[0]).unwrap().description_identity,
    );
}

mod port_restore {
    use super::*;

    #[test]
    fn transactional() {
        let table = Arc::new(hl_descriptor::DescriptorTable::new(16).unwrap());
        let descriptors = Arc::new(CheckpointDescriptorTable::new(table.clone()));
        let catalog = Arc::new(NetworkCatalog::new(
            NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
        ));
        let checkpoint_catalog = Arc::new(CheckpointNetworkCatalog::new(catalog.clone()));
        let sockets = Arc::new(RuntimeSocketRegistry::<UnixRestoreHost>::default());
        let host = Arc::new(UnixRestoreHost::default());
        let bindings = Arc::new(NetworkObjectBindings::new(
            descriptors.clone(),
            sockets.clone(),
            Some(host.clone()),
        ));

        let mut objects = Vec::new();
        for (offset, port) in [8080_u16, 8081].into_iter().enumerate() {
            let mut snapshot = SocketSnapshot {
                id: SocketId { slot: 1, generation: 1 },
                family: AddressFamily::Inet4,
                socket_type: SocketType::Stream,
                protocol: SocketProtocol::Tcp,
                state: SocketState::Bound,
                local: Some(SocketAddress::Inet4 {
                    address: [127, 0, 0, 1],
                    port,
                }),
                peer: None,
                connect_error: None,
                nonblocking: true,
                shutdown: ShutdownState::default(),
            };
            let resource = NetworkResourceKey::new(u64::try_from(offset + 1).unwrap()).unwrap();
            let id = catalog
                .insert_host(snapshot.clone(), resource, Arc::new(()), Vec::new())
                .unwrap();
            snapshot.id = id;
            catalog
                .claim_port(PortCheckpoint {
                    family: AddressFamily::Inet4,
                    port,
                    owner: id,
                })
                .unwrap();
            let description = Arc::new(SocketDescription::restored(
                host.clone(),
                resource.value(),
                StatusFlags::from_bits(StatusFlags::NONBLOCKING),
                SocketConnectStatus::Idle,
            ));
            description.bind_readiness();
            objects.push(RuntimeSocket::host(
                description,
                resource.value(),
                id,
                snapshot,
                catalog.clone(),
            ));
        }
        let opened = table
            .prepare_open_batch(
                0,
                objects
                    .iter()
                    .map(|object| {
                        (
                            object.clone() as Arc<dyn OpenFileDescription>,
                            StatusFlags::from_bits(StatusFlags::NONBLOCKING),
                            DescriptorFlags::default(),
                        )
                    })
                    .collect(),
            )
            .unwrap();
        let identities = opened.description_identities();
        for (identity, object) in identities.into_iter().zip(&objects) {
            sockets.register(identity, object.clone()).unwrap();
        }
        let numbers = opened.publish_all();

        let object_catalog =
            Arc::new(DescriptorObjectCatalog::rejecting().bind(hl_descriptor::ObjectKind::Socket, bindings.clone()));
        let descriptor_participant = Arc::new(DescriptorCheckpointParticipant::new(
            descriptors.clone(),
            object_catalog,
        ));
        let network_participant = Arc::new(NetworkCheckpointParticipant::new(
            checkpoint_catalog.clone(),
            bindings,
            Arc::new(PortableNetworkCodec),
        ));
        let (descriptor_section, network_section) =
            capture_sections(&descriptor_participant, &network_participant).unwrap();

        let previous_table = descriptors.current();
        let previous_catalog = checkpoint_catalog.current();
        let previous_descriptors = numbers
            .iter()
            .map(|number| previous_table.snapshot(*number).unwrap())
            .collect::<Vec<_>>();
        let (previous_generation, previous_registry) = sockets.checkpoint_lease();
        let expected_image = PortableNetworkCodec.decode(network_section.bytes()).unwrap();
        let assert_unchanged = || {
            assert!(Arc::ptr_eq(&descriptors.current(), &previous_table));
            assert!(Arc::ptr_eq(&checkpoint_catalog.current(), &previous_catalog));
            for (number, expected) in numbers.iter().zip(&previous_descriptors) {
                assert_eq!(descriptors.current().snapshot(*number).unwrap(), *expected);
            }
            let (generation, registry) = sockets.checkpoint_lease();
            assert_eq!(generation, previous_generation);
            assert_eq!(registry.len(), previous_registry.len());
            for (identity, expected) in &previous_registry {
                assert!(registry.get(identity).is_some_and(|value| Arc::ptr_eq(value, expected)));
            }
            previous_catalog.freeze_checkpoint();
            assert_eq!(previous_catalog.checkpoint_image().unwrap(), expected_image);
            previous_catalog.thaw_checkpoint();
        };

        host.occupied_ports
            .lock()
            .unwrap()
            .insert((AddressFamily::Inet4 as u8, 8081));
        let descriptor_reservation = descriptor_participant.stage(&descriptor_section).unwrap();
        assert!(network_participant.stage(&network_section).is_err());
        descriptor_participant.rollback(descriptor_reservation);
        assert_unchanged();
        assert_eq!(host.reserve_calls.load(Ordering::Relaxed), 1);
        assert_eq!(host.reconnects.load(Ordering::Relaxed), 0);
        assert_eq!(host.reservation_rollbacks.load(Ordering::Relaxed), 1);
        assert!(host.staged_ports.lock().unwrap().is_empty());
        assert_eq!(host.occupied_ports.lock().unwrap().len(), 1);

        host.occupied_ports.lock().unwrap().clear();
        host.fail_reconnect.store(true, Ordering::Relaxed);
        let descriptor_reservation = descriptor_participant.stage(&descriptor_section).unwrap();
        assert!(network_participant.stage(&network_section).is_err());
        descriptor_participant.rollback(descriptor_reservation);
        assert_unchanged();
        assert_eq!(host.reserve_calls.load(Ordering::Relaxed), 2);
        assert_eq!(host.reconnects.load(Ordering::Relaxed), 1);
        assert_eq!(host.reservation_rollbacks.load(Ordering::Relaxed), 2);
        assert!(host.staged_ports.lock().unwrap().is_empty());

        host.fail_reconnect.store(false, Ordering::Relaxed);
        let descriptor_reservation = descriptor_participant.stage(&descriptor_section).unwrap();
        let network_reservation = network_participant.stage(&network_section).unwrap();
        let (finished, completed) = std::sync::mpsc::channel();
        let rollback = network_participant.clone();
        std::thread::spawn(move || {
            rollback.rollback(network_reservation);
            let _ = finished.send(());
        });
        completed
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("network rollback deadlocked on its frozen replacement catalog");
        descriptor_participant.rollback(descriptor_reservation);
        assert_unchanged();
        assert_eq!(host.reserve_calls.load(Ordering::Relaxed), 3);
        assert_eq!(host.reconnects.load(Ordering::Relaxed), 3);
        assert_eq!(host.reservation_rollbacks.load(Ordering::Relaxed), 3);
        assert!(host.staged_ports.lock().unwrap().is_empty());
    }
}
