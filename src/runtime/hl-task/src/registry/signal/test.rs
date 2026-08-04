use std::sync::Arc;
use std::thread;

use crate::*;

struct Fixture;

impl Fixture {
    fn registry(limit: usize) -> (TaskRegistry, ProcessId, ThreadId) {
        let registry = TaskRegistry::new(RegistryConfig {
            max_processes: 4,
            max_threads: 8,
            max_groups: 8,
            max_pending_signals: limit,
            online_cpus: 1,
        })
        .unwrap();
        let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let identity = registry.create_init(credentials, ProcessLimits::empty()).unwrap();
        (registry, identity.0, identity.1)
    }

    fn number(number: u8) -> SignalNumber {
        SignalNumber::new(number).unwrap()
    }

    fn info(number: u8, value: u64) -> SignalInfo {
        let mut info = SignalInfo::bare(Self::number(number));
        info.value = value;
        info
    }

    fn enter_handler(registry: &TaskRegistry, thread: ThreadId) -> SignalInfo {
        let prepared = registry.prepare_deliverable_signal(thread).unwrap().unwrap();
        let signal = prepared.info().signal;
        registry.force_signal_delivery(prepared).unwrap();
        let forced = registry.prepare_forced_delivery(thread).unwrap();
        let depth = registry
            .snapshot()
            .threads
            .into_iter()
            .find(|entry| entry.id == thread)
            .unwrap()
            .signals
            .frames
            .len() as u64;
        registry
            .commit_frame_delivery(
                forced,
                SignalMask::from_bits(0).with(signal),
                AlternateStack::Disabled,
                0x20_000 - depth * 0x1000,
                false,
            )
            .unwrap()
    }

    fn return_handler(registry: &TaskRegistry, thread: ThreadId) {
        registry
            .replace_signal_context(thread, SignalMask::from_bits(0), AlternateStack::Disabled)
            .unwrap();
    }
}

#[test]
fn interrupting_signal_respects_delivery_action_and_temporary_mask() {
    let (registry, process, thread) = Fixture::registry(8);
    let terminate = Fixture::number(10);
    assert!(
        registry
            .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(terminate))
            .unwrap()
    );
    assert!(registry.has_interrupting_signal(thread, None).unwrap());
    assert!(
        !registry
            .has_interrupting_signal(thread, Some(SignalMask::from_bits(0).with(terminate)))
            .unwrap()
    );

    let ignored = Fixture::number(17);
    assert!(
        !registry
            .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(ignored))
            .unwrap()
    );
}

#[test]
fn prepared_signal_commit() {
    let (registry, process, thread) = Fixture::registry(8);
    let signal = Fixture::number(35);
    let mask = SignalMask::from_bits(0).with(signal);
    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(35, 1))
        .unwrap();
    let prepared = registry.prepare_signal_wait(thread, mask).unwrap().unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(35, 2))
        .unwrap();
    assert!(registry.commit_signal_wait(prepared).unwrap());
    assert_eq!(registry.consume_signal_wait(thread, mask).unwrap().unwrap().value, 2);

    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(35, 3))
        .unwrap();
    let reserved = registry.prepare_signal_wait(thread, mask).unwrap().unwrap();
    assert_eq!(registry.consume_signal_wait(thread, mask).unwrap(), None);
    drop(reserved);
    assert_eq!(registry.consume_signal_wait(thread, mask).unwrap().unwrap().value, 3);
}

#[test]
fn source_tag_coalesces_and_removes_only_its_own_expiry() {
    let (registry, process, thread) = Fixture::registry(8);
    let signal = Fixture::number(35);
    let mut first = Fixture::info(35, 11);
    first.source_tag = 7;
    let mut repeat = Fixture::info(35, 22);
    repeat.source_tag = 7;
    let mut distinct = Fixture::info(35, 33);
    distinct.source_tag = 8;

    assert!(
        registry
            .enqueue_source_signal(PendingTarget::Process(process), first)
            .unwrap()
    );
    assert!(
        !registry
            .enqueue_source_signal(PendingTarget::Process(process), repeat)
            .unwrap()
    );
    assert!(
        registry
            .enqueue_source_signal(PendingTarget::Process(process), distinct)
            .unwrap()
    );
    assert!(
        registry
            .remove_source_signal(PendingTarget::Process(process), signal, 7)
            .unwrap()
    );
    assert!(
        !registry
            .remove_source_signal(PendingTarget::Process(process), signal, 7)
            .unwrap()
    );

    let delivered = registry.dequeue_signal(thread).unwrap().unwrap().0;
    assert_eq!(delivered.value, 33);
    assert_eq!(delivered.source_tag, 8);
    assert_eq!(registry.dequeue_signal(thread).unwrap(), None);
}

