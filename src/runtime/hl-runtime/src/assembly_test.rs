use super::*;
use crate::PreparedExecParticipant;
use hl_linux::ClonePlan;
use hl_task::{ProcessId, ThreadId};

struct Fork;

impl crate::RuntimeForkPort for Fork {
    fn fork(
        &self,
        _: ProcessId,
        _: ThreadId,
        _: ClonePlan,
    ) -> Result<crate::RuntimeForkResult, crate::RuntimeForkError> {
        Err(crate::RuntimeForkError::Unsupported)
    }
}
use hl_checkpoint::{CheckpointImage, ImageLimits, MemorySink, MemorySource, Section};
use hl_memory::{
    MappingCoordinator, MemoryCheckpointHost, MemoryCheckpointImage, MemoryError, MemoryHostRestore, MemoryHostStage,
    SharedLimits, SharedObjectStore, TestMappingHost,
};
use hl_task::{
    ProcessCheckpointReference, TaskError, TaskExternalCheckpoint, TaskExternalRestore, TaskRegistryImage,
    TaskResourceKey, ThreadCheckpointReference,
};
use hl_task::{ProcessCredentials, ProcessLimits};
use std::sync::atomic::{AtomicUsize, Ordering};

struct CheckpointDomain {
    role: crate::CheckpointRole,
    freezes: Arc<AtomicUsize>,
}

struct TaskBindings;
struct TaskRestore;

struct MemoryHost;
struct MemoryTransaction;

#[test]
fn default_descriptor_capacity_covers_linux_high_descriptors() {
    assert_eq!(RuntimeAssemblyConfig::default().descriptor_limit, 65_536);
}

impl MemoryHostRestore<TestMappingHost> for MemoryTransaction {
    fn commit(&mut self) -> Result<(), MemoryError> {
        Ok(())
    }
    fn rollback(&mut self) {}
    fn resume(&mut self) -> Result<(), MemoryError> {
        Ok(())
    }
}

impl MemoryCheckpointHost<TestMappingHost> for MemoryHost {
    fn address_limit(&self) -> u64 {
        65_536
    }

    fn snapshot_mapping(
        &self,
        _: &hl_memory::FrozenSnapshotAuthority,
        region: hl_memory::Region,
    ) -> Result<Vec<u8>, MemoryError> {
        Ok(vec![0; region.range().length() as usize])
    }

    fn stage(&self, image: &MemoryCheckpointImage) -> Result<MemoryHostStage<TestMappingHost>, MemoryError> {
        let shared = Arc::new(
            SharedObjectStore::restore(image.shared_limits, image.shared.clone()).map_err(MemoryError::Shared)?,
        );
        Ok(MemoryHostStage {
            mapping: TestMappingHost,
            shared,
            restore: Box::new(MemoryTransaction),
        })
    }
}

impl TaskExternalRestore for TaskRestore {
    fn commit(&mut self) -> Result<(), TaskError> {
        Ok(())
    }
    fn rollback(&mut self) {}
    fn resume(&mut self) -> Result<(), TaskError> {
        Ok(())
    }
}

impl TaskExternalCheckpoint for TaskBindings {
    fn snapshot_process(&self, process: ProcessId) -> Result<ProcessCheckpointReference, TaskError> {
        Ok(ProcessCheckpointReference {
            process,
            descriptor_table: Some(TaskResourceKey(u64::from(process.number()))),
            shared_resources: Vec::new(),
        })
    }

    fn snapshot_thread(&self, thread: ThreadId) -> Result<ThreadCheckpointReference, TaskError> {
        Ok(ThreadCheckpointReference {
            thread,
            execution: TaskResourceKey(u64::from(thread.number())),
            tls: TaskResourceKey(10_000 + u64::from(thread.number())),
            host: TaskResourceKey(20_000 + u64::from(thread.number())),
            seccomp: TaskResourceKey(30_000 + u64::from(thread.number())),
        })
    }

    fn stage(&self, _: &TaskRegistryImage) -> Result<Box<dyn TaskExternalRestore>, TaskError> {
        Ok(Box::new(TaskRestore))
    }
}

