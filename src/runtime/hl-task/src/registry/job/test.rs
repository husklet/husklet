use std::sync::Arc;
use std::thread;

use crate::*;

struct Fixture;

impl Fixture {
    fn registry() -> (TaskRegistry, ProcessId, ThreadId) {
        let registry = TaskRegistry::new(RegistryConfig {
            max_processes: 12,
            max_threads: 12,
            max_groups: 8,
            max_pending_signals: 8,
            online_cpus: 1,
        })
        .unwrap();
        let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let identity = registry.create_init(credentials, ProcessLimits::empty()).unwrap();
        (registry, identity.0, identity.1)
    }

    fn fork(registry: &TaskRegistry, source: ThreadId) -> (ProcessId, ThreadId) {
        let plan = registry.begin_fork_process(source).unwrap();
        let identity = (plan.process(), plan.thread());
        registry.commit_fork_process(plan).unwrap();
        identity
    }
}

#[test]
fn init_and_fork() {
    let (registry, init, source) = Fixture::registry();
    let (child, _) = Fixture::fork(&registry, source);
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.process_groups.len(), 1);
    assert_eq!(snapshot.process_groups[0].members, [init, child]);
    assert_eq!(snapshot.processes[0].session, snapshot.processes[1].session);
    assert_eq!(snapshot.processes[0].process_group, snapshot.processes[1].process_group);
}

#[test]
fn no_child_wait_releases_exited_child() {
    let (registry, parent, source) = Fixture::registry();
    registry
        .set_action(
            parent,
            SignalNumber::new(17).unwrap(),
            SignalAction {
                flags: 2,
                ..SignalAction::DEFAULT
            },
        )
        .unwrap();
    let (child, _) = Fixture::fork(&registry, source);
    registry.charge_cpu(child, 11).unwrap();

    registry.exit_process(child, ExitStatus::Code(7)).unwrap();

    assert_eq!(
        registry.wait_child(parent, ChildSelector::Any, ChildWaitOptions::default()),
        Err(TaskError::NoChildren)
    );
    assert!(!registry.snapshot().processes.iter().any(|process| process.id == child));
    assert_eq!(registry.cpu_usage(parent).unwrap().children_nanoseconds, 11);
}

#[test]
fn fork_signal_state() {
    let (registry, parent, source) = Fixture::registry();
    let signal = SignalNumber::new(12).unwrap();
    let action = SignalAction {
        disposition: SignalDisposition::Handler(0x4000),
        ..SignalAction::DEFAULT
    };
    let mask = SignalMask::from_bits(0).with(signal);
    registry.set_action(parent, signal, action).unwrap();
    registry.set_signal_mask(source, mask).unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(parent), SignalInfo::bare(signal))
        .unwrap();
    let (child, child_thread) = Fixture::fork(&registry, source);
    let snapshot = registry.snapshot();
    let child = snapshot.processes.iter().find(|value| value.id == child).unwrap();
    let thread = snapshot.threads.iter().find(|value| value.id == child_thread).unwrap();
    assert!(child.signals.actions.contains(&(signal, action)));
    assert!(child.signals.pending.is_empty());
    assert_eq!(thread.signals.mask, mask);
    assert!(thread.signals.pending.is_empty());
}

#[test]
fn setsid_creates_new() {
    let (registry, init, source) = Fixture::registry();
    let (child, child_thread) = Fixture::fork(&registry, source);
    let old_session = registry.session_id(child).unwrap();
    let session = registry.create_session(child).unwrap();
    let group = registry.process_group_id(child).unwrap();
    assert_ne!(session, old_session);
    assert_eq!(session.number(), group.number());
    assert_eq!(registry.terminal_session(child).unwrap(), None);
    registry.attach_terminal(child, session).unwrap();
    assert_eq!(registry.terminal_session(child).unwrap(), Some(session));
    assert_eq!(registry.create_session(child), Err(TaskError::SessionLeader));
    registry.exit_process(child, ExitStatus::Code(0)).unwrap();
    registry.reap(init, child).unwrap();

    let (replacement, _) = Fixture::fork(&registry, source);
    let replacement_session = registry.create_session(replacement).unwrap();
    assert_eq!(replacement_session.number(), replacement.number());
    assert_ne!(replacement_session.number(), session.number());
    assert_ne!(replacement_session, session);
    assert_eq!(registry.session_id(child), Err(TaskError::InvalidProcess));
    assert_eq!(
        registry.request_cancellation(child_thread),
        Err(TaskError::InvalidThread)
    );
}

