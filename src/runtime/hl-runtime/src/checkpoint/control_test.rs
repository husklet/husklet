use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use hl_checkpoint::{
    CheckpointImage, CheckpointWriter, Fault, ImageLimits, MemorySink, MemorySource, PortError, Section, SectionKind,
};

use crate::{CheckpointError, CheckpointParticipant, CheckpointPhase, CheckpointRole, RuntimeCheckpointCoordinator};

const ROLES: [CheckpointRole; 8] = [
    CheckpointRole::Task,
    CheckpointRole::Descriptors,
    CheckpointRole::Memory,
    CheckpointRole::Provider,
    CheckpointRole::Event,
    CheckpointRole::Network,
    CheckpointRole::Ipc,
    CheckpointRole::Execution,
];

#[derive(Default)]
struct State {
    frozen: bool,
    live: BTreeSet<u64>,
    committed: BTreeSet<u64>,
    resumed: BTreeSet<u64>,
    rollbacks: Vec<u64>,
    admitted: usize,
}

struct TestParticipant {
    role: CheckpointRole,
    dependencies: Vec<CheckpointRole>,
    fail: Option<CheckpointPhase>,
    state: Mutex<State>,
    idle: Condvar,
}

impl TestParticipant {
    fn new(role: CheckpointRole, fail: Option<CheckpointPhase>) -> Self {
        Self {
            role,
            dependencies: ROLES[..role as usize].to_vec(),
            fail,
            state: Mutex::new(State::default()),
            idle: Condvar::new(),
        }
    }

    fn fails(&self, phase: CheckpointPhase) -> Result<(), ()> {
        if self.fail == Some(phase) { Err(()) } else { Ok(()) }
    }

    fn no_partial_state(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.live.is_empty() && state.committed.is_empty() && state.resumed.is_empty()
    }

    fn admit_waiter(&self) {
        self.state.lock().unwrap().admitted += 1;
    }

    fn release_waiter(&self) {
        let mut state = self.state.lock().unwrap();
        state.admitted -= 1;
        self.idle.notify_all();
    }
}

impl CheckpointParticipant for TestParticipant {
    fn role(&self) -> CheckpointRole {
        self.role
    }

    fn version(&self) -> u32 {
        1
    }

    fn dependencies(&self) -> &[CheckpointRole] {
        &self.dependencies
    }

    fn freeze(&self) -> Result<(), ()> {
        self.fails(CheckpointPhase::Freeze)?;
        let mut state = self.state.lock().unwrap();
        while state.admitted != 0 {
            state = self.idle.wait(state).unwrap();
        }
        state.frozen = true;
        Ok(())
    }

    fn snapshot(&self) -> Result<Vec<u8>, ()> {
        self.fails(CheckpointPhase::Snapshot)?;
        let state = self.state.lock().unwrap();
        if !state.frozen {
            return Err(());
        }
        Ok(vec![self.role as u8])
    }

    fn thaw(&self) -> Result<(), ()> {
        self.fails(CheckpointPhase::Thaw)?;
        self.state.lock().unwrap().frozen = false;
        Ok(())
    }

    fn validate(&self, image: &CheckpointImage, section: &Section) -> Result<(), ()> {
        self.fails(CheckpointPhase::Validate)?;
        if section.bytes() != [self.role as u8] {
            return Err(());
        }
        for dependency in &self.dependencies {
            let kind = hl_checkpoint::SectionKind::new(*dependency as u32 + 1).map_err(|_| ())?;
            if image.section(kind).is_none() {
                return Err(());
            }
        }
        Ok(())
    }

    fn stage(&self, _: &Section) -> Result<u64, ()> {
        self.fails(CheckpointPhase::Stage)?;
        let reservation = self.role as u64 + 1;
        self.state.lock().unwrap().live.insert(reservation);
        Ok(reservation)
    }

    fn commit(&self, reservation: u64) -> Result<(), ()> {
        self.fails(CheckpointPhase::Commit)?;
        self.state.lock().unwrap().committed.insert(reservation);
        Ok(())
    }

    fn rollback(&self, reservation: u64) {
        let mut state = self.state.lock().unwrap();
        state.live.remove(&reservation);
        state.committed.remove(&reservation);
        state.resumed.remove(&reservation);
        state.rollbacks.push(reservation);
    }