impl crate::CheckpointParticipant for CheckpointDomain {
    fn role(&self) -> crate::CheckpointRole {
        self.role
    }
    fn version(&self) -> u32 {
        1
    }
    fn dependencies(&self) -> &[crate::CheckpointRole] {
        &[]
    }
    fn freeze(&self) -> Result<(), ()> {
        self.freezes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        Ok(vec![self.role as u8])
    }
    fn thaw(&self) -> Result<(), ()> {
        Ok(())
    }
    fn validate(&self, _: &CheckpointImage, section: &Section) -> Result<(), ()> {
        (section.bytes() == [self.role as u8]).then_some(()).ok_or(())
    }
    fn stage(&self, _: &Section) -> Result<u64, ()> {
        Ok(self.role as u64 + 1)
    }
    fn commit(&self, _: u64) -> Result<(), ()> {
        Ok(())
    }
    fn rollback(&self, _: u64) {}
    fn resume(&self, _: u64) -> Result<(), ()> {
        Ok(())
    }
}

fn checkpoint_coordinator(
    roles: &[crate::CheckpointRole],
    freezes: &Arc<AtomicUsize>,
) -> Arc<crate::RuntimeCheckpointCoordinator> {
    let participants = roles
        .iter()
        .map(|role| {
            Arc::new(CheckpointDomain {
                role: *role,
                freezes: Arc::clone(freezes),
            }) as Arc<dyn crate::CheckpointParticipant>
        })
        .collect();
    Arc::new(crate::RuntimeCheckpointCoordinator::new(participants, ImageLimits::default()).unwrap())
}

#[test]
fn fallible_reports_domain() {
    let cases = [
        (
            RuntimeAssemblyConfig {
                maximum_processes: 0,
                ..RuntimeAssemblyConfig::default()
            },
            RuntimeDomain::Task,
        ),
        (
            RuntimeAssemblyConfig {
                descriptor_limit: -1,
                ..RuntimeAssemblyConfig::default()
            },
            RuntimeDomain::DescriptorEvent,
        ),
        (
            RuntimeAssemblyConfig {
                event_capacity: 0,
                ..RuntimeAssemblyConfig::default()
            },
            RuntimeDomain::EventCatalog,
        ),
        (
            RuntimeAssemblyConfig {
                provider_capacity: 0,
                ..RuntimeAssemblyConfig::default()
            },
            RuntimeDomain::Provider,
        ),
        (
            RuntimeAssemblyConfig {
                maximum_seccomp_threads: 0,
                ..RuntimeAssemblyConfig::default()
            },
            RuntimeDomain::Seccomp,
        ),
    ];
    for (config, domain) in cases {
        assert_eq!(
            RuntimeAssembly::new(config).err(),
            Some(RuntimeAssemblyError::Construction(domain))
        );
    }
}

#[test]
fn two_identity_spaces() {
    let first = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let second = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(0, 0, &[], 32).unwrap();
    let first_init = first
        .tasks()
        .create_init(credentials.clone(), ProcessLimits::default())
        .unwrap();
    let second_init = second
        .tasks()
        .create_init(credentials, ProcessLimits::default())
        .unwrap();
    assert_eq!(first_init, second_init);
    assert!(
        first
            .tasks()
            .create_init(
                ProcessCredentials::new(0, 0, &[], 32).unwrap(),
                ProcessLimits::default()
            )
            .is_err()
    );
}

#[test]
fn descriptor_assembly_identity() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    assert!(Arc::ptr_eq(&assembly.epoll(), &assembly.epoll()));
    assert!(Arc::ptr_eq(&assembly.descriptors(), &assembly.descriptors(),));
}

#[test]
fn descriptor_generation_swap() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let table = assembly.descriptors();
    let old = table.descriptor_table();
    let participant = crate::DescriptorExec::new(table.image_slot(), assembly.epoll());
    let mut prepared = participant.prepare_current().unwrap();
    let candidate = prepared.candidate().unwrap();
    prepared.publish().unwrap();
    assert!(Arc::ptr_eq(&table.descriptor_table(), &candidate));
    prepared.rollback();
    assert!(Arc::ptr_eq(&table.descriptor_table(), &old));
}

