use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hl_descriptor::{DescriptorError, DescriptorFlags, ObjectError, StatusFlags};
use hl_event::{EventFd, EventFdFlags};
use hl_memory::{MappingCoordinator, SharedLimits, SharedObjectStore, TestMappingHost};
use hl_provider::{HandleKind, HandleNamespace, NamespaceError, RemoteId};
use hl_task::{
    ForkCloneFlags, ForkEntityId, ForkRequest, ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry,
};

use crate::{
    Control, DescriptorForkParticipant, ForkCancellation, ForkContext, ForkCoordinator, ForkParticipant,
    ForkParticipantRole, ForkPhase, MemoryForkHost, MemoryForkParticipant, PrivateFutexReset, ProviderForkParticipant,
    TaskForkParticipant,
};

#[derive(Debug)]
struct NeverCancel;
impl ForkCancellation for NeverCancel {
    fn cancelled(&self) -> bool {
        false
    }
}

struct Passive(ForkParticipantRole, Option<ForkPhase>);

impl ForkParticipant for Passive {
    fn role(&self) -> ForkParticipantRole {
        self.0
    }
    fn prepare(&self, _: ForkContext) -> Result<u64, ()> {
        Ok(self.0 as u64 + 1)
    }
    fn freeze(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn clone_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn clone_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        if self.1 == Some(ForkPhase::CloneChild) {
            Err(())
        } else {
            Ok(())
        }
    }
    fn repair_parent(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn repair_child(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn commit(&self, _: ForkContext, _: u64) -> Result<(), ()> {
        if self.1 == Some(ForkPhase::Commit) {
            Err(())
        } else {
            Ok(())
        }
    }
    fn rollback(&self, _: ForkContext, _: u64) {}
}

struct HostFactory;
impl MemoryForkHost<TestMappingHost> for HostFactory {
    fn child_host(&self, _: ForkContext) -> Result<TestMappingHost, ()> {
        Ok(TestMappingHost)
    }
}

#[derive(Default)]
struct Reset(AtomicUsize);
impl PrivateFutexReset for Reset {
    fn reset_private_futexes(&self, _: ForkContext) -> Result<(), ()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

struct RuntimeFixture;

impl RuntimeFixture {
    fn request(flags: ForkCloneFlags) -> ForkRequest {
        ForkRequest {
            parent: ForkEntityId { slot: 1, generation: 1 },
            child: ForkEntityId { slot: 2, generation: 1 },
            flags,
        }
    }
}

#[test]
fn descriptor_preserves_cloexec() {
    for (flags, shared) in [(ForkCloneFlags::default(), false), (ForkCloneFlags::FILES, true)] {
        let (control, parent) = Control::new(64, 64).unwrap();
        let control = Arc::new(control);
        let parent = Arc::new(parent);
        let number = control
            .create_epoll(&parent, DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC))
            .unwrap();
        let participant = Arc::new(DescriptorForkParticipant::new(control.clone(), parent.clone()));
        let ports: Vec<Arc<dyn ForkParticipant>> = vec![
            Arc::new(Passive(ForkParticipantRole::Task, None)),
            participant.clone(),
            Arc::new(Passive(ForkParticipantRole::Memory, None)),
            Arc::new(Passive(ForkParticipantRole::Provider, None)),
            Arc::new(Passive(ForkParticipantRole::Execution, None)),
            Arc::new(Passive(ForkParticipantRole::Network, None)),
            Arc::new(Passive(ForkParticipantRole::Event, None)),
            Arc::new(Passive(ForkParticipantRole::Ipc, None)),
        ];
        let outcome = ForkCoordinator::new(ports)
            .unwrap()
            .fork(RuntimeFixture::request(flags), &NeverCancel)
            .unwrap();
        let child = participant.child(outcome.context.transaction).unwrap();
        assert!(control.snapshot(&child, number).unwrap().flags.closes_on_exec());
        control.close(&child, number).unwrap();
        assert_eq!(
            control.snapshot(&parent, number).map(|_| ()),
            if shared {
                Err(crate::ControlError::Descriptor(DescriptorError::BadDescriptor))
            } else {
                Ok(())
            }
        );
    }
}

#[test]
fn copied_table_close() {
    let (control, parent) = Control::new(64, 64).unwrap();
    let control = Arc::new(control);
    let parent = Arc::new(parent);
    let source = control.create_epoll(&parent, DescriptorFlags::default()).unwrap();
    let target = control.create_epoll(&parent, DescriptorFlags::default()).unwrap();
    control
        .add(
            &parent,
            source,
            target,
            hl_event::EpollInterest::from_bits(hl_event::EpollInterest::READ),
            7,
        )
        .unwrap();
    let participant = Arc::new(DescriptorForkParticipant::new(control.clone(), parent.clone()));
    let ports: Vec<Arc<dyn ForkParticipant>> = vec![
        Arc::new(Passive(ForkParticipantRole::Task, None)),
        participant.clone(),
        Arc::new(Passive(ForkParticipantRole::Memory, None)),
        Arc::new(Passive(ForkParticipantRole::Provider, None)),
        Arc::new(Passive(ForkParticipantRole::Execution, None)),
        Arc::new(Passive(ForkParticipantRole::Network, None)),
        Arc::new(Passive(ForkParticipantRole::Event, None)),
        Arc::new(Passive(ForkParticipantRole::Ipc, None)),
    ];
    let outcome = ForkCoordinator::new(ports)
        .unwrap()
        .fork(RuntimeFixture::request(ForkCloneFlags::default()), &NeverCancel)
        .unwrap();
    let child = participant.child(outcome.context.transaction).unwrap();
    let before = control.graph_snapshot();
    control.close(&child, source).unwrap();
    assert_eq!(control.graph_snapshot(), before);
    control.close(&parent, source).unwrap();
    assert!(control.graph_snapshot().edges.is_empty());
}

#[test]
fn eventfd_shared_ofd() {
    let (control, parent) = Control::new(8, 8).unwrap();
    let control = Arc::new(control);
    let parent = Arc::new(parent);
    let object = Arc::new(EventFd::new(17, EventFdFlags::default()).unwrap());
    let notifications = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&notifications);
    let _subscription = object
        .subscribe(
            11,
            Arc::new(move |token| {
                assert_eq!(token, 11);
                observed.fetch_add(1, Ordering::AcqRel);
            }),
        )
        .unwrap();
    let number = parent
        .descriptor_table()
        .install(0, object.clone(), DescriptorFlags::default())
        .unwrap();
    let participant = Arc::new(DescriptorForkParticipant::new(control.clone(), parent.clone()));
    let ports: Vec<Arc<dyn ForkParticipant>> = vec![
        Arc::new(Passive(ForkParticipantRole::Task, None)),
        participant.clone(),
        Arc::new(Passive(ForkParticipantRole::Memory, None)),
        Arc::new(Passive(ForkParticipantRole::Provider, None)),
        Arc::new(Passive(ForkParticipantRole::Execution, None)),
        Arc::new(Passive(ForkParticipantRole::Network, None)),
        Arc::new(Passive(ForkParticipantRole::Event, None)),
        Arc::new(Passive(ForkParticipantRole::Ipc, None)),
    ];
    let outcome = ForkCoordinator::new(ports)
        .unwrap()
        .fork(RuntimeFixture::request(ForkCloneFlags::default()), &NeverCancel)
        .unwrap();
    let child = participant.take_child(outcome.context.transaction).unwrap();
    let child_table = child.descriptor_table();
    let mut value = [0_u8; 8];
    assert_eq!(child_table.pin(number).unwrap().read(&mut value), Ok(8));
    assert_eq!(u64::from_ne_bytes(value), 17);
    assert_eq!(object.counter(), 0);
    assert_eq!(notifications.load(Ordering::Acquire), 1);

    child_table
        .pin(number)
        .unwrap()
        .set_status(StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    assert_eq!(
        parent.descriptor_table().pin(number).unwrap().read(&mut value),
        Err(ObjectError::WouldBlock),
    );
    child_table.close(number).unwrap();
    assert!(!object.is_retired());
    object.write(&3_u64.to_ne_bytes()).unwrap();
    assert_eq!(parent.descriptor_table().pin(number).unwrap().read(&mut value), Ok(8));
    assert_eq!(u64::from_ne_bytes(value), 3);
    parent.descriptor_table().close(number).unwrap();
    assert!(object.is_retired());
}

#[test]
fn eventfd_rollback_lifetime() {
    let (control, parent) = Control::new(8, 8).unwrap();
    let control = Arc::new(control);
    let parent = Arc::new(parent);
    let object = Arc::new(EventFd::new(9, EventFdFlags::default()).unwrap());
    let number = parent
        .descriptor_table()
        .install(0, object.clone(), DescriptorFlags::default())
        .unwrap();
    let participant = Arc::new(DescriptorForkParticipant::new(control, parent.clone()));
    let ports: Vec<Arc<dyn ForkParticipant>> = vec![
        Arc::new(Passive(ForkParticipantRole::Task, None)),
        participant.clone(),
        Arc::new(Passive(ForkParticipantRole::Memory, None)),
        Arc::new(Passive(ForkParticipantRole::Provider, Some(ForkPhase::CloneChild))),
        Arc::new(Passive(ForkParticipantRole::Execution, None)),
        Arc::new(Passive(ForkParticipantRole::Network, None)),
        Arc::new(Passive(ForkParticipantRole::Event, None)),
        Arc::new(Passive(ForkParticipantRole::Ipc, None)),
    ];
    assert!(
        ForkCoordinator::new(ports)
            .unwrap()
            .fork(RuntimeFixture::request(ForkCloneFlags::default()), &NeverCancel)
            .is_err()
    );
    assert_eq!(participant.staged_count(), 0);
    assert!(participant.child(1).is_none());
    assert_eq!(
        parent
            .descriptor_table()
            .snapshot(number)
            .unwrap()
            .descriptor_references,
        1
    );
    assert_eq!(object.counter(), 9);
    assert!(!object.is_retired());
}

#[test]
fn memory_shared_pins() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(1, 4096).unwrap();
    let parent = Arc::new(MappingCoordinator::with_shared(TestMappingHost, store.clone()));
    parent.map(TestMappingHost::shared_request(object)).unwrap();
    assert_eq!(store.pin_count(object), Ok(1));

    for (flags, expected_pins, resets) in [(ForkCloneFlags::VM, 1, 0), (ForkCloneFlags::default(), 2, 1)] {
        let reset = Arc::new(Reset::default());
        let participant = Arc::new(MemoryForkParticipant::new(parent.clone(), HostFactory, reset.clone()));
        let ports: Vec<Arc<dyn ForkParticipant>> = vec![
            Arc::new(Passive(ForkParticipantRole::Task, None)),
            Arc::new(Passive(ForkParticipantRole::Descriptors, None)),
            participant.clone(),
            Arc::new(Passive(ForkParticipantRole::Provider, None)),
            Arc::new(Passive(ForkParticipantRole::Execution, None)),
            Arc::new(Passive(ForkParticipantRole::Network, None)),
            Arc::new(Passive(ForkParticipantRole::Event, None)),
            Arc::new(Passive(ForkParticipantRole::Ipc, None)),
        ];
        let outcome = ForkCoordinator::new(ports)
            .unwrap()
            .fork(RuntimeFixture::request(flags), &NeverCancel)
            .unwrap();
        let child = participant.take_child(outcome.context.transaction).unwrap();
        assert_eq!(child.ledger().regions(), parent.ledger().regions());
        assert_eq!(store.pin_count(object), Ok(expected_pins));
        assert_eq!(reset.0.load(Ordering::Relaxed), resets);
        drop(child);
        assert_eq!(store.pin_count(object), Ok(1));
    }
}

#[test]
fn downstream_pins_exactly() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(1, 4096).unwrap();
    let parent = Arc::new(MappingCoordinator::with_shared(TestMappingHost, store.clone()));
    parent.map(TestMappingHost::shared_request(object)).unwrap();
    let participant = Arc::new(MemoryForkParticipant::new(
        parent,
        HostFactory,
        Arc::new(Reset::default()),
    ));
    let ports: Vec<Arc<dyn ForkParticipant>> = vec![
        Arc::new(Passive(ForkParticipantRole::Task, None)),
        Arc::new(Passive(ForkParticipantRole::Descriptors, None)),
        participant.clone(),
        Arc::new(Passive(ForkParticipantRole::Provider, Some(ForkPhase::CloneChild))),
        Arc::new(Passive(ForkParticipantRole::Execution, None)),
        Arc::new(Passive(ForkParticipantRole::Network, None)),
        Arc::new(Passive(ForkParticipantRole::Event, None)),
        Arc::new(Passive(ForkParticipantRole::Ipc, None)),
    ];
    assert!(
        ForkCoordinator::new(ports)
            .unwrap()
            .fork(RuntimeFixture::request(ForkCloneFlags::default()), &NeverCancel)
            .is_err()
    );
    assert_eq!(participant.staged_count(), 0);
    assert_eq!(store.pin_count(object), Ok(1));
}