#[test]
fn prepared_standard_signal() {
    let (registry, process, thread) = Fixture::registry(8);
    let signal = Fixture::number(10);
    let mask = SignalMask::from_bits(0).with(signal);
    assert!(
        registry
            .enqueue_signal(PendingTarget::Process(process), Fixture::info(10, 1))
            .unwrap()
    );
    let prepared = registry.prepare_signal_wait(thread, mask).unwrap().unwrap();
    assert!(
        !registry
            .enqueue_signal(PendingTarget::Process(process), Fixture::info(10, 2))
            .unwrap()
    );
    assert!(registry.commit_signal_wait(prepared).unwrap());
    assert_eq!(registry.consume_signal_wait(thread, mask).unwrap(), None);
}

#[test]
fn prepared_signal_is() {
    let (registry, process, thread) = Fixture::registry(8);
    let signal = SignalNumber::new(35).unwrap();
    let mask = SignalMask::from_bits(1_u64 << 34);
    for value in [7, 7, 9] {
        registry
            .enqueue_signal(
                PendingTarget::Process(process),
                SignalInfo {
                    value,
                    ..SignalInfo::bare(signal)
                },
            )
            .unwrap();
    }
    let first = registry.prepare_signal_wait(thread, mask).unwrap().unwrap();
    assert_eq!(first.info().value, 7);
    assert!(registry.prepare_signal_wait(thread, mask).unwrap().is_none());
    assert_eq!(registry.consume_signal_wait(thread, mask).unwrap(), None);
    drop(first);
    let retry = registry.prepare_signal_wait(thread, mask).unwrap().unwrap();
    assert_eq!(retry.info().value, 7);
    assert!(registry.commit_signal_wait(retry).unwrap());
    assert_eq!(registry.consume_signal_wait(thread, mask).unwrap().unwrap().value, 7);
    assert_eq!(registry.consume_signal_wait(thread, mask).unwrap().unwrap().value, 9);
}

#[test]
fn standard_signals_coalesce() {
    let (registry, process, thread) = Fixture::registry(4);
    assert!(
        registry
            .enqueue_signal(PendingTarget::Process(process), Fixture::info(10, 11),)
            .unwrap()
    );
    assert!(
        !registry
            .enqueue_signal(PendingTarget::Process(process), Fixture::info(10, 22),)
            .unwrap()
    );

    let delivered = registry.dequeue_signal(thread).unwrap().unwrap();
    assert_eq!(delivered.0.value, 11);
    assert_eq!(delivered.1, DeliveryAction::Terminate { dumped_core: false });
}

#[test]
fn realtime_signals_use() {
    let (registry, process, thread) = Fixture::registry(8);
    for info in [Fixture::info(33, 1), Fixture::info(33, 2), Fixture::info(32, 3)] {
        registry.enqueue_signal(PendingTarget::Process(process), info).unwrap();
    }

    let mut delivered = Vec::new();
    while let Some((info, _)) = registry.dequeue_signal(thread).unwrap() {
        delivered.push((info.signal.get(), info.value));
    }
    assert_eq!(delivered, [(33, 1), (33, 2), (32, 3)]);
}

#[test]
fn wait_owner_priority() {
    let (registry, process, thread) = Fixture::registry(8);
    let selected = SignalMask::from_bits(0)
        .with(Fixture::number(10))
        .with(Fixture::number(12));
    registry
        .enqueue_signal(PendingTarget::Thread(thread), Fixture::info(10, 1))
        .unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(12, 2))
        .unwrap();

    assert_eq!(
        registry.consume_signal_wait(thread, selected).unwrap().unwrap().value,
        1
    );
    assert_eq!(
        registry.consume_signal_wait(thread, selected).unwrap().unwrap().value,
        2
    );
}