#[test]
fn unavailable_coordinators_honest() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    for domain in [
        RuntimeDomain::Memory,
        RuntimeDomain::Ipc,
        RuntimeDomain::Linux,
        RuntimeDomain::Loader,
        RuntimeDomain::Execution,
        RuntimeDomain::Checkpoint,
        RuntimeDomain::Fork,
    ] {
        assert_eq!(assembly.require(domain), Err(RuntimeAssemblyError::Unsupported(domain)));
    }
}

#[test]
fn fork_installed_once() {
    let mut assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    assert_eq!(
        assembly.require(RuntimeDomain::Fork),
        Err(RuntimeAssemblyError::Unsupported(RuntimeDomain::Fork)),
    );
    assert!(assembly.fork().is_none());
    assembly.install_fork(Arc::new(Fork)).unwrap();
    assert_eq!(assembly.require(RuntimeDomain::Fork), Ok(()));
    assert!(assembly.fork().is_some());
    assert_eq!(
        assembly.install_fork(Arc::new(Fork)),
        Err(RuntimeAssemblyError::Construction(RuntimeDomain::Fork)),
    );
}

#[test]
fn exec_installed_once() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let slot = assembly.exec_slot();
    assert!(slot.get().is_none());
    assert_eq!(
        assembly.require(RuntimeDomain::Execution),
        Err(RuntimeAssemblyError::Unsupported(RuntimeDomain::Execution)),
    );
    assembly.install_exec(Arc::new(crate::RejectingExecPort)).unwrap();
    assert!(slot.get().is_some());
    assert_eq!(assembly.require(RuntimeDomain::Execution), Ok(()));
    assert!(assembly.exec().is_some());
    assert_eq!(
        assembly.install_exec(Arc::new(crate::RejectingExecPort)),
        Err(RuntimeAssemblyError::Construction(RuntimeDomain::Execution)),
    );
}

#[test]
fn ipc_installed_once() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let objects = Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
    assembly.install_ipc(Arc::clone(&objects)).unwrap();
    let installed = assembly.ipc().unwrap();
    assert_eq!(assembly.require(RuntimeDomain::Ipc), Ok(()));
    assert_eq!(
        assembly.install_ipc(objects),
        Err(RuntimeAssemblyError::Construction(RuntimeDomain::Ipc)),
    );
    assert!(Arc::ptr_eq(&installed, &assembly.ipc().unwrap()));
}

#[test]
fn exec_teardown_clears() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let slot = assembly.exec_slot();
    assembly.install_exec(Arc::new(crate::RejectingExecPort)).unwrap();
    assert!(assembly.teardown().contains(&RuntimeDomain::Execution));
    assert!(slot.get().is_none());
}

#[test]
fn exec_process_registry() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let slot = assembly.exec_slot();
    let first = hl_task::ProcessId::from_fork_identity(hl_task::ForkEntityId { slot: 7, generation: 1 }).unwrap();
    let reused = hl_task::ProcessId::from_fork_identity(hl_task::ForkEntityId { slot: 7, generation: 2 }).unwrap();
    slot.register(first, Arc::new(crate::RejectingExecPort)).unwrap();
    slot.register(reused, Arc::new(crate::RejectingExecPort)).unwrap();
    assert!(slot.for_process(first).is_some());
    assert!(slot.for_process(reused).is_some());
    assert!(slot.register(first, Arc::new(crate::RejectingExecPort)).is_err());
    slot.unregister(first);
    assert!(slot.for_process(first).is_none());
    assert!(slot.for_process(reused).is_some());
}

#[test]
fn explicit_reverse_exact() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    assert_eq!(
        assembly.teardown(),
        [
            RuntimeDomain::Seccomp,
            RuntimeDomain::Network,
            RuntimeDomain::Provider,
            RuntimeDomain::EventCatalog,
            RuntimeDomain::DescriptorEvent,
            RuntimeDomain::Task,
        ]
    );
}