#[test]
fn setpgid_requires_same() {
    let (registry, init, source) = Fixture::registry();
    let (child, child_thread) = Fixture::fork(&registry, source);
    let group = registry.set_process_group(init, child, None).unwrap();
    assert!(!registry.snapshot().process_groups[1].orphaned);
    registry.mark_exec(child).unwrap();
    assert_eq!(
        registry.set_process_group(init, child, None),
        Err(TaskError::ProcessExeced)
    );

    let (grandchild, _) = Fixture::fork(&registry, child_thread);
    let session = registry.create_session(grandchild).unwrap();
    assert_ne!(session, registry.session_id(init).unwrap());
    assert_eq!(
        registry.set_process_group(init, grandchild, Some(group)),
        Err(TaskError::WrongProcess)
    );
}

#[test]
fn exiting_session_parent() {
    let (registry, init, source) = Fixture::registry();
    let (parent, parent_thread) = Fixture::fork(&registry, source);
    registry.create_session(parent).unwrap();
    let (child, child_thread) = Fixture::fork(&registry, parent_thread);
    let child_group = registry.set_process_group(parent, child, None).unwrap();
    let before = registry
        .snapshot()
        .process_groups
        .into_iter()
        .find(|group| group.id == child_group)
        .unwrap();
    assert!(!before.orphaned);

    let handled = SignalAction {
        disposition: SignalDisposition::Handler(0x4000),
        ..SignalAction::DEFAULT
    };
    let hangup = SignalNumber::new(1).unwrap();
    registry.set_action(child, hangup, handled).unwrap();
    registry.set_action(child, SignalNumber::CONTINUE, handled).unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(child), SignalInfo::bare(SignalNumber::STOP))
        .unwrap();
    registry.dequeue_signal(child_thread).unwrap();
    assert_eq!(
        registry
            .snapshot()
            .processes
            .iter()
            .find(|entry| entry.id == child)
            .unwrap()
            .lifecycle,
        ProcessLifecycle::Stopped
    );

    registry.exit_process(parent, ExitStatus::Code(0)).unwrap();
    let snapshot = registry.snapshot();
    let after = snapshot
        .process_groups
        .into_iter()
        .find(|group| group.id == child_group)
        .unwrap();
    assert!(after.orphaned);
    let child_state = snapshot.processes.iter().find(|entry| entry.id == child).unwrap();
    assert_eq!(child_state.lifecycle, ProcessLifecycle::Running);
    assert_eq!(
        child_state
            .signals
            .pending
            .iter()
            .map(|info| info.signal)
            .collect::<Vec<_>>(),
        [hangup, SignalNumber::CONTINUE]
    );
    assert_eq!(
        snapshot
            .processes
            .iter()
            .find(|entry| entry.id == child)
            .unwrap()
            .parent,
        Some(init)
    );
}

