use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;

use hl_task::{ForkCloneFlags, ForkEntityId, ForkRequest};

use crate::{
    ForkArtifactExchange, ForkCancellation, ForkContext, ForkCoordinator, ForkError, ForkParticipant,
    ForkParticipantRole, ForkPhase,
};

#[derive(Debug)]
struct TestCancellation(AtomicBool);

impl ForkCancellation for TestCancellation {
    fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug, Default)]
struct ParticipantState {
    live: BTreeSet<u64>,
    events: Vec<(ForkPhase, u64, ForkEntityId, ForkEntityId, u8)>,
    artifact: Option<Weak<PhaseArtifact>>,
}

#[derive(Debug)]
struct PhaseArtifact;

#[derive(Debug)]
struct TestParticipant {
    role: ForkParticipantRole,
    fail: Option<ForkPhase>,
    next: AtomicU64,
    state: Mutex<ParticipantState>,
}

impl TestParticipant {
    fn new(role: ForkParticipantRole, fail: Option<ForkPhase>) -> Self {
        Self {
            role,
            fail,
            next: AtomicU64::new(role as u64 + 1),
            state: Mutex::new(ParticipantState::default()),
        }
    }

    fn operation(&self, phase: ForkPhase, context: ForkContext, reservation: u64) -> Result<(), ()> {
        let mut state = self.state.lock().unwrap();
        state.events.push((
            phase,
            reservation,
            context.request.parent,
            context.request.child,
            context.request.flags.bits(),
        ));
        if self.fail == Some(phase) {
            return Err(());
        }
        if phase == ForkPhase::Commit {
            state.live.remove(&reservation);
        }
        Ok(())
    }
}

impl ForkParticipant for TestParticipant {
    fn role(&self) -> ForkParticipantRole {
        self.role
    }

    fn prepare(&self, context: ForkContext) -> Result<u64, ()> {
        let reservation = self.next.fetch_add(5, Ordering::Relaxed);
        self.operation(ForkPhase::Prepare, context, reservation)?;
        self.state.lock().unwrap().live.insert(reservation);
        Ok(reservation)
    }

    fn freeze(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        self.operation(ForkPhase::Freeze, context, reservation)
    }
    fn clone_parent(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        self.operation(ForkPhase::CloneParent, context, reservation)
    }
    fn clone_child(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        self.operation(ForkPhase::CloneChild, context, reservation)
    }
    fn clone_with_artifacts(
        &self,
        context: ForkContext,
        reservation: u64,
        artifacts: &ForkArtifactExchange,
    ) -> Result<(), ()> {
        self.clone_child(context, reservation)?;
        if self.role == ForkParticipantRole::Memory {
            let artifact = Arc::new(PhaseArtifact);
            self.state.lock().unwrap().artifact = Some(Arc::downgrade(&artifact));
            artifacts.publish(context, self.role, reservation, artifact)?;
        }
        if self.role == ForkParticipantRole::Provider
            && artifacts
                .get::<PhaseArtifact>(context, ForkParticipantRole::Memory)
                .is_none()
        {
            return Err(());
        }
        Ok(())
    }
    fn repair_parent(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        self.operation(ForkPhase::RepairParent, context, reservation)
    }
    fn repair_child(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        self.operation(ForkPhase::RepairChild, context, reservation)
    }
    fn commit(&self, context: ForkContext, reservation: u64) -> Result<(), ()> {
        self.operation(ForkPhase::Commit, context, reservation)
    }
    fn rollback(&self, context: ForkContext, reservation: u64) {
        assert!(self.operation(ForkPhase::Rollback, context, reservation).is_ok());
        self.state.lock().unwrap().live.remove(&reservation);
    }
}

struct Fixture {
    coordinator: Arc<ForkCoordinator>,
    participants: Vec<Arc<TestParticipant>>,
}

impl Fixture {
    fn new(failure: Option<(ForkParticipantRole, ForkPhase)>) -> Self {
        let roles = [
            ForkParticipantRole::Task,
            ForkParticipantRole::Descriptors,
            ForkParticipantRole::Memory,
            ForkParticipantRole::Provider,
            ForkParticipantRole::Execution,
            ForkParticipantRole::Network,
            ForkParticipantRole::Event,
            ForkParticipantRole::Ipc,
        ];
        let participants: Vec<_> = roles
            .into_iter()
            .map(|role| {
                Arc::new(TestParticipant::new(
                    role,
                    failure.filter(|(failed, _)| *failed == role).map(|(_, phase)| phase),
                ))
            })
            .collect();
        let ports = participants
            .iter()
            .cloned()
            .map(|participant| participant as Arc<dyn ForkParticipant>)
            .collect();
        Self {
            coordinator: Arc::new(ForkCoordinator::new(ports).unwrap()),
            participants,
        }
    }

    fn request(flags: ForkCloneFlags) -> ForkRequest {
        ForkRequest {
            parent: ForkEntityId { slot: 1, generation: 2 },
            child: ForkEntityId { slot: 3, generation: 4 },
            flags,
        }
    }

    fn assert_no_live(&self) {
        assert!(self.participants.iter().all(|participant| {
            let state = participant.state.lock().unwrap();
            state.live.is_empty()
                && state
                    .artifact
                    .as_ref()
                    .map_or(true, |artifact| artifact.upgrade().is_none())
        }));
    }
}

