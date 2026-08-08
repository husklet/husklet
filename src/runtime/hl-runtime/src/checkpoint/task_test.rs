use std::sync::{Arc, Mutex};

use hl_checkpoint::{Section, SectionKind};
use hl_task::{
    ProcessCheckpointReference, ProcessCredentials, ProcessLimits, RegistryConfig, SignalFrameScope, SignalMask,
    TaskError, TaskExternalCheckpoint, TaskExternalRestore, TaskRegistry, TaskRegistryImage, TaskResourceKey,
    ThreadCheckpointReference,
};

use crate::{
    CheckpointParticipant, CheckpointTaskRegistry, PortableTaskCodec, TaskCheckpointCodec, TaskCheckpointParticipant,
};
use hl_linux::{BpfInstruction, BpfProgram, SeccompData, SeccompDecision, SeccompPolicy};

struct Codec(Mutex<Option<TaskRegistryImage>>);

impl Codec {
    fn new() -> Self {
        Self(Mutex::new(None))
    }
}

impl TaskCheckpointCodec for Codec {
    fn encode(&self, image: &TaskRegistryImage) -> Result<Vec<u8>, ()> {
        *self.0.lock().map_err(|_| ())? = Some(image.clone());
        Ok(b"task-v1".to_vec())
    }

    fn decode(&self, bytes: &[u8]) -> Result<TaskRegistryImage, ()> {
        if bytes != b"task-v1" {
            return Err(());
        }
        self.0.lock().map_err(|_| ())?.clone().ok_or(())
    }
}

#[derive(Default)]
struct ExternalState {
    commits: usize,
    rollbacks: usize,
    resumes: usize,
}

struct External {
    state: Arc<Mutex<ExternalState>>,
}

struct BindingTransaction {
    state: Arc<Mutex<ExternalState>>,
}

impl TaskExternalRestore for BindingTransaction {
    fn commit(&mut self) -> Result<(), TaskError> {
        self.state.lock().unwrap().commits += 1;
        Ok(())
    }

    fn rollback(&mut self) {
        self.state.lock().unwrap().rollbacks += 1;
    }

    fn resume(&mut self) -> Result<(), TaskError> {
        self.state.lock().unwrap().resumes += 1;
        Ok(())
    }
}

impl TaskExternalCheckpoint for External {
    fn snapshot_process(&self, process: hl_task::ProcessId) -> Result<ProcessCheckpointReference, TaskError> {
        Ok(ProcessCheckpointReference {
            process,
            descriptor_table: Some(TaskResourceKey(u64::from(process.number()))),
            shared_resources: Vec::new(),
        })
    }

    fn snapshot_thread(&self, thread: hl_task::ThreadId) -> Result<ThreadCheckpointReference, TaskError> {
        Ok(ThreadCheckpointReference {
            thread,
            execution: TaskResourceKey(u64::from(thread.number())),
            tls: TaskResourceKey(100 + u64::from(thread.number())),
            host: TaskResourceKey(200 + u64::from(thread.number())),
            seccomp: TaskResourceKey(300 + u64::from(thread.number())),
        })
    }

    fn stage(&self, _: &TaskRegistryImage) -> Result<Box<dyn TaskExternalRestore>, TaskError> {
        Ok(Box::new(BindingTransaction {
            state: self.state.clone(),
        }))
    }
}

fn fixture() -> (
    Arc<CheckpointTaskRegistry>,
    TaskCheckpointParticipant,
    Arc<Mutex<ExternalState>>,
) {
    let registry = Arc::new(
        TaskRegistry::new(RegistryConfig {
            max_processes: 4,
            max_threads: 8,
            max_groups: 8,
            max_pending_signals: 8,
            online_cpus: 1,
        })
        .unwrap(),
    );
    registry
        .create_init(ProcessCredentials::new(1, 1, &[], 8).unwrap(), ProcessLimits::empty())
        .unwrap();
    let handle = Arc::new(CheckpointTaskRegistry::new(registry));
    let state = Arc::new(Mutex::new(ExternalState::default()));
    let participant = TaskCheckpointParticipant::new(
        handle.clone(),
        Arc::new(External { state: state.clone() }),
        Arc::new(Codec::new()),
    );
    (handle, participant, state)
}

#[test]
fn task_external_bindings() {
    let (handle, participant, state) = fixture();
    let original = handle.current();
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(1).unwrap(),
        participant.version(),
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    assert!(!Arc::ptr_eq(&handle.current(), &original));
    participant.resume(reservation).unwrap();
    let state = state.lock().unwrap();
    assert_eq!((state.commits, state.rollbacks, state.resumes), (1, 0, 1));
}

#[test]
fn post_external_transaction() {
    let (handle, participant, state) = fixture();
    let original = handle.current();
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(1).unwrap(),
        participant.version(),
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&handle.current(), &original));
    assert_eq!(state.lock().unwrap().rollbacks, 1);
}

#[test]
fn malformed_live_registry() {
    let (handle, participant, state) = fixture();
    let section = Section::new(SectionKind::new(1).unwrap(), 1, vec![0]);
    assert!(participant.stage(&section).is_err());
    assert!(handle.current().snapshot().init.is_some());
    assert_eq!(state.lock().unwrap().commits, 0);
}