#[test]
fn foreground_group_is() {
    struct Terminal;

    impl TerminalControl for Terminal {
        type Error = ();

        fn foreground_changed(&self, _session: SessionId, _group: ProcessGroupId) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    let (registry, init, source) = Fixture::registry();
    let (child, _) = Fixture::fork(&registry, source);
    let group = registry.set_process_group(init, child, None).unwrap();
    let event = registry.set_foreground_group(init, group).unwrap();
    event.deliver(&Terminal).unwrap();
    assert_eq!(registry.snapshot().sessions[0].foreground_group, Some(group));

    let (detached, _) = Fixture::fork(&registry, source);
    registry.create_session(detached).unwrap();
    let detached_group = registry.process_group_id(detached).unwrap();
    assert_eq!(
        registry.set_foreground_group(init, detached_group),
        Err(TaskError::InvalidProcessGroup)
    );
}

#[test]
fn terminal_transitions_validate_and_signal_the_same_session_foreground_group() {
    let (registry, _, source) = Fixture::registry();
    let (leader, leader_thread) = Fixture::fork(&registry, source);
    let session = registry.create_session(leader).unwrap();
    registry.attach_terminal(leader, session).unwrap();
    let (worker, worker_thread) = Fixture::fork(&registry, leader_thread);
    let worker_group = registry.set_process_group(leader, worker, None).unwrap();
    registry.set_foreground_group(leader, worker_group).unwrap();
    for signal in [1, 18, 28] {
        registry
            .set_action(
                worker,
                SignalNumber::new(signal).unwrap(),
                SignalAction {
                    disposition: SignalDisposition::Handler(0x4000),
                    ..SignalAction::DEFAULT
                },
            )
            .unwrap();
    }

    assert_eq!(registry.pending_signal_mask(worker_thread).unwrap().bits(), 0);
    let window = registry
        .terminal_window_changed(session.number(), worker_group)
        .unwrap();
    assert_eq!(window.session, session);
    assert_eq!(window.foreground, Some(worker_group));
    assert_eq!(window.signals, [SignalNumber::new(28).ok(), None]);
    assert!(!window.session_wide);
    assert_eq!(registry.pending_signal_mask(worker_thread).unwrap().bits(), 1 << 27);

    let detach = registry
        .prepare_terminal_transition(leader, crate::TerminalTransition::Detach)
        .unwrap();
    let detach = detach.commit();
    assert_eq!(
        detach.signals,
        [SignalNumber::new(1).ok(), Some(SignalNumber::CONTINUE)]
    );
    assert!(detach.session_wide);
    let pending = registry.pending_signal_mask(worker_thread).unwrap().bits();
    assert_ne!(pending & 1, 0);
    assert_ne!(pending & (1 << 17), 0);

    assert!(matches!(
        registry.prepare_terminal_transition(worker, crate::TerminalTransition::SessionLeaderExit),
        Err(TaskError::InvalidSession)
    ));
}

#[test]
fn session_leader_exit_publishes_hangup_without_continue() {
    let (registry, _, source) = Fixture::registry();
    let (leader, leader_thread) = Fixture::fork(&registry, source);
    let session = registry.create_session(leader).unwrap();
    registry.attach_terminal(leader, session).unwrap();
    let (worker, worker_thread) = Fixture::fork(&registry, leader_thread);
    let worker_group = registry.set_process_group(leader, worker, None).unwrap();
    registry.set_foreground_group(leader, worker_group).unwrap();
    registry
        .set_action(
            worker,
            SignalNumber::new(1).unwrap(),
            SignalAction {
                disposition: SignalDisposition::Handler(0x4000),
                ..SignalAction::DEFAULT
            },
        )
        .unwrap();

    let effects = registry
        .prepare_terminal_transition(leader, crate::TerminalTransition::SessionLeaderExit)
        .unwrap();
    let effects = effects.commit();
    assert_eq!(effects.signals, [SignalNumber::new(1).ok(), None]);
    assert_eq!(registry.pending_signal_mask(worker_thread).unwrap().bits(), 1);
}

#[test]
fn nonleader_detach_clears_only_the_callers_terminal_association() {
    let (registry, leader, source) = Fixture::registry();
    let (worker, worker_thread) = Fixture::fork(&registry, source);
    let (peer, _) = Fixture::fork(&registry, worker_thread);
    let session = registry.session_id(leader).unwrap();

    let prepared = registry
        .prepare_terminal_transition(worker, crate::TerminalTransition::Detach)
        .unwrap();
    assert_eq!(prepared.effects().session, session);
    assert!(!prepared.effects().session_wide);
    let _ = prepared.commit();

    assert_eq!(registry.terminal_session(worker).unwrap(), None);
    assert_eq!(registry.terminal_session(leader).unwrap(), Some(session));
    assert_eq!(registry.terminal_session(peer).unwrap(), Some(session));
    let detached_child = Fixture::fork(&registry, worker_thread).0;
    assert_eq!(registry.terminal_session(detached_child).unwrap(), None);
    let restored = crate::TaskRegistry::restore(&registry.snapshot()).unwrap();
    assert_eq!(restored.terminal_session(worker).unwrap(), None);
    assert_eq!(restored.terminal_session(peer).unwrap(), Some(session));
    assert!(
        registry
            .prepare_terminal_transition(worker, crate::TerminalTransition::Detach)
            .is_err()
    );
}

#[test]
fn window_signal_rejects_other_session_and_stale_group_identity() {
    let (registry, leader, source) = Fixture::registry();
    let (worker, worker_thread) = Fixture::fork(&registry, source);
    let foreground = registry.set_process_group(leader, worker, None).unwrap();
    registry.set_foreground_group(leader, foreground).unwrap();
    let other_plan = registry.begin_fork_process(worker_thread).unwrap();
    let other = other_plan.process();
    registry.commit_fork_process(other_plan).unwrap();
    let other_session = registry.create_session(other).unwrap();

    assert_eq!(
        registry.terminal_window_changed(other_session.number(), foreground),
        Err(TaskError::InvalidProcessGroup),
    );
    let (slot, generation) = foreground.wire_parts();
    let stale = crate::ProcessGroupId::from_wire(slot, generation.saturating_add(1)).unwrap();
    assert_eq!(
        registry.terminal_window_changed(registry.session_id(leader).unwrap().number(), stale),
        Err(TaskError::InvalidProcessGroup),
    );
}

#[test]
fn wait_filters_class() {
    let (registry, parent, source) = Fixture::registry();
    let (standard, standard_thread) = Fixture::fork(&registry, source);
    let (clone, _) = Fixture::fork(&registry, source);
    registry.set_child_class(clone, ChildClass::Clone).unwrap();
    let group = registry.set_process_group(parent, standard, None).unwrap();

    assert_eq!(
        registry
            .wait_child(
                parent,
                ChildSelector::Any,
                ChildWaitOptions {
                    no_hang: true,
                    ..ChildWaitOptions::default()
                },
            )
            .unwrap(),
        ChildWaitResult::NoChange
    );
    registry
        .enqueue_signal(PendingTarget::Process(standard), SignalInfo::bare(SignalNumber::STOP))
        .unwrap();
    registry.dequeue_signal(standard_thread).unwrap();
    let stopped_options = ChildWaitOptions {
        report_stopped: true,
        keep_waitable: true,
        class: ChildClassSelector::All,
        ..ChildWaitOptions::default()
    };
    let first = registry
        .wait_child(parent, ChildSelector::ProcessGroup(group), stopped_options)
        .unwrap();
    assert_eq!(
        first,
        registry
            .wait_child(parent, ChildSelector::Process(standard), stopped_options,)
            .unwrap()
    );
    registry
        .wait_child(
            parent,
            ChildSelector::Process(standard),
            ChildWaitOptions {
                keep_waitable: false,
                ..stopped_options
            },
        )
        .unwrap();

    registry.exit_process(clone, ExitStatus::Code(7)).unwrap();
    assert_eq!(
        registry
            .wait_child(parent, ChildSelector::Any, ChildWaitOptions::default())
            .unwrap(),
        ChildWaitResult::WouldBlock
    );
    assert!(matches!(
        registry
            .wait_child(
                parent,
                ChildSelector::Any,
                ChildWaitOptions {
                    class: ChildClassSelector::Clone,
                    ..ChildWaitOptions::default()
                },
            )
            .unwrap(),
        ChildWaitResult::Event(ChildEvent {
            child,
            kind: ChildEventKind::Exited(ExitStatus::Code(7)),
            ..
        }) if child == clone
    ));
}

#[test]
fn continued_and_exit() {
    let (registry, parent, source) = Fixture::registry();
    let (child, child_thread) = Fixture::fork(&registry, source);
    registry
        .enqueue_signal(PendingTarget::Process(child), SignalInfo::bare(SignalNumber::STOP))
        .unwrap();
    registry.dequeue_signal(child_thread).unwrap();
    registry
        .enqueue_signal(PendingTarget::Process(child), SignalInfo::bare(SignalNumber::CONTINUE))
        .unwrap();
    registry.exit_process(child, ExitStatus::Code(3)).unwrap();
    let events = registry.snapshot().child_events;
    assert_eq!(events.len(), 3);
    assert!(events.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));

    let continued = registry
        .wait_child(
            parent,
            ChildSelector::Any,
            ChildWaitOptions {
                report_continued: true,
                ..ChildWaitOptions::default()
            },
        )
        .unwrap();
    assert!(matches!(
        continued,
        ChildWaitResult::Event(ChildEvent {
            kind: ChildEventKind::Continued,
            ..
        })
    ));
}