#[test]
fn participant_reservations_reverse() {
    let phases = [
        ForkPhase::Prepare,
        ForkPhase::Freeze,
        ForkPhase::CloneParent,
        ForkPhase::CloneChild,
        ForkPhase::RepairParent,
        ForkPhase::RepairChild,
        ForkPhase::Commit,
    ];
    for role in [
        ForkParticipantRole::Task,
        ForkParticipantRole::Descriptors,
        ForkParticipantRole::Memory,
        ForkParticipantRole::Provider,
        ForkParticipantRole::Execution,
        ForkParticipantRole::Network,
        ForkParticipantRole::Event,
        ForkParticipantRole::Ipc,
    ] {
        for phase in phases {
            let fixture = Fixture::new(Some((role, phase)));
            assert_eq!(
                fixture.coordinator.fork(
                    Fixture::request(ForkCloneFlags::default()),
                    &TestCancellation(AtomicBool::new(false)),
                ),
                Err(ForkError::Participant { role, phase })
            );
            fixture.assert_no_live();
        }
    }
}

#[test]
fn successful_pointer_free() {
    let fixture = Fixture::new(None);
    let flags = ForkCloneFlags::FILES
        .union(ForkCloneFlags::VM)
        .union(ForkCloneFlags::SIGHAND);
    let outcome = fixture
        .coordinator
        .fork(Fixture::request(flags), &TestCancellation(AtomicBool::new(false)))
        .unwrap();
    assert_eq!(
        outcome.wire.participant_reservations.len(),
        hl_task::MAX_FORK_PARTICIPANTS,
    );
    outcome.wire.validate().unwrap();
    assert!(
        outcome
            .transcript
            .windows(2)
            .all(|events| { events[0].phase as u8 <= events[1].phase as u8 })
    );
    for participant in &fixture.participants {
        let state = participant.state.lock().unwrap();
        assert!(state.events.iter().any(|event| event.0 == ForkPhase::CloneParent));
        assert!(state.events.iter().any(|event| event.0 == ForkPhase::CloneChild));
        assert!(
            state
                .events
                .iter()
                .all(|event| { event.2 != event.3 && event.4 == flags.bits() })
        );
    }
    fixture.assert_no_live();
}

#[test]
fn descriptor_inheritance_decision() {
    let fixture = Fixture::new(None);
    let flags = ForkCloneFlags::FILES;
    fixture
        .coordinator
        .fork(Fixture::request(flags), &TestCancellation(AtomicBool::new(false)))
        .unwrap();
    let descriptors = &fixture.participants[ForkParticipantRole::Descriptors as usize];
    assert!(
        descriptors
            .state
            .lock()
            .unwrap()
            .events
            .iter()
            .all(|event| event.4 & ForkCloneFlags::FILES.bits() != 0)
    );
}

#[test]
fn invalid_publish_nothing() {
    let fixture = Fixture::new(None);
    assert_eq!(
        fixture.coordinator.fork(
            Fixture::request(ForkCloneFlags::SIGHAND),
            &TestCancellation(AtomicBool::new(false)),
        ),
        Err(ForkError::InvalidRequest)
    );
    assert_eq!(
        fixture.coordinator.fork(
            Fixture::request(ForkCloneFlags::default()),
            &TestCancellation(AtomicBool::new(true)),
        ),
        Err(ForkError::Cancelled)
    );
    fixture.assert_no_live();
}

#[test]
fn artifact_single_consumer() {
    let exchange = ForkArtifactExchange::default();
    let context = ForkContext {
        transaction: 41,
        request: Fixture::request(ForkCloneFlags::default()),
    };
    assert!(exchange.get::<u64>(context, ForkParticipantRole::Memory).is_none());
    exchange
        .publish(context, ForkParticipantRole::Memory, 7, Arc::new(99_u64))
        .unwrap();
    assert_eq!(
        exchange.get::<u64>(context, ForkParticipantRole::Memory).as_deref(),
        Some(&99),
    );
    assert!(exchange.get::<u32>(context, ForkParticipantRole::Memory).is_none());
    assert!(
        exchange
            .get::<u64>(
                ForkContext {
                    transaction: 42,
                    ..context
                },
                ForkParticipantRole::Memory,
            )
            .is_none()
    );
    assert!(
        exchange
            .publish(context, ForkParticipantRole::Memory, 8, Arc::new(100_u64),)
            .is_err()
    );
    assert_eq!(
        exchange.take::<u64>(context, ForkParticipantRole::Memory).as_deref(),
        Some(&99),
    );
    assert!(exchange.get::<u64>(context, ForkParticipantRole::Memory).is_none());
}

#[test]
fn concurrent_lock_damage() {
    let fixture = Arc::new(Fixture::new(None));
    let workers: Vec<_> = (0..64)
        .map(|_| {
            let fixture = fixture.clone();
            thread::spawn(move || {
                fixture
                    .coordinator
                    .fork(
                        Fixture::request(ForkCloneFlags::default()),
                        &TestCancellation(AtomicBool::new(false)),
                    )
                    .unwrap();
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    fixture.assert_no_live();
}