#[test]
fn checkpoint_admission_rejects() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let freezes = Arc::new(AtomicUsize::new(0));
    let coordinator = checkpoint_coordinator(
        &[
            crate::CheckpointRole::Task,
            crate::CheckpointRole::Descriptors,
            crate::CheckpointRole::Event,
        ],
        &freezes,
    );
    assembly.install_checkpoint(coordinator).unwrap();
    let mut sink = MemorySink::new();
    assert!(matches!(
        assembly.capture_checkpoint(&mut sink),
        Err(AssemblyCheckpointError::Unsupported(RuntimeDomain::Provider)),
    ));
    assert_eq!(freezes.load(Ordering::Relaxed), 0);
    assert!(sink.committed().is_none());
}

#[test]
fn checkpoint_roundtrip_repeats() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let freezes = Arc::new(AtomicUsize::new(0));
    let coordinator = checkpoint_coordinator(
        &[
            crate::CheckpointRole::Task,
            crate::CheckpointRole::Descriptors,
            crate::CheckpointRole::Provider,
            crate::CheckpointRole::Event,
            crate::CheckpointRole::Network,
        ],
        &freezes,
    );
    assembly.install_checkpoint(coordinator).unwrap();
    for generation in 1..=2 {
        let mut sink = MemorySink::new();
        assembly.capture_checkpoint(&mut sink).unwrap();
        let mut source = MemorySource::new(sink.committed().unwrap().to_vec());
        assembly.restore_checkpoint(&mut source).unwrap();
        assert_eq!(freezes.load(Ordering::Relaxed), generation * 5);
    }
}

#[test]
fn task_owner_restores() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let task = Arc::new(crate::TaskCheckpointParticipant::new(
        assembly.checkpoint_tasks(),
        Arc::new(TaskBindings),
        Arc::new(crate::PortableTaskCodec),
    ));
    let freezes = Arc::new(AtomicUsize::new(0));
    let mut participants = vec![task as Arc<dyn crate::CheckpointParticipant>];
    for role in [
        crate::CheckpointRole::Descriptors,
        crate::CheckpointRole::Provider,
        crate::CheckpointRole::Event,
        crate::CheckpointRole::Network,
    ] {
        participants.push(Arc::new(CheckpointDomain {
            role,
            freezes: Arc::clone(&freezes),
        }));
    }
    let coordinator = Arc::new(crate::RuntimeCheckpointCoordinator::new(participants, ImageLimits::default()).unwrap());
    assembly.install_checkpoint(coordinator).unwrap();

    let (process, _) = assembly
        .tasks()
        .create_init(ProcessCredentials::new(1, 1, &[], 4).unwrap(), ProcessLimits::default())
        .unwrap();
    for mutation in [2, 3] {
        let mut sink = MemorySink::new();
        assembly.capture_checkpoint(&mut sink).unwrap();
        assembly
            .tasks()
            .replace_credentials(process, ProcessCredentials::new(mutation, mutation, &[], 4).unwrap())
            .unwrap();
        assert_eq!(assembly.tasks().snapshot().processes[0].credentials.real_user, mutation,);
        let mut source = MemorySource::new(sink.committed().unwrap().to_vec());
        assembly.restore_checkpoint(&mut source).unwrap();
        assert_eq!(assembly.tasks().snapshot().processes[0].credentials.real_user, 1);
    }
}

#[test]
fn memory_graph_finalizes() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let task = Arc::new(crate::TaskCheckpointParticipant::new(
        assembly.checkpoint_tasks(),
        Arc::new(TaskBindings),
        Arc::new(crate::PortableTaskCodec),
    ));
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let mappings = Arc::new(MappingCoordinator::with_shared(TestMappingHost, Arc::clone(&shared)));
    let memory = Arc::new(crate::CheckpointMemoryState::new(Arc::new(
        crate::CheckpointMemory::new(mappings, shared),
    )));
    let memory = Arc::new(crate::MemoryCheckpointParticipant::new(
        memory,
        Arc::new(MemoryHost),
        Arc::new(crate::PortableMemoryCodec),
    ));

    assembly.prepare_checkpoint(task).unwrap();
    assembly.prepare_checkpoint(memory).unwrap();
    assembly.finalize_checkpoint(ImageLimits::default()).unwrap();

    assert_eq!(
        assembly.checkpoint().unwrap().roles(),
        vec![crate::CheckpointRole::Task, crate::CheckpointRole::Memory],
    );
}