#[test]
fn concurrent_group_publication() {
    let (registry, parent, source) = Fixture::registry();
    let children = (0..8).map(|_| Fixture::fork(&registry, source).0).collect::<Vec<_>>();
    let registry = Arc::new(registry);
    let workers = children
        .iter()
        .copied()
        .map(|child| {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.set_process_group(parent, child, None).unwrap())
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }

    let snapshot = registry.snapshot();
    for child in children {
        let process = snapshot.processes.iter().find(|entry| entry.id == child).unwrap();
        let memberships = snapshot
            .process_groups
            .iter()
            .filter(|group| group.members.contains(&child))
            .collect::<Vec<_>>();
        assert_eq!(memberships.len(), 1);
        assert_eq!(memberships[0].id, process.process_group);
    }
}

#[test]
fn deterministic_group_reuse() {
    let (registry, parent, source) = Fixture::registry();
    let parent_group = registry.process_group_id(parent).unwrap();
    let (child, _) = Fixture::fork(&registry, source);
    let mut retired = Vec::new();
    for _ in 0..100 {
        let group = registry.set_process_group(parent, child, None).unwrap();
        retired.push(group);
        registry.set_process_group(parent, child, Some(parent_group)).unwrap();
    }
    for group in retired {
        assert_eq!(
            registry.set_foreground_group(parent, group),
            Err(TaskError::InvalidProcessGroup)
        );
    }
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.process_groups.len(), 1);
    assert_eq!(snapshot.process_groups[0].members, [parent, child]);
}