#[test]
fn masks_defer_delivery() {
    let (registry, process, thread) = Fixture::registry(4);
    let signal = Fixture::number(12);
    let selected = SignalMask::from_bits(0).with(signal);
    registry.set_signal_mask(thread, selected).unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(12, 44))
        .unwrap();

    assert_eq!(registry.dequeue_signal(thread).unwrap(), None);
    assert_eq!(
        registry.consume_signal_wait(thread, selected).unwrap().unwrap().value,
        44
    );
    assert_eq!(registry.consume_signal_wait(thread, selected).unwrap(), None);
}

#[test]
fn thread_pending_wins() {
    let (registry, process, thread) = Fixture::registry(4);
    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(10, 1))
        .unwrap();
    registry
        .enqueue_signal(PendingTarget::Thread(thread), Fixture::info(10, 2))
        .unwrap();

    assert_eq!(registry.dequeue_signal(thread).unwrap().unwrap().0.value, 2);
    assert_eq!(registry.dequeue_signal(thread).unwrap().unwrap().0.value, 1);
}

#[test]
fn dispositions_flush_ignored() {
    let (registry, process, thread) = Fixture::registry(4);
    let signal = Fixture::number(12);
    registry
        .enqueue_signal(PendingTarget::Thread(thread), Fixture::info(12, 1))
        .unwrap();
    let ignored = SignalAction {
        disposition: SignalDisposition::Ignore,
        ..SignalAction::DEFAULT
    };
    registry.set_action(process, signal, ignored).unwrap();
    assert_eq!(registry.dequeue_signal(thread).unwrap(), None);
    assert!(
        !registry
            .enqueue_signal(PendingTarget::Process(process), Fixture::info(12, 2),)
            .unwrap()
    );

    let handled = SignalAction {
        disposition: SignalDisposition::Handler(0x4000),
        flags: 7,
        restorer: 0x8000,
        mask: SignalMask::from_bits(4),
    };
    registry.set_action(process, signal, handled).unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(12, 3))
        .unwrap();
    assert_eq!(
        registry.dequeue_signal(thread).unwrap().unwrap().1,
        DeliveryAction::Handle(handled)
    );
}

#[test]
fn default_stop_continue() {
    let (registry, parent, source) = Fixture::registry(8);
    registry
        .enqueue_signal(PendingTarget::Process(parent), SignalInfo::bare(SignalNumber::STOP))
        .unwrap();
    assert_eq!(
        registry.dequeue_signal(source).unwrap().unwrap().1,
        DeliveryAction::Stop
    );
    assert_eq!(registry.snapshot().processes[0].lifecycle, ProcessLifecycle::Stopped);

    registry
        .enqueue_signal(PendingTarget::Process(parent), SignalInfo::bare(SignalNumber::CONTINUE))
        .unwrap();
    assert_eq!(registry.snapshot().processes[0].lifecycle, ProcessLifecycle::Running);
    assert_eq!(
        registry.dequeue_signal(source).unwrap().unwrap().1,
        DeliveryAction::Continue
    );

    let fork = registry.begin_fork_process(source).unwrap();
    let child = fork.process();
    let child_thread = fork.thread();
    registry.commit_fork_process(fork).unwrap();
    registry
        .set_limit(child, Resource::Core, Limit::new(1, 1).unwrap())
        .unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(child), Fixture::info(5, 0))
        .unwrap();
    assert_eq!(
        registry.dequeue_signal(child_thread).unwrap().unwrap().1,
        DeliveryAction::Terminate { dumped_core: true }
    );
    let child = registry
        .snapshot()
        .processes
        .into_iter()
        .find(|entry| entry.id == child)
        .unwrap();
    assert_eq!(child.lifecycle, ProcessLifecycle::Exiting);
}

