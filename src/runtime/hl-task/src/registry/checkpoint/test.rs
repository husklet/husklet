use std::sync::Arc;
use std::thread;

use crate::{
    AlternateStack, PendingTarget, ProcessCheckpointReference, ProcessCredentials, ProcessLimits, RegistryConfig,
    SignalAction, SignalDisposition, SignalFrameScope, SignalInfo, SignalMask, SignalNumber, TaskError,
    TaskExternalCheckpoint, TaskExternalRestore, TaskRegistry, TaskRegistryImage, TaskResourceKey,
    ThreadCheckpointReference,
};

struct BindingTransaction;

impl TaskExternalRestore for BindingTransaction {
    fn commit(&mut self) -> Result<(), TaskError> {
        Ok(())
    }
    fn rollback(&mut self) {}
    fn resume(&mut self) -> Result<(), TaskError> {
        Ok(())
    }
}

struct External;

impl TaskExternalCheckpoint for External {
    fn snapshot_process(&self, process: crate::ProcessId) -> Result<ProcessCheckpointReference, TaskError> {
        Ok(ProcessCheckpointReference {
            process,
            descriptor_table: Some(TaskResourceKey(u64::from(process.number()))),
            shared_resources: vec![TaskResourceKey(100 + u64::from(process.number()))],
        })
    }

    fn snapshot_thread(&self, thread: crate::ThreadId) -> Result<ThreadCheckpointReference, TaskError> {
        Ok(ThreadCheckpointReference {
            thread,
            execution: TaskResourceKey(u64::from(thread.number())),
            tls: TaskResourceKey(100 + u64::from(thread.number())),
            host: TaskResourceKey(200 + u64::from(thread.number())),
            seccomp: TaskResourceKey(300 + u64::from(thread.number())),
        })
    }

    fn stage(&self, _: &TaskRegistryImage) -> Result<Box<dyn TaskExternalRestore>, TaskError> {
        Ok(Box::new(BindingTransaction))
    }
}

fn registry() -> (TaskRegistry, crate::ProcessId, crate::ThreadId) {
    let registry = TaskRegistry::new(RegistryConfig {
        max_processes: 8,
        max_threads: 16,
        max_groups: 8,
        max_pending_signals: 8,
        online_cpus: 1,
    })
    .unwrap();
    let (process, thread) = registry
        .create_init(
            ProcessCredentials::new(1000, 1000, &[7, 9], 8).unwrap(),
            ProcessLimits::empty(),
        )
        .unwrap();
    (registry, process, thread)
}

#[test]
fn aggregate_round_trip() {
    let (registry, parent, source) = registry();
    registry.set_dumpable(parent, false).unwrap();
    registry.set_timer_slack(parent, 77_777).unwrap();
    registry.set_thp(parent, true).unwrap();
    registry.set_mce_policy(parent, 1).unwrap();
    registry.set_subreaper(parent, true).unwrap();
    registry.set_pdeath(parent, 12).unwrap();
    registry.charge_cpu(parent, 19).unwrap();
    registry.set_name(source, *b"checkpoint\0\0\0\0\0\0").unwrap();
    let plan = registry.begin_fork_process(source).unwrap();
    let child = plan.process();
    registry.commit_fork_process(plan).unwrap();
    registry.exit_process(child, crate::ExitStatus::Code(17)).unwrap();
    let snapshot = registry.snapshot();
    let restored = TaskRegistry::restore(&snapshot).unwrap();
    assert_eq!(restored.snapshot(), snapshot);
    let parent = restored
        .snapshot()
        .processes
        .into_iter()
        .find(|process| process.id == parent)
        .unwrap();
    assert_eq!(parent.children, vec![child]);
    assert_eq!(parent.cpu_usage.self_nanoseconds, 19);
}

#[test]
fn malformed_parent_thread() {
    let (registry, _, _) = registry();
    let snapshot = registry.snapshot();
    for corruption in 0..4 {
        let mut invalid = snapshot.clone();
        match corruption {
            0 => {
                let process = invalid.processes[0].id;
                invalid.processes[0].children.push(process);
            }
            1 => invalid.processes[0].threads.clear(),
            2 => invalid.process_groups[0].members.clear(),
            _ => invalid.process_generations[0] = invalid.process_generations[0].saturating_add(1),
        }
        assert_eq!(TaskRegistry::restore(&invalid).err(), Some(TaskError::InvalidSnapshot));
    }
}

#[test]
fn realtime_signal_priority() {
    let (registry, process, thread) = registry();
    for (number, value) in [(33, 1), (33, 2), (32, 3)] {
        let mut info = SignalInfo::bare(SignalNumber::new(number).unwrap());
        info.value = value;
        registry.enqueue_signal(PendingTarget::Process(process), info).unwrap();
    }
    let restored = TaskRegistry::restore(&registry.snapshot()).unwrap();
    let mut delivered = Vec::new();
    while let Some((info, _)) = restored.dequeue_signal(thread).unwrap() {
        delivered.push((info.signal.get(), info.value));
    }
    assert_eq!(delivered, [(33, 1), (33, 2), (32, 3)]);
}