#[test]
fn provider_parent_child() {
    let parent = Arc::new(HandleNamespace::new(1).unwrap());
    let remote = RemoteId::new(71).unwrap();
    let handle = parent.open(remote, HandleKind::File).unwrap();
    let participant = Arc::new(ProviderForkParticipant::new(parent.clone()));
    let ports: Vec<Arc<dyn ForkParticipant>> = vec![
        Arc::new(Passive(ForkParticipantRole::Task, None)),
        Arc::new(Passive(ForkParticipantRole::Descriptors, None)),
        Arc::new(Passive(ForkParticipantRole::Memory, None)),
        participant.clone(),
        Arc::new(Passive(ForkParticipantRole::Execution, None)),
        Arc::new(Passive(ForkParticipantRole::Network, None)),
        Arc::new(Passive(ForkParticipantRole::Event, None)),
        Arc::new(Passive(ForkParticipantRole::Ipc, None)),
    ];
    let outcome = ForkCoordinator::new(ports)
        .unwrap()
        .fork(RuntimeFixture::request(ForkCloneFlags::default()), &NeverCancel)
        .unwrap();
    let child = participant.take_child(outcome.context.transaction).unwrap();
    assert_eq!(child.resolve(handle, HandleKind::File), Ok(remote));
    assert_eq!(parent.close(handle), Ok(None));
    assert_eq!(child.close(handle).unwrap().unwrap().remote(), remote);
}

