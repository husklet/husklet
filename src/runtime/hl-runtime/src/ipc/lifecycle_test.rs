use std::sync::{Arc, Mutex};

use hl_ipc::{
    AttachPlan, Credentials, IPC_PRIVATE, IpcCatalog, IpcKey, MessageLimits, MessageQueueNamespace, SEM_UNDO,
    SemGetRequest, SemaphoreLimits, SemaphoreNamespace, SemaphoreOperation, SharedMemoryError as CatalogError,
    SharedMemoryLimits, SharedMemoryNamespace, ShmGetRequest,
};
use hl_isa::GuestAddress;
use hl_memory::{SharedLimits, SharedObjectStore};

use crate::{
    CommittedBindingSet, ForkBinding, MappingError, MemoryBinding, MemoryLifecycle, MemoryPort, PreparedBindingSet,
    RuntimeIpcLifecycle,
};

const OWNER: Credentials = Credentials { uid: 10, gid: 20 };
const PARENT: u32 = 10;
const CHILD: u32 = 20;

#[derive(Debug)]
struct Port(Mutex<Vec<MemoryBinding>>);

impl Port {
    fn with_attachment(attachment: u64) -> Arc<Self> {
        Arc::new(Self(Mutex::new(vec![MemoryBinding {
            address: GuestAddress::new(0x4000),
            length: 4096,
            attachment,
        }])))
    }

    fn empty() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Vec::new())))
    }
}

impl MemoryPort for Port {
    fn map(&self, _: AttachPlan, _: GuestAddress) -> Result<GuestAddress, MappingError> {
        Err(MappingError::Invalid)
    }

    fn bind(&self, _: GuestAddress, _: u64) -> Result<(), MappingError> {
        Err(MappingError::Invalid)
    }

    fn rollback(&self, _: GuestAddress) -> Result<(), MappingError> {
        Err(MappingError::Invalid)
    }

    fn unmap(&self, _: GuestAddress) -> Result<u64, MappingError> {
        Err(MappingError::Invalid)
    }

    fn bindings(&self) -> Result<Vec<MemoryBinding>, MappingError> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn restore_bindings(&self, bindings: &[MemoryBinding]) -> Result<(), MappingError> {
        *self.0.lock().unwrap() = bindings.to_vec();
        Ok(())
    }

    fn prepare_fork_bindings(
        &self,
        bindings: &[ForkBinding],
    ) -> Result<Box<dyn PreparedBindingSet<'_> + '_>, MappingError> {
        let replacement = bindings.iter().map(|value| value.binding).collect();
        Ok(Box::new(PreparedPort {
            port: self,
            expected: self.0.lock().unwrap().clone(),
            replacement,
        }))
    }

    fn unmap_all(&self) -> Result<Vec<u64>, MappingError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .drain(..)
            .map(|binding| binding.attachment)
            .collect())
    }
}

struct PreparedPort<'a> {
    port: &'a Port,
    expected: Vec<MemoryBinding>,
    replacement: Vec<MemoryBinding>,
}

struct CommittedPort<'a> {
    port: &'a Port,
    previous: Vec<MemoryBinding>,
    published: Vec<MemoryBinding>,
}

impl<'a> PreparedBindingSet<'a> for PreparedPort<'a> {
    fn commit(self: Box<Self>) -> Result<Box<dyn CommittedBindingSet + 'a>, MappingError> {
        let mut bindings = self.port.0.lock().unwrap();
        if *bindings != self.expected {
            return Err(MappingError::Invariant);
        }
        let previous = std::mem::replace(&mut *bindings, self.replacement);
        let published = bindings.clone();
        Ok(Box::new(CommittedPort {
            port: self.port,
            previous,
            published,
        }))
    }
}

impl CommittedBindingSet for CommittedPort<'_> {
    fn rollback(self: Box<Self>) -> Result<(), MappingError> {
        let mut bindings = self.port.0.lock().unwrap();
        if *bindings != self.published {
            return Err(MappingError::Invariant);
        }
        *bindings = self.previous;
        Ok(())
    }

    fn finish(self: Box<Self>) {}
}

pub(crate) struct Fixture {
    pub(crate) catalog: Arc<IpcCatalog>,
    pub(crate) shared: Arc<SharedMemoryNamespace>,
    pub(crate) semaphores: Arc<SemaphoreNamespace>,
    pub(crate) memory: Arc<SharedObjectStore>,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        let shared_limits = SharedMemoryLimits::default();
        let memory = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let shared = Arc::new(SharedMemoryNamespace::new(memory.clone(), shared_limits).unwrap());
        let message_limits = MessageLimits::default();
        let semaphore_limits = SemaphoreLimits::default();
        let semaphores = Arc::new(SemaphoreNamespace::new(semaphore_limits).unwrap());
        let catalog = Arc::new(IpcCatalog::new(
            Arc::clone(&shared),
            shared_limits,
            Vec::new(),
            Arc::new(MessageQueueNamespace::new(message_limits).unwrap()),
            message_limits,
            Arc::clone(&semaphores),
            semaphore_limits,
            Vec::new(),
        ));
        Self {
            catalog,
            shared,
            semaphores,
            memory,
        }
    }

    fn attachment(&self) -> (hl_ipc::SharedMemoryId, u64) {
        let id = self
            .shared
            .shmget(ShmGetRequest {
                key: IPC_PRIVATE,
                size: 4096,
                create: true,
                exclusive: false,
                mode: 0o600,
                actor: OWNER,
                pid: PARENT,
                now: 1,
            })
            .unwrap();
        let plan = self.shared.shmat_plan(id, OWNER, 0).unwrap();
        let attachment = self.shared.commit_attach(plan, PARENT, 2).unwrap();
        (id, attachment)
    }

    fn semaphore(&self) -> hl_ipc::SemaphoreId {
        self.semaphores
            .semget(SemGetRequest {
                key: IpcKey(7),
                semaphores: 1,
                create: true,
                exclusive: false,
                mode: 0o600,
                actor: OWNER,
                pid: PARENT,
                now: 1,
            })
            .unwrap()
    }

    fn undo(&self, id: hl_ipc::SemaphoreId, process: u32, delta: i32) {
        self.semaphores
            .operate(
                id,
                OWNER,
                process,
                &[SemaphoreOperation {
                    index: 0,
                    delta,
                    flags: SEM_UNDO,
                }],
                3,
            )
            .unwrap();
    }
}