#[test]
fn delivered_stop_wakes_parent_waiting_after_signal_enqueue() {
    let (registry, parent, source) = Fixture::registry(8);
    let registry = Arc::new(registry);
    let fork = registry.begin_fork_process(source).unwrap();
    let child = fork.process();
    let child_thread = fork.thread();
    registry.commit_fork_process(fork).unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(child), SignalInfo::bare(SignalNumber::STOP))
        .unwrap();

    // A parent can enter waitpid after signal enqueue but before the child
    // consumes SIGSTOP. Only the delivered stop transition can wake it.
    let observed = registry.wait_observation();
    let waiter_registry = Arc::clone(&registry);
    let (sent, received) = std::sync::mpsc::channel();
    let waiter = thread::spawn(move || {
        waiter_registry.wait_change(observed);
        sent.send(()).unwrap();
    });
    thread::sleep(std::time::Duration::from_millis(10));
    assert_eq!(
        registry.dequeue_signal(child_thread).unwrap().unwrap().1,
        DeliveryAction::Stop,
    );
    received.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
    waiter.join().unwrap();
    let event = registry
        .prepare_wait_child(
            parent,
            ChildSelector::Process(child),
            ChildWaitOptions {
                report_stopped: true,
                ..ChildWaitOptions::default()
            },
        )
        .unwrap();
    assert!(matches!(event, PreparedChildWait::Selection(_)));
}

#[test]
fn ignored_continue_resumes_and_publishes_control_activity() {
    let (registry, process, thread) = Fixture::registry(8);
    registry
        .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(SignalNumber::STOP))
        .unwrap();
    assert_eq!(
        registry.dequeue_signal(thread).unwrap().unwrap().1,
        DeliveryAction::Stop
    );
    let ignored = SignalAction {
        disposition: SignalDisposition::Ignore,
        ..SignalAction::DEFAULT
    };
    registry.set_action(process, SignalNumber::CONTINUE, ignored).unwrap();
    let observed = registry.activity_observation();
    assert!(
        !registry
            .enqueue_signal(
                PendingTarget::Process(process),
                SignalInfo::bare(SignalNumber::CONTINUE)
            )
            .unwrap()
    );
    assert_eq!(registry.snapshot().processes[0].lifecycle, ProcessLifecycle::Running);
    assert_ne!(registry.activity_observation(), observed);
}

#[test]
fn core_disposition_sets() {
    let (registry, _, source) = Fixture::registry(4);
    let fork = registry.begin_fork_process(source).unwrap();
    let child = fork.process();
    let child_thread = fork.thread();
    registry.commit_fork_process(fork).unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(child), Fixture::info(5, 0))
        .unwrap();
    assert_eq!(
        registry.dequeue_signal(child_thread).unwrap().unwrap().1,
        DeliveryAction::Terminate { dumped_core: false }
    );
}

#[test]
fn masks_actions_altstack() {
    let (registry, process, thread) = Fixture::registry(8);
    let handler_signal = Fixture::number(12);
    let ignored_signal = Fixture::number(10);
    let handler = SignalAction {
        disposition: SignalDisposition::Handler(0x1000),
        ..SignalAction::DEFAULT
    };
    let ignored = SignalAction {
        disposition: SignalDisposition::Ignore,
        ..SignalAction::DEFAULT
    };
    let mask = SignalMask::from_bits(0).with(handler_signal);
    let stack = AlternateStack::Enabled {
        pointer: 0x8000,
        size: 0x4000,
    };
    registry.set_action(process, handler_signal, handler).unwrap();
    registry.set_action(process, ignored_signal, ignored).unwrap();
    registry.set_signal_mask(thread, mask).unwrap();
    registry.set_alternate_stack(thread, stack).unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(12, 9))
        .unwrap();

    let fork = registry.fork_plan(thread).unwrap();
    assert_eq!(fork.process.pending, []);
    assert_eq!(fork.thread.pending, []);
    assert_eq!(fork.thread.mask, mask);
    assert_eq!(fork.thread.alternate_stack, stack);
    assert!(fork.process.actions.contains(&(handler_signal, handler)));
    assert!(fork.process.actions.contains(&(ignored_signal, ignored)));

    let exec = registry.exec_plan(thread).unwrap();
    assert_eq!(exec.process.actions, [(ignored_signal, ignored)]);
    assert_eq!(exec.process.pending.len(), 1);
    assert_eq!(exec.thread.mask, mask);
    assert_eq!(exec.thread.alternate_stack, AlternateStack::Disabled);
}