#[test]
fn provider_graph_roundtrip() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let freezes = Arc::new(AtomicUsize::new(0));
    for role in [
        crate::CheckpointRole::Task,
        crate::CheckpointRole::Descriptors,
        crate::CheckpointRole::Memory,
    ] {
        assembly
            .prepare_checkpoint(Arc::new(CheckpointDomain {
                role,
                freezes: freezes.clone(),
            }))
            .unwrap();
    }
    assembly.prepare_provider_checkpoint().unwrap();
    assembly.finalize_checkpoint(ImageLimits::default()).unwrap();
    let mut sink = MemorySink::new();
    assembly.checkpoint().unwrap().checkpoint(&mut sink).unwrap();
    let mut source = MemorySource::new(sink.committed().unwrap().to_vec());
    assembly.checkpoint().unwrap().restore(&mut source).unwrap();
    assert_eq!(
        assembly.checkpoint().unwrap().roles().last(),
        Some(&crate::CheckpointRole::Provider)
    );
}

#[test]
fn event_graph_roundtrip() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let freezes = Arc::new(AtomicUsize::new(0));
    assembly
        .prepare_checkpoint(Arc::new(CheckpointDomain {
            role: crate::CheckpointRole::Task,
            freezes: freezes.clone(),
        }))
        .unwrap();
    assembly
        .prepare_checkpoint(Arc::new(crate::DescriptorCheckpointParticipant::new(
            assembly.checkpoint_descriptors(),
            Arc::new(crate::DescriptorObjectCatalog::rejecting()),
        )))
        .unwrap();
    assembly
        .prepare_checkpoint(Arc::new(CheckpointDomain {
            role: crate::CheckpointRole::Memory,
            freezes: freezes.clone(),
        }))
        .unwrap();
    assembly.prepare_provider_checkpoint().unwrap();
    assembly.prepare_event_checkpoint().unwrap();
    assembly.finalize_checkpoint(ImageLimits::default()).unwrap();
    let mut sink = MemorySink::new();
    assembly.checkpoint().unwrap().checkpoint(&mut sink).unwrap();
    let mut source = MemorySource::new(sink.committed().unwrap().to_vec());
    assembly.checkpoint().unwrap().restore(&mut source).unwrap();
    assert_eq!(
        assembly.checkpoint().unwrap().roles().last(),
        Some(&crate::CheckpointRole::Event)
    );
}