#[test]
fn fork_child_undo() {
    let fixture = Fixture::new();
    let (segment, attachment) = fixture.attachment();
    let semaphore = fixture.semaphore();
    fixture.semaphores.set_value(semaphore, 0, 4, OWNER, PARENT, 2).unwrap();
    fixture.undo(semaphore, PARENT, -2);
    fixture.undo(semaphore, CHILD, -1);
    let parent = Port::with_attachment(attachment);
    let child = Port::empty();
    RuntimeIpcLifecycle::new(Arc::clone(&fixture.catalog), parent)
        .fork(PARENT, CHILD, 4, child.as_ref())
        .unwrap();
    assert_eq!(fixture.shared.metadata(segment).unwrap().attaches, 2);
    assert_ne!(child.bindings().unwrap()[0].attachment, attachment);
    RuntimeIpcLifecycle::new(Arc::clone(&fixture.catalog), child)
        .exit(CHILD, 5)
        .unwrap();
    assert_eq!(fixture.semaphores.get_value(semaphore, 0, OWNER), Ok(1));
    assert_eq!(fixture.shared.metadata(segment).unwrap().attaches, 1);
}

#[test]
fn prepared_becomes_stale() {
    let fixture = Fixture::new();
    let (segment, attachment) = fixture.attachment();
    let parent = Port::with_attachment(attachment);
    let child = Port::empty();
    let lifecycle = MemoryLifecycle::new(Arc::clone(&fixture.catalog), parent);
    let prepared = lifecycle.prepare_fork(PARENT, CHILD, 4, child.as_ref()).unwrap();
    assert!(child.bindings().unwrap().is_empty());

    let concurrent = fixture.shared.shmat_plan(segment, OWNER, 0).unwrap();
    fixture.shared.commit_attach(concurrent, 30, 5).unwrap();
    let namespace = fixture.shared.snapshot();
    assert_eq!(prepared.commit(), Err(MappingError::Invalid),);
    assert!(child.bindings().unwrap().is_empty());
    assert_eq!(fixture.shared.snapshot(), namespace);
}

#[test]
fn prepared_namespace_finishes() {
    let fixture = Fixture::new();
    let (segment, attachment) = fixture.attachment();
    let parent = Port::with_attachment(attachment);
    let child = Port::empty();
    MemoryLifecycle::new(Arc::clone(&fixture.catalog), parent)
        .prepare_fork(PARENT, CHILD, 4, child.as_ref())
        .unwrap()
        .commit()
        .unwrap();
    let child_binding = child.bindings().unwrap()[0];
    assert_ne!(child_binding.attachment, attachment);
    assert_eq!(fixture.shared.metadata(segment).unwrap().attaches, 2);
    assert!(
        fixture
            .shared
            .snapshot()
            .attachments
            .iter()
            .any(|(token, id, process)| { *token == child_binding.attachment && *id == segment && *process == CHILD })
    );
}

#[test]
fn exec_reuse_alias() {
    let fixture = Fixture::new();
    let (segment, attachment) = fixture.attachment();
    let semaphore = fixture.semaphore();
    fixture.semaphores.set_value(semaphore, 0, 3, OWNER, PARENT, 2).unwrap();
    fixture.undo(semaphore, PARENT, -2);
    fixture.shared.remove(segment, OWNER, PARENT, 3).unwrap();
    RuntimeIpcLifecycle::new(Arc::clone(&fixture.catalog), Port::with_attachment(attachment))
        .exec(PARENT, 4)
        .unwrap();
    assert_eq!(fixture.shared.metadata(segment), Err(CatalogError::NotFound),);
    assert_eq!(fixture.semaphores.get_value(semaphore, 0, OWNER), Ok(1));
    fixture.semaphores.exit(PARENT, 5);
    assert_eq!(fixture.semaphores.get_value(semaphore, 0, OWNER), Ok(3));
    let (replacement, _) = fixture.attachment();
    assert_eq!(replacement.slot, segment.slot);
    assert_ne!(replacement.generation, segment.generation);
}

#[test]
fn exit_parent_undo() {
    let fixture = Fixture::new();
    let (segment, attachment) = fixture.attachment();
    let semaphore = fixture.semaphore();
    fixture.semaphores.set_value(semaphore, 0, 3, OWNER, PARENT, 2).unwrap();
    fixture.undo(semaphore, PARENT, -2);
    fixture.shared.remove(segment, OWNER, PARENT, 3).unwrap();
    RuntimeIpcLifecycle::new(Arc::clone(&fixture.catalog), Port::with_attachment(attachment))
        .exit(PARENT, 4)
        .unwrap();
    assert_eq!(fixture.shared.metadata(segment), Err(CatalogError::NotFound),);
    assert_eq!(fixture.semaphores.get_value(semaphore, 0, OWNER), Ok(3));
}