#[test]
fn pid_job_reuse_is_delayed() {
    let (registry, parent, source) = Fixture::registry();
    let registry = Arc::new(registry);
    let (leader, leader_thread) = Fixture::fork(&registry, source);
    let (member, _) = Fixture::fork(&registry, leader_thread);
    let group = registry.set_process_group(parent, leader, None).unwrap();
    registry.set_process_group(leader, member, Some(group)).unwrap();
    registry.exit_process(leader, ExitStatus::Code(0)).unwrap();
    registry.reap(parent, leader).unwrap();

    let sources = (0..3)
        .map(|_| {
            let plan = registry.begin_clone_thread(source).unwrap();
            registry.commit_clone_thread(plan).unwrap()
        })
        .collect::<Vec<_>>();
    let workers = sources
        .into_iter()
        .map(|source| {
            let registry = Arc::clone(&registry);
            thread::spawn(move || Fixture::fork(&registry, source).0)
        })
        .collect::<Vec<_>>();
    let children = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert!(children.iter().all(|child| child.number() != leader.number()));
    for child in children {
        registry.exit_process(child, ExitStatus::Code(0)).unwrap();
        registry.reap(parent, child).unwrap();
    }
    registry.exit_process(member, ExitStatus::Code(0)).unwrap();
    registry.reap(parent, member).unwrap();
    let replacement = Fixture::fork(&registry, source).0;
    assert_ne!(replacement.number(), leader.number());
    assert_ne!(replacement, leader);
}

#[test]
fn prepared_wait_reservation() {
    let (registry, parent, source) = Fixture::registry();
    let (child, _) = Fixture::fork(&registry, source);
    registry.exit_process(child, ExitStatus::Code(3)).unwrap();
    let options = ChildWaitOptions {
        no_hang: true,
        ..ChildWaitOptions::default()
    };
    let PreparedChildWait::Selection(first) = registry
        .prepare_wait_child(parent, ChildSelector::Any, options)
        .unwrap()
    else {
        panic!("missing prepared event")
    };
    assert!(matches!(
        registry
            .prepare_wait_child(parent, ChildSelector::Any, options)
            .unwrap(),
        PreparedChildWait::NoChange,
    ));
    drop(first);
    let PreparedChildWait::Selection(retry) = registry
        .prepare_wait_child(parent, ChildSelector::Any, options)
        .unwrap()
    else {
        panic!("reservation was not released")
    };
    assert_eq!(retry.commit().unwrap().child, child);
    assert!(matches!(
        registry.prepare_wait_child(parent, ChildSelector::Any, options),
        Err(TaskError::NoChildren),
    ));
}

#[test]
fn reap_usage() {
    let (registry, parent, source) = Fixture::registry();
    let (child, child_thread) = Fixture::fork(&registry, source);
    let (grandchild, _) = Fixture::fork(&registry, child_thread);
    registry.charge_cpu(grandchild, 3).unwrap();
    registry.exit_process(grandchild, ExitStatus::Code(0)).unwrap();
    let PreparedChildWait::Selection(selection) = registry
        .prepare_wait_child(child, ChildSelector::Any, ChildWaitOptions::default())
        .unwrap()
    else {
        panic!("missing grandchild");
    };
    selection.commit().unwrap();
    registry.charge_cpu(child, 5).unwrap();
    registry.exit_process(child, ExitStatus::Code(0)).unwrap();
    let PreparedChildWait::Selection(selection) = registry
        .prepare_wait_child(parent, ChildSelector::Any, ChildWaitOptions::default())
        .unwrap()
    else {
        panic!("missing child");
    };
    assert_eq!(selection.usage().unwrap().total_nanoseconds(), 8);
    selection.commit().unwrap();
    assert_eq!(registry.cpu_usage(parent).unwrap().children_nanoseconds, 8);
    assert!(matches!(
        registry.prepare_wait_child(parent, ChildSelector::Any, ChildWaitOptions::default()),
        Err(TaskError::NoChildren),
    ));
    assert_eq!(registry.cpu_usage(parent).unwrap().children_nanoseconds, 8);
}