#[test]
fn active_signal_frames_round_trip() {
    let (registry, process, thread) = registry();
    for number in [10, 12] {
        let signal = SignalNumber::new(number).unwrap();
        registry
            .set_action(
                process,
                signal,
                SignalAction {
                    disposition: SignalDisposition::Handler(0x4000),
                    ..SignalAction::DEFAULT
                },
            )
            .unwrap();
        registry
            .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(signal))
            .unwrap();
    }
    let prepared = registry.prepare_deliverable_signal(thread).unwrap().unwrap();
    registry.force_signal_delivery(prepared).unwrap();
    let forced = registry.prepare_forced_delivery(thread).unwrap();
    registry
        .commit_frame_delivery(
            forced,
            SignalMask::from_bits(0).with(SignalNumber::new(12).unwrap()),
            AlternateStack::Disabled,
            0x20_000,
            false,
        )
        .unwrap();

    let snapshot = registry.snapshot();
    assert_eq!(
        snapshot.threads[0].signals.frames,
        [SignalFrameScope {
            deferred: SignalMask::from_bits(0),
            stack_pointer: 0x20_000,
        }]
    );
    assert!(snapshot.threads[0]
        .signals
        .deferred
        .contains(SignalNumber::new(10).unwrap()));
    assert_eq!(TaskRegistry::restore(&snapshot).unwrap().snapshot(), snapshot);
}

#[test]
fn malformed_signal_frames_rejected() {
    let (registry, _, _) = registry();
    let snapshot = registry.snapshot();
    for corruption in 0..4 {
        let mut invalid = snapshot.clone();
        let signals = &mut invalid.threads[0].signals;
        match corruption {
            0 => signals.deferred = SignalMask::from_bits(1),
            1 => {
                signals.frames = vec![
                    SignalFrameScope {
                        deferred: SignalMask::from_bits(0),
                        stack_pointer: 0x20_000,
                    };
                    33
                ]
            }
            2 => {
                signals.frames = vec![SignalFrameScope {
                    deferred: SignalMask::from_bits(1),
                    stack_pointer: 0x20_000,
                }];
                signals.deferred = SignalMask::from_bits(1);
            }
            _ => {
                signals.frames = vec![
                    SignalFrameScope {
                        deferred: SignalMask::from_bits(0),
                        stack_pointer: 0x20_000,
                    },
                    SignalFrameScope {
                        deferred: SignalMask::from_bits(4),
                        stack_pointer: 0x1f_000,
                    },
                    SignalFrameScope {
                        deferred: SignalMask::from_bits(2),
                        stack_pointer: 0x1e_000,
                    },
                ];
                signals.deferred = SignalMask::from_bits(6);
            }
        }
        assert_eq!(TaskRegistry::restore(&invalid).err(), Some(TaskError::InvalidSnapshot));
    }
}

#[test]
fn freeze_serializes() {
    let (registry, process, thread_id) = registry();
    let registry = Arc::new(registry);
    let mut workers = Vec::new();
    for _ in 0..8 {
        let registry = registry.clone();
        workers.push(thread::spawn(move || {
            registry
                .set_signal_mask(thread_id, crate::SignalMask::from_bits(7))
                .unwrap();
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    registry.freeze_checkpoint();
    let image = registry.image(&External).unwrap();
    assert_eq!(image.processes[0].process, process);
    assert_eq!(image.threads[0].thread, thread_id);
    let blocked = {
        let registry = registry.clone();
        thread::spawn(move || registry.set_signal_mask(thread_id, crate::SignalMask::from_bits(0)))
    };
    thread::yield_now();
    assert!(!blocked.is_finished());
    registry.thaw_checkpoint();
    assert_eq!(blocked.join().unwrap(), Ok(()));
}

#[test]
fn starting_transaction_is() {
    let (registry, _, source) = registry();
    let plan = registry.begin_clone_thread(source).unwrap();
    let snapshot = registry.snapshot();
    assert_eq!(TaskRegistry::restore(&snapshot).err(), Some(TaskError::InvalidSnapshot));
    registry.rollback_clone_thread(plan).unwrap();
}

#[test]
fn duplicate_resource_rejected() {
    let (registry, _, _) = registry();
    registry.freeze_checkpoint();
    let mut image = registry.image(&External).unwrap();
    registry.thaw_checkpoint();
    image.processes[0].shared_resources = [TaskResourceKey(7), TaskResourceKey(7)].to_vec();
    assert_eq!(image.validate(), Err(TaskError::InvalidSnapshot));
    image.processes[0].shared_resources.clear();
    image.threads[0].host = image.threads[0].execution;
    assert_eq!(image.validate(), Err(TaskError::InvalidSnapshot));
}