#[test]
fn execution_weakening_transfer() {
    let parent = Arc::new(HandleNamespace::new(1).unwrap());
    let remote = RemoteId::new(72).unwrap();
    let handle = parent.open(remote, HandleKind::Transfer).unwrap();
    let participant = Arc::new(ProviderForkParticipant::new(parent.clone()));
    let ports: Vec<Arc<dyn ForkParticipant>> = vec![
        Arc::new(Passive(ForkParticipantRole::Task, None)),
        Arc::new(Passive(ForkParticipantRole::Descriptors, None)),
        Arc::new(Passive(ForkParticipantRole::Memory, None)),
        participant.clone(),
        Arc::new(Passive(ForkParticipantRole::Execution, Some(ForkPhase::CloneChild))),
        Arc::new(Passive(ForkParticipantRole::Network, None)),
        Arc::new(Passive(ForkParticipantRole::Event, None)),
        Arc::new(Passive(ForkParticipantRole::Ipc, None)),
    ];
    assert!(
        ForkCoordinator::new(ports)
            .unwrap()
            .fork(RuntimeFixture::request(ForkCloneFlags::default()), &NeverCancel)
            .is_err()
    );
    assert_eq!(participant.staged_count(), 0);
    let capability = parent
        .transfer(handle)
        .map_err(|error| {
            assert_ne!(error, NamespaceError::SharedTransfer);
            error
        })
        .unwrap();
    assert_eq!(capability.close().remote(), remote);
}