    fn resume(&self, reservation: u64) -> Result<(), ()> {
        self.fails(CheckpointPhase::Resume)?;
        let mut state = self.state.lock().unwrap();
        state.live.remove(&reservation);
        state.resumed.insert(reservation);
        Ok(())
    }
}

struct Fixture {
    coordinator: Arc<RuntimeCheckpointCoordinator>,
    participants: Vec<Arc<TestParticipant>>,
}

impl Fixture {
    fn new(failure: Option<(CheckpointRole, CheckpointPhase)>) -> Self {
        let participants: Vec<_> = ROLES
            .into_iter()
            .map(|role| {
                Arc::new(TestParticipant::new(
                    role,
                    failure.filter(|(target, _)| *target == role).map(|(_, phase)| phase),
                ))
            })
            .collect();
        let ports = participants
            .iter()
            .cloned()
            .map(|value| value as Arc<dyn CheckpointParticipant>)
            .collect();
        Self {
            coordinator: Arc::new(RuntimeCheckpointCoordinator::new(ports, ImageLimits::default()).unwrap()),
            participants,
        }
    }

    fn image() -> Vec<u8> {
        let fixture = Self::new(None);
        let mut sink = MemorySink::new();
        fixture.coordinator.checkpoint(&mut sink).unwrap();
        sink.committed().unwrap().to_vec()
    }

    fn sections(roles: impl IntoIterator<Item = (CheckpointRole, u32)>) -> Vec<u8> {
        let mut writer = CheckpointWriter::new(ImageLimits::default());
        for (role, version) in roles {
            writer
                .push(Section::new(
                    SectionKind::new(role as u32 + 1).unwrap(),
                    version,
                    vec![role as u8],
                ))
                .unwrap();
        }
        let mut sink = MemorySink::new();
        writer.publish(&mut sink).unwrap();
        sink.committed().unwrap().to_vec()
    }
}

#[test]
fn capture_frozen_domains() {
    for role in ROLES {
        for phase in [CheckpointPhase::Freeze, CheckpointPhase::Snapshot] {
            let fixture = Fixture::new(Some((role, phase)));
            let mut sink = MemorySink::new();
            assert!(matches!(
                fixture.coordinator.checkpoint(&mut sink),
                Err(CheckpointError::Participant { role: failed, phase: failed_phase })
                    if failed == role && failed_phase == phase
            ));
            assert!(sink.committed().is_none());
            assert!(
                fixture
                    .participants
                    .iter()
                    .all(|participant| !participant.state.lock().unwrap().frozen)
            );
        }
    }
}

#[test]
fn thaw_domain_visited() {
    let fixture = Fixture::new(Some((CheckpointRole::Memory, CheckpointPhase::Thaw)));
    let mut sink = MemorySink::new();
    assert!(matches!(
        fixture.coordinator.checkpoint(&mut sink),
        Err(CheckpointError::Participant {
            role: CheckpointRole::Memory,
            phase: CheckpointPhase::Thaw,
        })
    ));
    for participant in &fixture.participants {
        if participant.role != CheckpointRole::Memory {
            assert!(!participant.state.lock().unwrap().frozen);
        }
    }
}

#[test]
fn restore_staged_state() {
    let image = Fixture::image();
    for role in ROLES {
        for phase in [
            CheckpointPhase::Validate,
            CheckpointPhase::Stage,
            CheckpointPhase::Commit,
            CheckpointPhase::Resume,
        ] {
            let fixture = Fixture::new(Some((role, phase)));
            let mut source = MemorySource::new(image.clone());
            assert!(matches!(
                fixture.coordinator.restore(&mut source),
                Err(CheckpointError::Participant { role: failed, phase: failed_phase })
                    if failed == role && failed_phase == phase
            ));
            assert!(
                fixture
                    .participants
                    .iter()
                    .all(|participant| participant.no_partial_state())
            );
        }
    }
}

#[test]
fn complete_resumes_domain() {
    let fixture = Fixture::new(None);
    let mut source = MemorySource::new(Fixture::image());
    fixture.coordinator.restore(&mut source).unwrap();
    for participant in &fixture.participants {
        let state = participant.state.lock().unwrap();
        assert!(state.live.is_empty());
        assert_eq!(state.committed, state.resumed);
        assert_eq!(state.resumed, BTreeSet::from([participant.role as u64 + 1]));
        assert!(state.rollbacks.is_empty());
    }
}