#[test]
fn portable_corruption_rejected() {
    let (handle, _, _) = fixture();
    let external = External {
        state: Arc::new(Mutex::new(ExternalState::default())),
    };
    let registry = handle.current();
    registry.freeze_checkpoint();
    let image = registry.image(&external).unwrap();
    registry.thaw_checkpoint();
    let codec = PortableTaskCodec;
    let bytes = codec.encode(&image).unwrap();
    let digest = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    });
    // Re-baselined when init moved off slot zero: the snapshot carries init's
    // identity, so its encoded bytes changed while the wire format did not.
    assert_eq!(digest, 0xddcc_f8b5_93d8_9e7d);
    assert_eq!(codec.encode(&image).unwrap(), bytes);
    assert_eq!(codec.decode(&bytes).unwrap(), image);
    assert_eq!(bytes.first(), Some(&b'{'));

    for length in [0, 1, bytes.len() / 2, bytes.len() - 1] {
        assert!(codec.decode(&bytes[..length]).is_err());
    }
    let unknown = br#"{"wire":1,"unknown":true}"#;
    assert!(codec.decode(unknown).is_err());
    let oversized = vec![b' '; super::task::TASK_BYTES_MAXIMUM + 1];
    assert!(codec.decode(&oversized).is_err());
    let mut stale = image.clone();
    stale.version -= 1;
    assert!(codec.encode(&stale).is_err());

    let mut corrupt = bytes;
    let marker = br#""wire":8"#;
    let offset = corrupt
        .windows(marker.len())
        .position(|window| window == marker)
        .unwrap();
    corrupt[offset + marker.len() - 1] = b'5';
    assert!(codec.decode(&corrupt).is_err());
}

#[test]
fn portable_signal_frames_round_trip() {
    let (handle, _, _) = fixture();
    let external = External {
        state: Arc::new(Mutex::new(ExternalState::default())),
    };
    let registry = handle.current();
    registry.freeze_checkpoint();
    let mut image = registry.image(&external).unwrap();
    registry.thaw_checkpoint();
    image.registry.threads[0].signals.frames = vec![SignalFrameScope {
        deferred: SignalMask::from_bits(0),
        stack_pointer: 0x20_000,
    }];
    image.registry.threads[0].signals.deferred = SignalMask::from_bits(1 << 9);

    let codec = PortableTaskCodec;
    let bytes = codec.encode(&image).unwrap();
    assert_eq!(codec.decode(&bytes).unwrap(), image);

    let marker = br#""frames":[[0,131072]]"#;
    let offset = bytes.windows(marker.len()).position(|window| window == marker).unwrap();
    let frames = (0..=hl_task::SIGNAL_FRAME_MAXIMUM)
        .map(|_| "[0,131072]")
        .collect::<Vec<_>>()
        .join(",");
    let replacement = format!("\"frames\":[{frames}]");
    let mut oversized = bytes.clone();
    oversized.splice(offset..offset + marker.len(), replacement.bytes());
    assert!(codec.decode(&oversized).is_err());
}

#[test]
fn stale_generation_rollback() {
    let (handle, participant, state) = fixture();
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(1).unwrap(),
        participant.version(),
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    let reservation = participant.stage(&section).unwrap();
    let newer = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    handle.test_publish(Arc::clone(&newer)).unwrap();
    assert!(participant.commit(reservation).is_err());
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&handle.current(), &newer));
    let state = state.lock().unwrap();
    assert_eq!((state.commits, state.rollbacks, state.resumes), (1, 1, 0));
}

#[test]
fn seccomp_chain_rollback() {
    let (handle, _, state) = fixture();
    let thread = handle.current().snapshot().threads[0].id;
    let control = Arc::new(crate::SeccompControl::new(8).unwrap());
    control.register(thread).unwrap();
    control.lock_privileges(thread).unwrap();
    let program = |errno: u32| {
        BpfProgram::new(vec![BpfInstruction {
            code: 0x06,
            jump_true: 0,
            jump_false: 0,
            value: 0x0005_0000 | errno,
        }])
        .unwrap()
    };
    let install = SeccompPolicy::install_plan(program(7), 0).unwrap();
    let transaction = control.begin_install(thread, &[], install, false).unwrap();
    control.commit_install(transaction).unwrap();
    let participant = TaskCheckpointParticipant::new(handle, Arc::new(External { state }), Arc::new(Codec::new()))
        .with_seccomp(Arc::clone(&control));
    participant.freeze().unwrap();
    let bytes = participant.snapshot().unwrap();
    participant.thaw().unwrap();
    let install = SeccompPolicy::install_plan(program(9), 0).unwrap();
    let transaction = control.begin_install(thread, &[], install, false).unwrap();
    control.commit_install(transaction).unwrap();
    let section = Section::new(SectionKind::new(1).unwrap(), participant.version(), bytes);
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    let data = SeccompData {
        number: 1,
        architecture: 0xc000_003e,
        instruction_pointer: 0,
        arguments: [0; 6],
    };
    assert_eq!(control.evaluate(thread, data).unwrap(), SeccompDecision::ReturnErrno(7));
    participant.rollback(reservation);
    assert_eq!(control.evaluate(thread, data).unwrap(), SeccompDecision::ReturnErrno(9));
}

#[test]
fn seccomp_corruption_rejected() {
    let (handle, _, state) = fixture();
    let thread = handle.current().snapshot().threads[0].id;
    let control = Arc::new(crate::SeccompControl::new(8).unwrap());
    control.register(thread).unwrap();
    let participant = TaskCheckpointParticipant::new(handle, Arc::new(External { state }), Arc::new(Codec::new()))
        .with_seccomp(control);
    participant.freeze().unwrap();
    let mut bytes = participant.snapshot().unwrap();
    participant.thaw().unwrap();
    *bytes.last_mut().unwrap() = b'!';
    let section = Section::new(SectionKind::new(1).unwrap(), participant.version(), bytes);
    assert!(participant.stage(&section).is_err());
}