#[test]
fn unblockable_masks_and() {
    let (registry, process, thread) = Fixture::registry(4);
    let requested = SignalMask::from_bits(u64::MAX);
    registry.set_signal_mask(thread, requested).unwrap();
    let snapshot = registry.snapshot();
    assert!(!snapshot.threads[0].signals.mask.contains(SignalNumber::KILL));
    assert!(!snapshot.threads[0].signals.mask.contains(SignalNumber::STOP));
    assert_eq!(
        registry.set_action(
            process,
            SignalNumber::KILL,
            SignalAction {
                disposition: SignalDisposition::Ignore,
                ..SignalAction::DEFAULT
            },
        ),
        Err(TaskError::InvalidLifecycle)
    );
    assert_eq!(
        registry.set_alternate_stack(thread, AlternateStack::Enabled { pointer: 1, size: 0 },),
        Err(TaskError::InvalidLifecycle)
    );
}

#[test]
fn unblockable_delivery() {
    let (registry, process, thread) = Fixture::registry(4);
    registry
        .set_signal_mask(thread, SignalMask::from_bits(u64::MAX))
        .unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(SignalNumber::STOP))
        .unwrap();

    let prepared = registry.prepare_deliverable_signal(thread).unwrap().unwrap();
    assert_eq!(prepared.info().signal, SignalNumber::STOP);
    assert!(registry.commit_signal_wait(prepared).unwrap());
    assert_eq!(registry.pending_signal_mask(thread).unwrap(), SignalMask::from_bits(0));
}

#[test]
fn sigreturn_exposes_pending_asynchronous_delivery() {
    let (registry, process, thread) = Fixture::registry(4);
    let signal = Fixture::number(10);
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
        .enqueue_signal(PendingTarget::Thread(thread), SignalInfo::bare(signal))
        .unwrap();

    registry
        .replace_signal_context(thread, SignalMask::from_bits(0), AlternateStack::Disabled)
        .unwrap();

    let prepared = registry.prepare_deliverable_signal(thread).unwrap().unwrap();
    assert_eq!(prepared.info().signal, signal);
}

#[test]
fn sigreturn_does_not_defer_synchronous_or_kill_delivery() {
    for (signal, code) in [(SignalNumber::KILL, 0), (Fixture::number(8), 1)] {
        let (registry, process, thread) = Fixture::registry(4);
        let information = SignalInfo {
            code,
            ..SignalInfo::bare(signal)
        };
        registry
            .enqueue_signal(PendingTarget::Process(process), information)
            .unwrap();
        registry
            .replace_signal_context(thread, SignalMask::from_bits(0), AlternateStack::Disabled)
            .unwrap();

        let prepared = registry.prepare_deliverable_signal(thread).unwrap().unwrap();
        assert_eq!(prepared.info().signal, signal);
    }
}

#[test]
fn pending_batch_drains_high_to_low() {
    let (registry, process, thread) = Fixture::registry(8);
    for signal in [12, 10, 15] {
        registry
            .enqueue_signal(
                PendingTarget::Process(process),
                Fixture::info(signal, u64::from(signal)),
            )
            .unwrap();
    }

    for signal in [15, 12, 10] {
        assert_eq!(
            Fixture::enter_handler(&registry, thread).signal,
            Fixture::number(signal)
        );
        assert!(registry.prepare_deliverable_signal(thread).unwrap().is_none());
        Fixture::return_handler(&registry, thread);
    }
}

#[test]
fn realtime_batch_preserves_fifo() {
    let (registry, process, thread) = Fixture::registry(8);
    for value in [11, 22, 33] {
        registry
            .enqueue_signal(PendingTarget::Process(process), Fixture::info(35, value))
            .unwrap();
    }

    for value in [11, 22, 33] {
        assert_eq!(Fixture::enter_handler(&registry, thread).value, value);
        Fixture::return_handler(&registry, thread);
    }
}