#[test]
fn stream_stages_nothing() {
    let fixture = Fixture::new(None);
    for operation in 1..=3 {
        let mut sink = MemorySink::with_fault(Fault {
            operation,
            error: PortError::Failed,
        });
        assert!(matches!(
            fixture.coordinator.checkpoint(&mut sink),
            Err(CheckpointError::Image(_))
        ));
        assert!(sink.committed().is_none());
    }
    let image = Fixture::image();
    for operation in 1..=3 {
        let fixture = Fixture::new(None);
        let mut source = MemorySource::with_fault(
            image.clone(),
            Fault {
                operation,
                error: PortError::Failed,
            },
        );
        assert!(matches!(
            fixture.coordinator.restore(&mut source),
            Err(CheckpointError::Image(_))
        ));
        assert!(
            fixture
                .participants
                .iter()
                .all(|participant| participant.no_partial_state())
        );
    }
}

#[test]
fn blocked_after_publish() {
    let fixture = Fixture::new(None);
    let task = fixture.participants[0].clone();
    task.admit_waiter();
    let coordinator = fixture.coordinator.clone();
    let worker = thread::spawn(move || {
        let mut sink = MemorySink::new();
        coordinator.checkpoint(&mut sink).unwrap();
        sink.committed().unwrap().to_vec()
    });
    thread::yield_now();
    assert!(!worker.is_finished());
    task.release_waiter();
    assert!(!worker.join().unwrap().is_empty());
    assert!(
        fixture
            .participants
            .iter()
            .all(|participant| !participant.state.lock().unwrap().frozen)
    );
}

#[test]
fn invalid_before_staging() {
    let mut participants: Vec<Arc<dyn CheckpointParticipant>> = ROLES
        .into_iter()
        .map(|role| Arc::new(TestParticipant::new(role, None)) as Arc<dyn CheckpointParticipant>)
        .collect();
    participants.swap(0, 1);
    assert!(matches!(
        RuntimeCheckpointCoordinator::new(participants, ImageLimits::default()),
        Err(CheckpointError::InvalidParticipants)
    ));

    let fixture = Fixture::new(None);
    let image = Fixture::sections(
        ROLES
            .into_iter()
            .map(|role| (role, if role == CheckpointRole::Memory { 2 } else { 1 })),
    );
    let mut source = MemorySource::new(image);
    assert!(matches!(
        fixture.coordinator.restore(&mut source),
        Err(CheckpointError::Version {
            role: CheckpointRole::Memory,
            expected: 1,
            actual: 2,
        })
    ));
    assert!(
        fixture
            .participants
            .iter()
            .all(|participant| participant.no_partial_state())
    );

    let missing = Fixture::sections(
        ROLES
            .into_iter()
            .filter(|role| *role != CheckpointRole::Provider)
            .map(|role| (role, 1)),
    );
    let mut source = MemorySource::new(missing);
    assert!(matches!(
        fixture.coordinator.restore(&mut source),
        Err(CheckpointError::MissingSection(CheckpointRole::Provider))
    ));
    assert!(
        fixture
            .participants
            .iter()
            .all(|participant| participant.no_partial_state())
    );
}

#[test]
fn coherent_prefix_composes() {
    let task = Arc::new(TestParticipant::new(CheckpointRole::Task, None));
    let descriptors = Arc::new(TestParticipant::new(CheckpointRole::Descriptors, None));
    let coordinator = RuntimeCheckpointCoordinator::new(
        vec![
            task as Arc<dyn CheckpointParticipant>,
            descriptors as Arc<dyn CheckpointParticipant>,
        ],
        ImageLimits::default(),
    )
    .unwrap();
    assert_eq!(coordinator.roles(), [CheckpointRole::Task, CheckpointRole::Descriptors],);
    let mut sink = MemorySink::new();
    coordinator.checkpoint(&mut sink).unwrap();
    let mut source = MemorySource::new(sink.committed().unwrap().to_vec());
    coordinator.restore(&mut source).unwrap();

    let execution = Arc::new(TestParticipant::new(CheckpointRole::Execution, None));
    assert!(matches!(
        RuntimeCheckpointCoordinator::new(
            vec![execution as Arc<dyn CheckpointParticipant>],
            ImageLimits::default(),
        ),
        Err(CheckpointError::InvalidParticipants),
    ));
}