#[test]
fn orphan_usage() {
    let (registry, init, source) = Fixture::registry();
    let (parent, parent_thread) = Fixture::fork(&registry, source);
    let (child, _) = Fixture::fork(&registry, parent_thread);
    registry.charge_cpu(child, 7).unwrap();
    registry.exit_process(child, ExitStatus::Code(0)).unwrap();
    registry.exit_process(parent, ExitStatus::Code(0)).unwrap();
    let selection = registry
        .prepare_wait_child(init, ChildSelector::Process(child), ChildWaitOptions::default())
        .unwrap();
    let PreparedChildWait::Selection(selection) = selection else {
        panic!("missing orphan")
    };
    selection.commit().unwrap();
    assert_eq!(registry.cpu_usage(init).unwrap().children_nanoseconds, 7);
}

#[test]
fn concurrent_usage() {
    let (registry, parent, source) = Fixture::registry();
    let registry = Arc::new(registry);
    let (child, _) = Fixture::fork(&registry, source);
    registry.charge_cpu(child, 13).unwrap();
    registry.exit_process(child, ExitStatus::Code(0)).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let reaped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let waiters = (0..2)
        .map(|_| {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            let reaped = Arc::clone(&reaped);
            thread::spawn(move || {
                barrier.wait();
                let options = ChildWaitOptions {
                    no_hang: true,
                    ..ChildWaitOptions::default()
                };
                if let Ok(PreparedChildWait::Selection(selection)) =
                    registry.prepare_wait_child(parent, ChildSelector::Any, options)
                    && selection.commit().is_ok()
                {
                    reaped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for waiter in waiters {
        waiter.join().unwrap();
    }
    assert_eq!(reaped.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(registry.cpu_usage(parent).unwrap().children_nanoseconds, 13);
}

#[test]
fn prepared_wait_wakes() {
    let (registry, parent, source) = Fixture::registry();
    let registry = Arc::new(registry);
    let (child, _) = Fixture::fork(&registry, source);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let waiter_registry = Arc::clone(&registry);
    let waiter_barrier = Arc::clone(&barrier);
    let waiter = thread::spawn(move || {
        waiter_barrier.wait();
        let prepared = waiter_registry
            .prepare_wait(parent, ChildSelector::Any, ChildWaitOptions::default())
            .unwrap();
        let PreparedChildWait::Selection(selection) = prepared else {
            panic!("missing child exit");
        };
        selection.commit().unwrap()
    });
    barrier.wait();
    thread::sleep(std::time::Duration::from_millis(10));
    registry.exit_process(child, ExitStatus::Code(7)).unwrap();
    let event = waiter.join().unwrap();
    assert_eq!(event.child, child);
    assert_eq!(event.kind, ChildEventKind::Exited(ExitStatus::Code(7)));
}

#[test]
fn wait_epoch_advances() {
    let (registry, _, source) = Fixture::registry();
    let registry = Arc::new(registry);
    let (child, _) = Fixture::fork(&registry, source);
    let observed = registry.wait_observation();
    let waiter_registry = Arc::clone(&registry);
    let waiter = thread::spawn(move || waiter_registry.wait_change(observed));

    registry.exit_process(child, ExitStatus::Code(7)).unwrap();

    waiter.join().unwrap();
    assert_ne!(registry.wait_observation(), observed);
}

#[test]
fn signal_wakes_child_wait() {
    let (registry, _, thread) = Fixture::registry();
    let registry = Arc::new(registry);
    let observed = registry.wait_observation();
    let waiter_registry = Arc::clone(&registry);
    let waiter = thread::spawn(move || waiter_registry.wait_change(observed));

    registry
        .enqueue_signal(
            PendingTarget::Thread(thread),
            SignalInfo::bare(SignalNumber::new(10).unwrap()),
        )
        .unwrap();

    waiter.join().unwrap();
    assert_ne!(registry.wait_observation(), observed);
}