#[test]
fn standard_batch_coalesces() {
    let (registry, process, thread) = Fixture::registry(8);
    assert!(
        registry
            .enqueue_signal(PendingTarget::Process(process), Fixture::info(12, 1))
            .unwrap()
    );
    assert!(
        !registry
            .enqueue_signal(PendingTarget::Process(process), Fixture::info(12, 2))
            .unwrap()
    );

    assert_eq!(Fixture::enter_handler(&registry, thread).value, 1);
    Fixture::return_handler(&registry, thread);
    assert!(registry.prepare_deliverable_signal(thread).unwrap().is_none());
}

#[test]
fn handler_arrival_nests_before_deferred_batch() {
    let (registry, process, thread) = Fixture::registry(8);
    for signal in [12, 10] {
        registry
            .enqueue_signal(
                PendingTarget::Process(process),
                Fixture::info(signal, u64::from(signal)),
            )
            .unwrap();
    }
    assert_eq!(Fixture::enter_handler(&registry, thread).signal, Fixture::number(12));

    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(14, 14))
        .unwrap();
    assert_eq!(Fixture::enter_handler(&registry, thread).signal, Fixture::number(14));
    Fixture::return_handler(&registry, thread);
    assert!(registry.prepare_deliverable_signal(thread).unwrap().is_none());

    Fixture::return_handler(&registry, thread);
    assert_eq!(Fixture::enter_handler(&registry, thread).signal, Fixture::number(10));
}

#[test]
fn nonlocal_unwind_releases_exact_scopes() {
    let (registry, process, thread) = Fixture::registry(8);
    for signal in [12, 10] {
        registry
            .enqueue_signal(
                PendingTarget::Process(process),
                Fixture::info(signal, u64::from(signal)),
            )
            .unwrap();
    }
    assert_eq!(Fixture::enter_handler(&registry, thread).signal, Fixture::number(12));
    registry
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(14, 14))
        .unwrap();
    assert_eq!(Fixture::enter_handler(&registry, thread).signal, Fixture::number(14));

    assert_eq!(registry.unwind_signal_frames(thread, 0x1f_800).unwrap(), 1);
    assert!(registry.prepare_deliverable_signal(thread).unwrap().is_none());
    assert_eq!(registry.unwind_signal_frames(thread, 0x21_000).unwrap(), 1);
    assert_eq!(Fixture::enter_handler(&registry, thread).signal, Fixture::number(10));
}

#[test]
fn synchronous_fault_bypasses_deferred_batch() {
    let (registry, process, thread) = Fixture::registry(8);
    for signal in [12, 10] {
        registry
            .enqueue_signal(
                PendingTarget::Process(process),
                Fixture::info(signal, u64::from(signal)),
            )
            .unwrap();
    }
    assert_eq!(Fixture::enter_handler(&registry, thread).signal, Fixture::number(12));
    let fault = SignalInfo {
        code: 1,
        address: 0x4040,
        ..SignalInfo::bare(Fixture::number(8))
    };
    registry.enqueue_signal(PendingTarget::Thread(thread), fault).unwrap();

    let prepared = registry.prepare_deliverable_signal(thread).unwrap().unwrap();
    assert_eq!(prepared.info(), fault);
}

#[test]
fn deferred_readiness() {
    let (registry, process, thread) = Fixture::registry(8);
    for signal in [12, 10] {
        registry
            .set_action(
                process,
                Fixture::number(signal),
                SignalAction {
                    disposition: SignalDisposition::Handler(0x4000),
                    ..SignalAction::DEFAULT
                },
            )
            .unwrap();
        registry
            .enqueue_signal(
                PendingTarget::Process(process),
                Fixture::info(signal, u64::from(signal)),
            )
            .unwrap();
    }
    assert_eq!(Fixture::enter_handler(&registry, thread).signal, Fixture::number(12));

    assert_eq!(registry.dequeue_signal(thread).unwrap(), None);
    assert!(!registry.has_interrupting_signal(thread, None).unwrap());
    assert_eq!(registry.restart_interrupted_signal(thread).unwrap(), None);
    assert!(
        !registry
            .has_deliverable_except(thread, SignalMask::from_bits(0))
            .unwrap()
    );
}

