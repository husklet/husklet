use super::*;

use crate::test_support::ProcessFixture;
use crate::{MemoryPort, RuntimeExecPort, SafeRuntimeExec};
use hl_ipc::{Credentials, SEM_UNDO, SemaphoreOperation};
use hl_memory::TestMappingHost;

const OWNER: Credentials = Credentials { uid: 7, gid: 8 };

#[test]
fn empty_domain_transaction() {
    let fixture = ProcessFixture::new();
    let mut prepared = EmptyIpcExec
        .prepare(fixture.process, fixture.thread, &fixture.plan())
        .unwrap();
    prepared.publish().unwrap();
    prepared.rollback();
    prepared.finish();
}

impl ProcessFixture {
    fn participant(&self) -> ExecParticipant<TestMappingHost> {
        ExecParticipant::new(self.ipc.catalog.clone(), self.mappings.clone(), Arc::new(|| 9))
    }

    fn plan(&self) -> ExecPlan {
        ExecPlan {
            directory: None,
            path: b"/bin/program".to_vec(),
            arguments: vec![b"program".to_vec()],
            environment: Vec::new(),
            flags: 0,
        }
    }

    fn prepared(&self) -> Box<dyn PreparedExecParticipant> {
        self.participant()
            .prepare(self.process, self.thread, &self.plan())
            .unwrap()
    }
}

#[test]
fn sem_undo() {
    let fixture = ProcessFixture::new();
    let undo = fixture.ipc.semaphores.snapshot().undo;
    let mut prepared = fixture.prepared();
    prepared.publish().unwrap();
    assert!(fixture.mappings.bindings().unwrap().is_empty());
    assert!(fixture.ipc.shared.snapshot().attachments.is_empty());
    assert_eq!(fixture.ipc.semaphores.snapshot().undo, undo);
    prepared.finish();
    assert_eq!(fixture.ipc.semaphores.snapshot().undo, undo);
}

#[test]
fn undo_exactly() {
    let fixture = ProcessFixture::new();
    let shared = fixture.ipc.shared.snapshot();
    let undo = fixture.ipc.semaphores.snapshot();
    let bindings = fixture.mappings.bindings().unwrap();
    let mut prepared = fixture.prepared();
    prepared.publish().unwrap();
    prepared.rollback();
    assert_eq!(fixture.mappings.bindings().unwrap(), bindings);
    assert_eq!(fixture.ipc.shared.snapshot(), shared);
    assert_eq!(fixture.ipc.semaphores.snapshot(), undo);
}

#[test]
fn namespace_published_mappings() {
    let fixture = ProcessFixture::new();
    let mut namespace_stale = fixture.prepared();
    fixture
        .ipc
        .shared
        .shmdt(fixture.tokens[0], fixture.process.number(), 10)
        .unwrap();
    assert_eq!(namespace_stale.publish(), Err(RuntimeExecError::Failed),);
    assert_eq!(fixture.mappings.bindings().unwrap().len(), 2);

    let fixture = ProcessFixture::new();
    let bindings = fixture.mappings.bindings().unwrap();
    let regions = fixture.mappings.coordinator.ledger().regions();
    let shared = fixture.ipc.shared.snapshot();
    let mut semaphore_stale = fixture.prepared();
    fixture
        .ipc
        .semaphores
        .operate(
            fixture.semaphore,
            OWNER,
            fixture.process.number(),
            &[SemaphoreOperation {
                index: 0,
                delta: 1,
                flags: SEM_UNDO,
            }],
            11,
        )
        .unwrap();
    assert_eq!(semaphore_stale.publish(), Err(RuntimeExecError::Failed),);
    assert_eq!(fixture.mappings.bindings().unwrap(), bindings);
    assert_eq!(fixture.mappings.coordinator.ledger().regions(), regions);
    assert_eq!(fixture.ipc.shared.snapshot(), shared);
}

struct FailingParticipant;

struct FailingStage;

impl RuntimeExecParticipant for FailingParticipant {
    fn prepare(
        &self,
        _: ProcessId,
        _: ThreadId,
        _: &ExecPlan,
    ) -> Result<Box<dyn PreparedExecParticipant>, RuntimeExecError> {
        Ok(Box::new(FailingStage))
    }
}

impl PreparedExecParticipant for FailingStage {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        Err(RuntimeExecError::Failed)
    }

    fn rollback(&mut self) {}

    fn finish(&mut self) {}
}

#[test]
fn downstream_ipc_state() {
    let fixture = ProcessFixture::new();
    let bindings = fixture.mappings.bindings().unwrap();
    let regions = fixture.mappings.coordinator.ledger().regions();
    let shared = fixture.ipc.shared.snapshot();
    let semaphores = fixture.ipc.semaphores.snapshot();
    let exec = SafeRuntimeExec::new(vec![Arc::new(fixture.participant()), Arc::new(FailingParticipant)]).unwrap();
    assert_eq!(
        exec.exec(fixture.process, fixture.thread, fixture.plan()),
        Err(RuntimeExecError::Failed),
    );
    assert_eq!(fixture.mappings.bindings().unwrap(), bindings);
    assert_eq!(fixture.mappings.coordinator.ledger().regions(), regions);
    assert_eq!(fixture.ipc.shared.snapshot(), shared);
    assert_eq!(fixture.ipc.semaphores.snapshot(), semaphores);
}

#[test]
fn stale_new_state() {
    let fixture = ProcessFixture::new();
    let regions = fixture.mappings.coordinator.ledger().regions();
    let mut prepared = fixture.prepared();
    fixture
        .mappings
        .mappings
        .lock()
        .unwrap()
        .remove(&GuestAddress::new(0x4000));
    assert_eq!(prepared.publish(), Err(RuntimeExecError::Failed));
    assert_eq!(fixture.mappings.coordinator.ledger().regions(), regions);
    assert_eq!(fixture.mappings.bindings().unwrap().len(), 1);
}