#[test]
fn ipc_graph_roundtrip() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    assembly
        .tasks()
        .create_init(ProcessCredentials::new(1, 1, &[], 4).unwrap(), ProcessLimits::default())
        .unwrap();
    let task = Arc::new(crate::TaskCheckpointParticipant::new(
        assembly.checkpoint_tasks(),
        Arc::new(TaskBindings),
        Arc::new(crate::PortableTaskCodec),
    ));
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    assembly.install_ipc(shared.clone()).unwrap();
    let pipe_registry = assembly.ipc_pipes().unwrap();
    let pipes = pipe_registry.bindings();
    let descriptor_table = assembly.descriptors().descriptor_table();
    let opened = pipe_registry.open(Arc::new(hl_ipc::Pipe::new(true)));
    let descriptions = opened.descriptions();
    let prepared = descriptor_table
        .prepare_open_batch(
            0,
            vec![
                (
                    descriptions[0].clone(),
                    hl_descriptor::StatusFlags::default(),
                    hl_descriptor::DescriptorFlags::default(),
                ),
                (
                    descriptions[1].clone(),
                    hl_descriptor::StatusFlags::from_bits(1),
                    hl_descriptor::DescriptorFlags::default(),
                ),
            ],
        )
        .unwrap();
    let identities = prepared.description_identities();
    let publication = opened
        .prepare([identities[0].identity, identities[1].identity])
        .unwrap();
    publication.publish();
    let pipe_numbers = prepared.publish_all();
    let reader_alias = descriptor_table
        .duplicate(pipe_numbers[0], 0, hl_descriptor::DescriptorFlags::default())
        .unwrap();
    descriptor_table.pin(pipe_numbers[1]).unwrap().write(b"before").unwrap();
    let objects =
        Arc::new(crate::DescriptorObjectCatalog::rejecting().bind(hl_descriptor::ObjectKind::Pipe, pipes.clone()));
    let descriptors = Arc::new(crate::DescriptorCheckpointParticipant::new(
        assembly.checkpoint_descriptors(),
        objects,
    ));
    let mappings = Arc::new(MappingCoordinator::with_shared(TestMappingHost, shared.clone()));
    let memory_state = Arc::new(crate::CheckpointMemoryState::new(Arc::new(
        crate::CheckpointMemory::new(mappings, shared.clone()),
    )));
    let memory = Arc::new(crate::MemoryCheckpointParticipant::new(
        memory_state.clone(),
        Arc::new(MemoryHost),
        Arc::new(crate::PortableMemoryCodec),
    ));
    let freezes = Arc::new(AtomicUsize::new(0));

    assembly.prepare_checkpoint(task).unwrap();
    assembly.prepare_checkpoint(descriptors).unwrap();
    assembly.prepare_checkpoint(memory).unwrap();
    for role in [
        crate::CheckpointRole::Provider,
        crate::CheckpointRole::Event,
        crate::CheckpointRole::Network,
    ] {
        assembly
            .prepare_checkpoint(Arc::new(CheckpointDomain {
                role,
                freezes: freezes.clone(),
            }))
            .unwrap();
    }
    assembly.prepare_ipc_checkpoint(memory_state).unwrap();
    assembly.finalize_checkpoint(ImageLimits::default()).unwrap();

    let mut sink = MemorySink::new();
    assembly.capture_checkpoint(&mut sink).unwrap();
    let mut source = MemorySource::new(sink.committed().unwrap().to_vec());
    assembly.restore_checkpoint(&mut source).unwrap();
    assert_eq!(
        assembly.checkpoint().unwrap().roles().last(),
        Some(&crate::CheckpointRole::Ipc)
    );
    let restored = assembly.descriptors().descriptor_table();
    assert_eq!(
        restored.snapshot(pipe_numbers[0]).unwrap().description_identity,
        restored.snapshot(reader_alias).unwrap().description_identity,
    );
    restored.pin(pipe_numbers[1]).unwrap().write(b"-after").unwrap();
    let mut bytes = [0; 12];
    assert_eq!(restored.pin(reader_alias).unwrap().read(&mut bytes).unwrap(), 12);
    assert_eq!(&bytes, b"before-after");
    restored.close(pipe_numbers[0]).unwrap();
    restored.close(reader_alias).unwrap();
    restored.close(pipe_numbers[1]).unwrap();
    let ipc = assembly.ipc().unwrap();
    ipc.freeze_checkpoint();
    assert!(ipc.checkpoint_image().unwrap().pipes.is_empty());
    ipc.thaw_checkpoint();
}

#[test]
fn memory_requires_task() {
    let assembly = RuntimeAssembly::new(RuntimeAssemblyConfig::default()).unwrap();
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let mappings = Arc::new(MappingCoordinator::with_shared(TestMappingHost, Arc::clone(&shared)));
    let memory = Arc::new(crate::CheckpointMemoryState::new(Arc::new(
        crate::CheckpointMemory::new(mappings, shared),
    )));
    let memory = Arc::new(crate::MemoryCheckpointParticipant::new(
        memory,
        Arc::new(MemoryHost),
        Arc::new(crate::PortableMemoryCodec),
    ));

    assert_eq!(
        assembly.prepare_checkpoint(memory).unwrap_err(),
        RuntimeAssemblyError::Construction(RuntimeDomain::Checkpoint),
    );
    assert!(assembly.checkpoint().is_none());
}