#[test]
fn depth_cap() {
    let (registry, process, thread) = Fixture::registry(64);
    let signal = Fixture::number(35);
    for value in 0..33 {
        registry
            .enqueue_signal(PendingTarget::Process(process), Fixture::info(35, value))
            .unwrap();
    }
    for depth in 0..33_u64 {
        let prepared = registry.prepare_deliverable_signal(thread).unwrap().unwrap();
        registry.force_signal_delivery(prepared).unwrap();
        let forced = registry.prepare_forced_delivery(thread).unwrap();
        registry
            .commit_frame_delivery(
                forced,
                SignalMask::from_bits(0),
                AlternateStack::Disabled,
                0x40_000 - depth * 0x1000,
                false,
            )
            .unwrap();
    }
    let before = registry.deliver_thread_state(thread).unwrap();
    assert_eq!(before.frames.len(), SIGNAL_FRAME_MAXIMUM);
    assert_eq!(before.frames.last().unwrap().stack_pointer, 0x20_000);

    registry
        .replace_signal_context(thread, SignalMask::from_bits(0), AlternateStack::Disabled)
        .unwrap();
    assert_eq!(registry.deliver_thread_state(thread).unwrap().frames.len(), 31);
    assert!(!registry.pending_signal_mask(thread).unwrap().contains(signal));
}

#[test]
fn handler_fork() {
    let (registry, process, thread) = Fixture::registry(8);
    for signal in [12, 10] {
        registry
            .enqueue_signal(
                PendingTarget::Process(process),
                Fixture::info(signal, u64::from(signal)),
            )
            .unwrap();
    }
    assert_eq!(Fixture::enter_handler(&registry, thread).signal, Fixture::number(12));
    let source = registry.deliver_thread_state(thread).unwrap();

    let plan = registry.begin_fork_process(thread).unwrap();
    let child_thread = plan.thread();
    registry.commit_fork_process(plan).unwrap();
    let child = registry.deliver_thread_state(child_thread).unwrap();
    assert_eq!(child.deferred, source.deferred);
    assert_eq!(child.frames, source.frames);
}

#[test]
fn synchronous_forces_default() {
    let (registry, process, thread) = Fixture::registry(4);
    let signal = SignalNumber::new(8).unwrap();
    registry.set_signal_mask(thread, SignalMask::from_bits(1 << 7)).unwrap();
    registry
        .set_action(
            process,
            signal,
            SignalAction {
                disposition: SignalDisposition::Ignore,
                ..SignalAction::DEFAULT
            },
        )
        .unwrap();
    registry
        .enqueue_signal(
            PendingTarget::Thread(thread),
            SignalInfo {
                code: 1,
                address: 0x401000,
                ..SignalInfo::bare(signal)
            },
        )
        .unwrap();

    let prepared = registry.prepare_deliverable_signal(thread).unwrap().unwrap();
    assert_eq!(prepared.info().address, 0x401000);
    registry.force_signal_delivery(prepared).unwrap();
    let forced = registry.prepare_forced_delivery(thread).unwrap();
    let (_, action, _) = registry.commit_forced_delivery(forced).unwrap().unwrap();
    assert_eq!(action, DeliveryAction::Terminate { dumped_core: false });
}

#[test]
fn realtime_queue_bound() {
    let (registry, process, _) = Fixture::registry(8);
    let registry = Arc::new(registry);
    let producers = (0..32)
        .map(|value| {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.enqueue_signal(PendingTarget::Process(process), Fixture::info(32, value)))
        })
        .collect::<Vec<_>>();
    let mut accepted = 0;
    let mut rejected = 0;
    for producer in producers {
        match producer.join().unwrap() {
            Ok(true) => accepted += 1,
            Err(TaskError::SignalQueueLimit) => rejected += 1,
            result => panic!("unexpected enqueue result: {result:?}"),
        }
    }
    assert_eq!((accepted, rejected), (8, 24));
    assert_eq!(registry.snapshot().processes[0].signals.pending.len(), 8);
}

#[test]
fn zero_signal_capacity() {
    assert!(matches!(
        TaskRegistry::new(RegistryConfig {
            max_pending_signals: 0,
            ..RegistryConfig::default()
        }),
        Err(TaskError::InvalidCapacity)
    ));
}