fn task_registry() -> (Arc<TaskRegistry>, hl_task::ThreadId) {
    let tasks = Arc::new(
        TaskRegistry::new(RegistryConfig {
            max_processes: 2,
            max_threads: 2,
            ..RegistryConfig::default()
        })
        .unwrap(),
    );
    let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
    let (_, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
    (tasks, thread)
}

#[test]
fn task_rolls_back() {
    let (tasks, source) = task_registry();
    let task = Arc::new(TaskForkParticipant::reserve(Arc::clone(&tasks), source).unwrap());
    let stale_child = task.request().child;
    let participants: Vec<Arc<dyn ForkParticipant>> = vec![
        task,
        Arc::new(Passive(ForkParticipantRole::Descriptors, Some(ForkPhase::Commit))),
        Arc::new(Passive(ForkParticipantRole::Memory, None)),
        Arc::new(Passive(ForkParticipantRole::Provider, None)),
        Arc::new(Passive(ForkParticipantRole::Execution, None)),
        Arc::new(Passive(ForkParticipantRole::Network, None)),
        Arc::new(Passive(ForkParticipantRole::Event, None)),
        Arc::new(Passive(ForkParticipantRole::Ipc, None)),
    ];
    assert!(
        ForkCoordinator::new(participants)
            .unwrap()
            .fork(
                ForkRequest {
                    parent: tasks.snapshot().processes[0].id.fork_identity(),
                    child: stale_child,
                    flags: ForkCloneFlags::default(),
                },
                &NeverCancel,
            )
            .is_err()
    );
    assert_eq!(tasks.snapshot().processes.len(), 1);
    let replacement = TaskForkParticipant::reserve(Arc::clone(&tasks), source).unwrap();
    assert_eq!(replacement.request().child.slot, stale_child.slot);
    assert_ne!(replacement.request().child.generation, stale_child.generation);
}

#[test]
fn successful_resource_commits() {
    let (tasks, source) = task_registry();
    let task = Arc::new(TaskForkParticipant::reserve(Arc::clone(&tasks), source).unwrap());
    let request = task.request();
    let participants: Vec<Arc<dyn ForkParticipant>> = vec![
        task.clone(),
        Arc::new(Passive(ForkParticipantRole::Descriptors, None)),
        Arc::new(Passive(ForkParticipantRole::Memory, None)),
        Arc::new(Passive(ForkParticipantRole::Provider, None)),
        Arc::new(Passive(ForkParticipantRole::Execution, None)),
        Arc::new(Passive(ForkParticipantRole::Network, None)),
        Arc::new(Passive(ForkParticipantRole::Event, None)),
        Arc::new(Passive(ForkParticipantRole::Ipc, None)),
    ];
    ForkCoordinator::new(participants)
        .unwrap()
        .fork(request, &NeverCancel)
        .unwrap();
    let child = task.child().unwrap();
    assert_eq!(child.0.fork_identity(), request.child);
    assert_eq!(tasks.snapshot().processes.len(), 2);
}
