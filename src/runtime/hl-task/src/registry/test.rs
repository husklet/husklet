use std::sync::{Arc, Mutex};
use std::thread;

use crate::*;

struct Fixture;

#[test]
fn root_uses_container_capability_boundary() {
    let credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
    assert_eq!(credentials.capabilities.effective, 0x0000_0000_a804_25fb);
    assert_eq!(credentials.capabilities.permitted, 0x0000_0000_a804_25fb);
    assert_eq!(credentials.capability_bounding, 0x0000_0000_a804_25fb);
    assert_eq!(credentials.capabilities.inheritable, 0);
    assert_eq!(credentials.capabilities.ambient, 0);
}

impl Fixture {
    fn credentials() -> ProcessCredentials {
        ProcessCredentials::new(1000, 2000, &[30, 20, 30], 8).unwrap()
    }

    fn limits() -> ProcessLimits {
        let mut limits = ProcessLimits::empty();
        limits.set(Resource::OpenFiles, Limit::new(64, 128).unwrap());
        limits.set(Resource::Processes, Limit::new(32, 64).unwrap());
        limits
    }

    fn registry(processes: usize, threads: usize) -> (TaskRegistry, ProcessId, ThreadId) {
        let registry = TaskRegistry::new(RegistryConfig {
            max_processes: processes,
            max_threads: threads,
            max_groups: 8,
            max_pending_signals: 64,
            online_cpus: 1,
        })
        .unwrap();
        let (process, thread) = registry.create_init(Self::credentials(), Self::limits()).unwrap();
        (registry, process, thread)
    }
}

#[test]
fn publication_is_atomic() {
    let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let original = registry.snapshot();
    let reservation = registry
        .begin_create_init(Fixture::credentials(), Fixture::limits())
        .unwrap();

    let reserved = registry.snapshot();
    assert_eq!(reserved.init, None);
    assert!(reserved.processes.is_empty());
    assert!(reserved.threads.is_empty());
    assert!(reserved.sessions.is_empty());
    assert!(reserved.process_groups.is_empty());
    assert_eq!(reserved.user_namespaces, original.user_namespaces);

    let (process, thread) = reservation.commit().unwrap();
    let committed = registry.snapshot();
    assert_eq!(committed.init, Some(process));
    assert_eq!(committed.processes.len(), 1);
    assert_eq!(committed.threads.len(), 1);
    assert_eq!(committed.sessions.len(), 1);
    assert_eq!(committed.process_groups.len(), 1);
    assert_eq!(committed.processes[0].leader, thread);
    assert_eq!(
        committed
            .user_namespaces
            .iter()
            .find(|namespace| namespace.id == NamespaceSet::initial().user)
            .unwrap()
            .owner,
        Fixture::credentials().effective_user
    );
}

#[test]
fn process_number_boundaries_and_publication() {
    let registry = TaskRegistry::new(RegistryConfig {
        max_processes: 2,
        max_threads: 2,
        ..RegistryConfig::default()
    })
    .unwrap();
    assert_eq!(registry.process_by_number(0), None);
    assert_eq!(registry.process_by_number(u32::MAX), None);
    assert_eq!(registry.process_by_number(1), None);

    let reservation = registry
        .begin_create_init(Fixture::credentials(), Fixture::limits())
        .unwrap();
    assert_eq!(registry.process_by_number(1), None);
    let (init, leader) = reservation.commit().unwrap();
    assert_eq!(registry.process_by_number(init.number()), Some(init));

    let fork = registry.begin_fork_process(leader).unwrap();
    let child = fork.process();
    assert_eq!(registry.process_by_number(child.number()), None);
    registry.commit_fork_process(fork).unwrap();
    assert_eq!(registry.process_by_number(child.number()), Some(child));
}

#[test]
fn process_number_tracks_zombie_reap_and_generation() {
    let (registry, parent, leader) = Fixture::registry(2, 2);
    let (child, _) = registry
        .commit_fork_process(registry.begin_fork_process(leader).unwrap())
        .unwrap();
    registry.exit_process(child, ExitStatus::Code(0)).unwrap();
    assert_eq!(registry.process_by_number(child.number()), Some(child));
    registry.reap(parent, child).unwrap();
    assert_eq!(registry.process_by_number(child.number()), None);

    let replacement = registry.begin_fork_process(leader).unwrap();
    let replacement_id = replacement.process();
    assert_eq!(replacement_id.number(), child.number());
    assert_ne!(replacement_id, child);
    registry.commit_fork_process(replacement).unwrap();
    assert_eq!(registry.process_by_number(child.number()), Some(replacement_id));
}

#[test]
fn process_number_concurrently_returns_exact_identity() {
    let (registry, process, _) = Fixture::registry(2, 2);
    let registry = Arc::new(registry);
    let readers: Vec<_> = (0..8)
        .map(|_| {
            let registry = Arc::clone(&registry);
            thread::spawn(move || {
                for _ in 0..1_000 {
                    assert_eq!(registry.process_by_number(process.number()), Some(process));
                }
            })
        })
        .collect();
    for reader in readers {
        reader.join().unwrap();
    }
}

#[test]
fn process_number_remains_visible_during_unpublished_exec() {
    let (registry, process, leader) = Fixture::registry(2, 2);
    let registry = Arc::new(registry);
    let prepared = registry.prepare_exec(process, leader).unwrap();
    assert_eq!(registry.process_by_number(process.number()), Some(process));
    drop(prepared);
    assert_eq!(registry.process_by_number(process.number()), Some(process));
}

#[test]
fn abort_consumes_generation() {
    let registry = TaskRegistry::new(RegistryConfig {
        max_processes: 1,
        max_threads: 1,
        ..RegistryConfig::default()
    })
    .unwrap();
    let before = registry.snapshot();
    drop(
        registry
            .begin_create_init(Fixture::credentials(), Fixture::limits())
            .unwrap(),
    );
    let aborted = registry.snapshot();
    assert_eq!(aborted.init, None);
    assert!(aborted.processes.is_empty());
    assert!(aborted.threads.is_empty());
    assert_eq!(aborted.wait_events, before.wait_events);
    assert_eq!(aborted.child_events, before.child_events);

    let (process, thread) = registry.create_init(Fixture::credentials(), Fixture::limits()).unwrap();
    assert_eq!(process.wire_parts().1, 2);
    assert_eq!(thread.wire_parts().1, 2);
}

#[test]
fn reservation_excludes_competitors() {
    let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let reservation = registry
        .begin_create_init(Fixture::credentials(), Fixture::limits())
        .unwrap();
    assert!(matches!(
        registry.begin_create_init(Fixture::credentials(), Fixture::limits()),
        Err(TaskError::InvalidLifecycle)
    ));
    drop(reservation);
    assert!(
        registry
            .begin_create_init(Fixture::credentials(), Fixture::limits())
            .is_ok()
    );
}

#[test]
fn reservation_blocks_freeze() {
    let registry = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let reservation = registry
        .begin_create_init(Fixture::credentials(), Fixture::limits())
        .unwrap();
    let (frozen_tx, frozen_rx) = std::sync::mpsc::channel();
    let freezer = {
        let registry = Arc::clone(&registry);
        thread::spawn(move || {
            registry.freeze_checkpoint();
            frozen_tx.send(registry.checkpoint_snapshot()).unwrap();
            registry.thaw_checkpoint();
        })
    };
    registry.activity.wait_until_freeze_waits();
    assert_eq!(frozen_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    let (process, _) = reservation.commit().unwrap();
    let image = frozen_rx.recv().unwrap().unwrap();
    assert_eq!(image.init, Some(process));
    assert_eq!(image.processes.len(), 1);
    freezer.join().unwrap();
}

#[test]
fn freeze_blocks_reservation() {
    let registry = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    registry.freeze_checkpoint();
    let image = registry.checkpoint_snapshot().unwrap();
    assert_eq!(image.init, None);
    let (reservation_tx, reservation_rx) = std::sync::mpsc::channel();
    let beginner = {
        let registry = Arc::clone(&registry);
        thread::spawn(move || {
            let reservation = registry
                .begin_create_init(Fixture::credentials(), Fixture::limits())
                .unwrap();
            reservation_tx.send(()).unwrap();
            reservation.commit().unwrap()
        })
    };
    registry.activity.wait_until_admit_waits();
    assert_eq!(reservation_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    assert_eq!(registry.checkpoint_snapshot().unwrap(), image);
    registry.thaw_checkpoint();
    reservation_rx.recv().unwrap();
    let (process, _) = beginner.join().unwrap();
    assert_eq!(registry.snapshot().init, Some(process));
}

#[test]
fn commit_once() {
    let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let reservation = registry
        .begin_create_init(Fixture::credentials(), Fixture::limits())
        .unwrap();
    let slots = reservation.slots;
    let committed = reservation.commit().unwrap();
    assert_eq!(
        registry.commit_init(slots, Fixture::credentials(), Fixture::limits()),
        Err(TaskError::InvalidPlan)
    );
    assert_eq!(registry.snapshot().init, Some(committed.0));
    assert_eq!(registry.snapshot().processes.len(), 1);
}

#[test]
fn failures_publish_nothing() {
    for stage in 0..4 {
        let registry = TaskRegistry::new(RegistryConfig {
            max_processes: 1,
            max_threads: 1,
            ..RegistryConfig::default()
        })
        .unwrap();
        {
            let mut state = registry.lock();
            match stage {
                0 => state.processes[0].generation = u16::MAX,
                1 => state.threads[0].generation = u16::MAX,
                2 => state.sessions[0].generation = u16::MAX,
                3 => state.process_groups[0].generation = u16::MAX,
                _ => unreachable!(),
            }
        }
        assert!(
            registry
                .begin_create_init(Fixture::credentials(), Fixture::limits())
                .is_err()
        );
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.init, None);
        assert!(snapshot.processes.is_empty());
        assert!(snapshot.threads.is_empty());
        assert!(snapshot.sessions.is_empty());
        assert!(snapshot.process_groups.is_empty());
    }

    let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let reservation = registry
        .begin_create_init(Fixture::credentials(), Fixture::limits())
        .unwrap();
    let slots = reservation.slots;
    registry.lock().threads[slots.thread.parts().unwrap().0].generation += 1;
    assert_eq!(reservation.commit(), Err(TaskError::InvalidPlan));
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.init, None);
    assert!(snapshot.processes.is_empty());
    assert!(snapshot.threads.is_empty());
}

#[test]
fn signal_thread_target_is_generation_qualified() {
    let (registry, process, thread) = Fixture::registry(4, 4);
    let target = registry
        .signal_thread_target(process, thread.number())
        .unwrap()
        .unwrap();
    assert_eq!(target.thread, thread);
    assert_eq!(target.process, process);
    assert_eq!(target.sender_credentials, Fixture::credentials());
    assert_eq!(target.target_credentials, Fixture::credentials());
    assert_eq!(registry.signal_thread_target(process, 0).unwrap(), None);
    assert_eq!(registry.signal_thread_target(process, 4).unwrap(), None);
}

#[test]
fn process_snapshot_is_exact_and_generation_qualified() {
    let (registry, process, _) = Fixture::registry(65_536, 65_536);
    let expected = registry
        .snapshot()
        .processes
        .into_iter()
        .find(|entry| entry.id == process)
        .unwrap();
    assert_eq!(registry.process_snapshot(process).unwrap(), expected);

    let (slot, generation) = process.wire_parts();
    let stale = ProcessId::from_wire(slot, generation.checked_add(1).unwrap()).unwrap();
    assert_eq!(registry.process_snapshot(stale), Err(TaskError::InvalidProcess));
}

#[test]
fn process_observation_is_exact_and_generation_qualified() {
    let (registry, process, _) = Fixture::registry(65_536, 65_536);
    let full = registry.process_snapshot(process).unwrap();
    let observation = registry.process_observation(process).unwrap();
    assert_eq!(observation.parent, full.parent);
    assert_eq!(observation.credentials, full.credentials);
    assert_eq!(observation.parent_death_signal, full.parent_death_signal);
    assert_eq!(observation.child_subreaper, full.child_subreaper);
    assert_eq!(observation.dumpable, full.dumpable);
    assert_eq!(observation.timer_slack, full.timer_slack);
    assert_eq!(observation.thp_disabled, full.thp_disabled);
    assert_eq!(observation.mce_policy, full.mce_policy);

    let (slot, generation) = process.wire_parts();
    let stale = ProcessId::from_wire(slot, generation.checked_add(1).unwrap()).unwrap();
    assert_eq!(registry.process_observation(stale), Err(TaskError::InvalidProcess));
}

#[test]
fn parent_death_reparents() {
    let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (_, leader) = registry.create_init(Fixture::credentials(), Fixture::limits()).unwrap();
    let middle = registry
        .commit_fork_process(registry.begin_fork_process(leader).unwrap())
        .unwrap();
    registry.set_subreaper(registry.snapshot().init.unwrap(), true).unwrap();
    let grandchild = registry
        .commit_fork_process(registry.begin_fork_process(middle.1).unwrap())
        .unwrap();
    registry.set_pdeath(grandchild.0, 10).unwrap();
    registry.exit_process(middle.0, ExitStatus::Code(0)).unwrap();
    let child = registry
        .snapshot()
        .processes
        .into_iter()
        .find(|process| process.id == grandchild.0)
        .unwrap();
    assert_eq!(child.parent, registry.snapshot().init);
    assert_eq!(child.signals.pending[0].signal.get(), 10);
}

#[test]
fn prctl_state_lifecycle() {
    let registry = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let (process, thread) = registry.create_init(Fixture::credentials(), Fixture::limits()).unwrap();
    registry.set_dumpable(process, false).unwrap();
    registry.set_timer_slack(process, 123_456).unwrap();
    registry.set_thp(process, true).unwrap();
    registry.set_mce_policy(process, 1).unwrap();
    registry.set_subreaper(process, true).unwrap();
    registry.set_pdeath(process, 12).unwrap();
    assert_eq!(registry.set_personality(process, 0x40000), Ok(0));
    registry.set_name(thread, *b"parent-name\0\0\0\0\0").unwrap();
    let child = registry
        .commit_fork_process(registry.begin_fork_process(thread).unwrap())
        .unwrap();
    let snapshot = registry.snapshot();
    let child_process = snapshot.processes.iter().find(|value| value.id == child.0).unwrap();
    let child_thread = snapshot.threads.iter().find(|value| value.id == child.1).unwrap();
    assert!(!child_process.dumpable);
    assert_eq!(child_process.timer_slack, 123_456);
    assert!(child_process.thp_disabled);
    assert_eq!(child_process.mce_policy, 1);
    assert_eq!(child_process.personality, 0x40000);
    assert!(!child_process.child_subreaper);
    assert_eq!(child_process.parent_death_signal, 0);
    assert_eq!(child_thread.name, *b"parent-name\0\0\0\0\0");
    let mut credentials = snapshot
        .processes
        .iter()
        .find(|value| value.id == process)
        .unwrap()
        .credentials
        .clone();
    credentials.keep_capabilities = true;
    registry.replace_credentials(process, credentials).unwrap();
    let mut exec = registry
        .prepare_named(process, thread, *b"executed\0\0\0\0\0\0\0\0")
        .unwrap();
    exec.publish().unwrap();
    let snapshot = registry.snapshot();
    let parent = snapshot.processes.iter().find(|value| value.id == process).unwrap();
    let leader = snapshot.threads.iter().find(|value| value.id == thread).unwrap();
    assert!(parent.dumpable);
    assert_eq!(parent.timer_slack, 123_456);
    assert!(parent.thp_disabled && parent.child_subreaper);
    assert_eq!(parent.mce_policy, 1);
    assert_eq!(parent.personality, 0x40000);
    assert_eq!(parent.parent_death_signal, 12);
    assert!(!parent.credentials.keep_capabilities);
    assert_eq!(parent.credentials.setid_authority(), SetIdAuthority::None);
    assert_eq!(leader.name, *b"executed\0\0\0\0\0\0\0\0");
    exec.finish();
}

#[test]
fn init_snapshot_is() {
    let (registry, process, thread) = Fixture::registry(4, 8);
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.init, Some(process));
    assert_eq!(snapshot.processes.len(), 1);
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.processes[0].threads, vec![thread]);
    assert_eq!(snapshot.processes[0].leader, thread);
    assert_eq!(snapshot.processes[0].credentials.supplementary_groups(), &[30, 20, 30]);
    assert_eq!(
        snapshot.processes[0]
            .limits
            .iter()
            .map(|entry| entry.0)
            .collect::<Vec<_>>(),
        vec![Resource::Processes, Resource::OpenFiles]
    );
}

#[test]
fn default_limits() {
    let limits = ProcessLimits::default();
    for resource in [
        Resource::CpuTime,
        Resource::FileSize,
        Resource::Data,
        Resource::ResidentSet,
        Resource::Processes,
        Resource::LockedMemory,
        Resource::AddressSpace,
        Resource::Locks,
        Resource::PendingSignals,
        Resource::MessageQueue,
        Resource::Nice,
        Resource::RealtimePriority,
        Resource::RealtimeTime,
    ] {
        assert_eq!(
            limits.get(resource),
            Some(Limit {
                soft: u64::MAX,
                hard: u64::MAX
            }),
        );
    }
    assert_eq!(
        limits.get(Resource::Stack),
        Some(Limit {
            soft: 8 << 20,
            hard: u64::MAX,
        })
    );
    assert_eq!(
        limits.get(Resource::Core),
        Some(Limit {
            soft: 0,
            hard: u64::MAX,
        })
    );
    assert_eq!(
        limits.get(Resource::OpenFiles),
        Some(Limit {
            soft: 20_480,
            hard: 1_048_576,
        })
    );
}

#[test]
fn robust_list_is() {
    let (registry, process, source) = Fixture::registry(4, 6);
    let registration = RobustListRegistration::new(0x8000);
    registry.set_robust_list(source, registration).unwrap();
    assert_eq!(registry.robust_list(source).unwrap(), Some(registration));

    let clone = registry.begin_clone_thread(source).unwrap();
    let clone_id = clone.thread();
    registry.commit_clone_thread(clone).unwrap();
    assert_eq!(registry.robust_list(clone_id).unwrap(), None);

    let fork = registry.begin_fork_process(source).unwrap();
    let child_thread = fork.thread();
    registry.commit_fork_process(fork).unwrap();
    assert_eq!(registry.robust_list(child_thread).unwrap(), None);

    registry.mark_exec(process).unwrap();
    assert_eq!(registry.robust_list(source).unwrap(), None);
}

#[test]
fn worker_fork_leader_uses_process_number() {
    let (registry, parent, leader) = Fixture::registry(4, 6);
    let worker = registry
        .commit_clone_thread(registry.begin_clone_thread(leader).unwrap())
        .unwrap();
    assert_ne!(worker.number(), parent.number());

    let (child, child_leader) = registry
        .commit_fork_process(registry.begin_fork_process(worker).unwrap())
        .unwrap();

    assert_eq!(child_leader.number(), child.number());
    assert_ne!(child_leader.number(), worker.number());
}

#[test]
fn exited_thread_number_is_not_immediately_reused() {
    let (registry, _, leader) = Fixture::registry(4, 5);
    let mut numbers = Vec::new();
    for _ in 0..4 {
        let thread = registry
            .commit_clone_thread(registry.begin_clone_thread(leader).unwrap())
            .unwrap();
        numbers.push(thread.number());
        registry.exit_thread(thread, ExitStatus::Code(0)).unwrap();
    }
    numbers.sort_unstable();
    numbers.dedup();
    assert_eq!(numbers.len(), 4);
    assert!(!numbers.contains(&leader.number()));
}

#[test]
fn robust_exit_take() {
    let (registry, _, thread) = Fixture::registry(2, 2);
    let registration = RobustListRegistration::new(0x9000);
    registry.set_robust_list(thread, registration).unwrap();
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.threads[0].robust_list, Some(registration));
    let restored = TaskRegistry::restore(&snapshot).unwrap();
    assert_eq!(restored.robust_list(thread).unwrap(), Some(registration));
    assert_eq!(registry.take_robust_exit(thread).unwrap(), Some(registration));
    assert_eq!(registry.take_robust_exit(thread).unwrap(), None);
}

#[test]
fn clear_tid_rules() {
    let (registry, _, source) = Fixture::registry(4, 6);
    registry.set_clear_tid(source, 0x8000).unwrap();
    assert_eq!(registry.clear_tid(source).unwrap(), Some(0x8000));

    let clone = registry.begin_clone_thread(source).unwrap();
    let clone_id = clone.thread();
    registry.commit_clone_thread(clone).unwrap();
    assert_eq!(registry.clear_tid(clone_id).unwrap(), None);

    let staged = registry.begin_fork_process(source).unwrap();
    let staged_thread = staged.thread();
    registry.stage_fork_clear(&staged, 0x9000).unwrap();
    assert_eq!(registry.clear_tid(staged_thread).unwrap(), Some(0x9000));
    registry.rollback_fork_process(staged).unwrap();
    assert_eq!(registry.clear_tid(staged_thread), Err(TaskError::InvalidThread));

    let fork = registry.begin_fork_process(source).unwrap();
    let child_thread = fork.thread();
    registry.commit_fork_process(fork).unwrap();
    assert_eq!(registry.clear_tid(child_thread).unwrap(), None);

    assert_eq!(registry.take_clear_tid(source).unwrap(), Some(0x8000));
    assert_eq!(registry.take_clear_tid(source).unwrap(), None);
}

#[test]
fn namespace_identity_rules() {
    let (registry, process, source) = Fixture::registry(4, 6);
    let initial = registry.namespaces(process).unwrap();
    let identifier = registry.unshare_namespace(process, NamespaceKind::Uts).unwrap();
    assert_ne!(identifier, initial.uts);
    assert_eq!(registry.namespaces(process).unwrap().uts, identifier);

    let clone = registry.begin_clone_thread(source).unwrap();
    registry.commit_clone_thread(clone).unwrap();
    assert_eq!(
        registry.unshare_namespace(process, NamespaceKind::User),
        Err(TaskError::InvalidLifecycle),
    );

    let fork = registry.begin_fork_process(source).unwrap();
    let child = fork.process();
    registry.commit_fork_process(fork).unwrap();
    assert_eq!(
        registry.namespaces(child).unwrap(),
        registry.namespaces(process).unwrap()
    );

    let joined = registry.unshare_namespace(child, NamespaceKind::Uts).unwrap();
    assert_eq!(registry.namespaces(child).unwrap().uts, joined);
    assert_ne!(registry.namespaces(process).unwrap().uts, joined);
    registry.join_namespace(process, joined).unwrap();
    assert_eq!(registry.namespaces(process).unwrap().uts, joined);

    let snapshot = registry.snapshot();
    let restored = TaskRegistry::restore(&snapshot).unwrap();
    assert_eq!(restored.namespaces(child).unwrap().uts, joined);
}

#[test]
fn user_namespace_maps() {
    let (registry, process, source) = Fixture::registry(4, 6);
    let before = registry.namespaces(process).unwrap().user;
    let identifier = registry.unshare_user(process).unwrap();
    assert_ne!(identifier, before);
    let credentials = registry
        .snapshot()
        .processes
        .into_iter()
        .find(|entry| entry.id == process)
        .unwrap()
        .credentials;
    assert_eq!(credentials.capabilities.effective, CapabilitySets::SUPPORTED);

    let user_map = "0 1000 1\n".parse().unwrap();
    registry.write_user_map(process, process, user_map).unwrap();
    assert_eq!(
        registry.write_user_map(process, process, "0 1000 1\n".parse().unwrap()),
        Err(crate::MapError::Written),
    );
    assert_eq!(
        registry.write_group_map(process, process, "0 2000 1\n".parse().unwrap()),
        Err(crate::MapError::Permission),
    );
    registry.deny_setgroups(process, process).unwrap();
    registry
        .write_group_map(process, process, "0 2000 1\n".parse().unwrap())
        .unwrap();

    let fork = registry.begin_fork_process(source).unwrap();
    let child = fork.process();
    registry.commit_fork_process(fork).unwrap();
    assert_eq!(
        registry.user_namespace(child).unwrap(),
        registry.user_namespace(process).unwrap()
    );

    let snapshot = registry.snapshot();
    let restored = TaskRegistry::restore(&snapshot).unwrap();
    assert_eq!(
        restored.user_namespace(process).unwrap(),
        registry.user_namespace(process).unwrap()
    );
}

#[test]
fn uts_owner_authorizes() {
    let (registry, process, _) = Fixture::registry(4, 6);
    let mut credentials = registry.credentials(process).unwrap();
    credentials.capabilities.effective |= CapabilitySets::SYS_ADMIN;
    registry.replace_credentials(process, credentials).unwrap();
    assert!(registry.may_administer_uts(process).unwrap());
    registry.unshare_user(process).unwrap();
    assert!(!registry.may_administer_uts(process).unwrap());
    registry.unshare_namespace(process, NamespaceKind::Uts).unwrap();
    assert!(registry.may_administer_uts(process).unwrap());
}

#[test]
fn initial_root_administers_owned_uts_without_advertised_sys_admin() {
    let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
    assert!(!credentials.has_capability(CapabilitySets::SYS_ADMIN));
    let (process, _) = registry.create_init(credentials, Fixture::limits()).unwrap();
    assert!(registry.may_administer_uts(process).unwrap());
}

#[test]
fn launch_identity_owns_initial_namespaces() {
    let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let credentials = ProcessCredentials::new(501, 20, &[], 8).unwrap();
    let (process, _) = registry.create_init(credentials, Fixture::limits()).unwrap();
    assert!(registry.may_administer_uts(process).unwrap());
    assert_eq!(registry.user_namespace(process).unwrap().owner, 501);
}

#[test]
fn uts_ancestor_authorizes() {
    let (registry, parent, source) = Fixture::registry(4, 6);
    let mut credentials = registry.credentials(parent).unwrap();
    credentials.capabilities.effective |= CapabilitySets::SYS_ADMIN;
    registry.replace_credentials(parent, credentials).unwrap();
    let fork = registry.begin_fork_process(source).unwrap();
    let child = fork.process();
    registry.commit_fork_process(fork).unwrap();
    registry.unshare_user(child).unwrap();
    let uts = registry.unshare_namespace(child, NamespaceKind::Uts).unwrap();
    registry.join_namespace(parent, uts).unwrap();
    assert!(registry.may_administer_uts(parent).unwrap());
}

#[test]
fn sibling_denied() {
    let (registry, parent, source) = Fixture::registry(5, 6);
    let fork = registry.begin_fork_process(source).unwrap();
    let child = fork.process();
    registry.commit_fork_process(fork).unwrap();
    let namespace = registry.unshare_user(child).unwrap();

    let map = "0 1000 1\n".parse().unwrap();
    assert_eq!(
        registry.write_user_map(parent, child, map),
        Err(crate::MapError::Permission),
    );
    assert_eq!(
        registry.join_namespace(parent, namespace),
        Err(TaskError::InvalidLifecycle),
    );
    assert_eq!(registry.unshare_user(child), Err(TaskError::InvalidLifecycle));
}

#[test]
fn scoped_caps_denied() {
    let (registry, _, source) = Fixture::registry(4, 5);
    let fork = registry.begin_fork_process(source).unwrap();
    let child = fork.process();
    registry.commit_fork_process(fork).unwrap();
    registry.unshare_user(child).unwrap();
    let credentials = registry
        .snapshot()
        .processes
        .into_iter()
        .find(|process| process.id == child)
        .unwrap()
        .credentials;
    assert!(credentials.has_capability(CapabilitySets::SET_USER));
    assert_eq!(
        registry.write_user_map(child, child, "0 77 1\n".parse().unwrap()),
        Err(crate::MapError::Permission),
    );
}

#[test]
fn authority_consumed() {
    let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, _) = registry
        .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), Fixture::limits())
        .unwrap();
    registry.unshare_user(process).unwrap();
    registry
        .write_user_map(process, process, "0 55 3\n".parse().unwrap())
        .unwrap();
    assert_eq!(
        registry.write_user_map(process, process, "0 0 1\n".parse().unwrap()),
        Err(crate::MapError::Written),
    );
    registry
        .write_group_map(process, process, "0 77 2\n".parse().unwrap())
        .unwrap();
    assert_eq!(
        registry.deny_setgroups(process, process),
        Err(crate::MapError::Permission),
    );
}

#[test]
fn setgroups_ordering() {
    let (registry, process, _) = Fixture::registry(3, 3);
    registry.unshare_user(process).unwrap();
    let map: crate::IdMap = "0 2000 1\n".parse().unwrap();
    assert_eq!(
        registry.write_group_map(process, process, map.clone()),
        Err(crate::MapError::Permission),
    );
    registry.deny_setgroups(process, process).unwrap();
    assert_eq!(
        registry.deny_setgroups(process, process),
        Err(crate::MapError::Permission),
    );
    registry.write_group_map(process, process, map).unwrap();
    assert_eq!(
        registry.deny_setgroups(process, process),
        Err(crate::MapError::Permission),
    );
}

#[test]
fn user_map_validation() {
    assert!("".parse::<crate::IdMap>().is_err());
    assert!("0 1 0\n".parse::<crate::IdMap>().is_err());
    assert!("0 1 2\n1 4 1\n".parse::<crate::IdMap>().is_err());
    assert!("0 1 2\n4 2 1\n".parse::<crate::IdMap>().is_err());
    assert!("4294967295 0 2\n".parse::<crate::IdMap>().is_err());
    assert_eq!(
        "0 1000 1\n4 2000 2\n".parse::<crate::IdMap>().unwrap().ranges().len(),
        2,
    );
}

#[test]
fn process_credentials_and() {
    let (registry, process, source) = Fixture::registry(3, 3);
    let replacement = ProcessCredentials::new(7, 8, &[9], 8).unwrap();
    registry.replace_credentials(process, replacement.clone()).unwrap();
    registry
        .set_limit(process, Resource::OpenFiles, Limit::new(10, 20).unwrap())
        .unwrap();
    let fork = registry.begin_fork_process(source).unwrap();
    let child = fork.process();
    registry.commit_fork_process(fork).unwrap();
    registry.mark_exec(child).unwrap();
    let snapshot = registry.snapshot();
    let parent = snapshot.processes.iter().find(|entry| entry.id == process).unwrap();
    let child = snapshot.processes.iter().find(|entry| entry.id == child).unwrap();
    assert_eq!(parent.credentials, replacement);
    assert_eq!(child.credentials, parent.credentials);
    assert_eq!(child.limits, parent.limits);
}

#[test]
fn clone_plan_is() {
    let (registry, _, source) = Fixture::registry(2, 3);
    let rolled_back = registry.begin_clone_thread(source).unwrap();
    let stale = rolled_back.thread();
    assert_eq!(registry.snapshot().threads[1].lifecycle, ThreadLifecycle::Starting);
    registry.rollback_clone_thread(rolled_back).unwrap();
    let committed = registry.begin_clone_thread(source).unwrap();
    let current = committed.thread();
    assert_ne!(stale, current);
    assert_ne!(stale.number(), current.number());
    registry.commit_clone_thread(committed).unwrap();
    assert_eq!(registry.request_cancellation(stale), Err(TaskError::InvalidThread));
    let snapshot = registry.snapshot();
    assert!(!snapshot.threads.iter().any(|thread| thread.id == stale));
    assert_eq!(
        snapshot
            .threads
            .iter()
            .find(|thread| thread.id == current)
            .unwrap()
            .lifecycle,
        ThreadLifecycle::Runnable,
    );
}

#[test]
fn fork_inherits_values() {
    let (registry, parent, source) = Fixture::registry(3, 4);
    let plan = registry.begin_fork_process(source).unwrap();
    let child = plan.process();
    assert!(registry.snapshot().processes[0].children.is_empty());
    registry.commit_fork_process(plan).unwrap();
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.processes[0].children, vec![child]);
    assert_eq!(snapshot.processes[1].parent, Some(parent));
    assert_eq!(snapshot.processes[0].credentials, snapshot.processes[1].credentials);
    assert_eq!(snapshot.processes[0].limits, snapshot.processes[1].limits);
}

#[test]
fn fork_rollback_reuses() {
    let (registry, _, source) = Fixture::registry(2, 3);
    let first = registry.begin_fork_process(source).unwrap();
    let stale_process = first.process();
    let stale_thread = first.thread();
    registry.rollback_fork_process(first).unwrap();
    let second = registry.begin_fork_process(source).unwrap();
    assert_eq!(stale_process.number(), second.process().number());
    assert_ne!(stale_process, second.process());
    assert_eq!(stale_thread.number(), second.thread().number());
    assert_ne!(stale_thread, second.thread());
}

#[test]
fn reaped_pid_is_not_immediately_reused() {
    let (registry, parent, source) = Fixture::registry(3, 3);
    let first = registry
        .commit_fork_process(registry.begin_fork_process(source).unwrap())
        .unwrap();
    registry.exit_process(first.0, ExitStatus::Code(0)).unwrap();
    registry.reap(parent, first.0).unwrap();

    let second = registry.begin_fork_process(source).unwrap();
    assert_ne!(first.0.number(), second.process().number());
}

#[test]
fn zombie_wait_status() {
    let (registry, parent, source) = Fixture::registry(2, 2);
    let plan = registry.begin_fork_process(source).unwrap();
    let child = plan.process();
    let child_thread = plan.thread();
    registry.commit_fork_process(plan).unwrap();
    let name = *b"zombie-name\0\0\0\0\0";
    registry.set_name(child_thread, name).unwrap();
    let status = ExitStatus::Signal {
        signal: 11,
        dumped_core: true,
    };
    registry.exit_process(child, status).unwrap();
    assert_eq!(
        registry
            .snapshot()
            .processes
            .into_iter()
            .find(|process| process.id == child)
            .unwrap()
            .name,
        name,
    );
    let event = registry.wait(parent, WaitSelector::Process(child)).unwrap().unwrap();
    assert_eq!(event.status.wait_status(), 0x8b);
    assert_eq!(registry.wait(parent, WaitSelector::Any).unwrap(), Some(event));
    assert_eq!(registry.reap(parent, child), Ok(status));
    assert_eq!(registry.wait(parent, WaitSelector::Any), Err(TaskError::NoChildren));
    assert_eq!(registry.reap(parent, child), Err(TaskError::InvalidProcess));
}

#[test]
fn exiting_parent_reparents() {
    let (registry, init, init_thread) = Fixture::registry(4, 4);
    let parent_plan = registry.begin_fork_process(init_thread).unwrap();
    let parent = parent_plan.process();
    let parent_thread = parent_plan.thread();
    registry.commit_fork_process(parent_plan).unwrap();
    let child_plan = registry.begin_fork_process(parent_thread).unwrap();
    let child = child_plan.process();
    registry.commit_fork_process(child_plan).unwrap();
    registry.exit_process(child, ExitStatus::Code(7)).unwrap();
    registry.exit_process(parent, ExitStatus::Code(3)).unwrap();
    let snapshot = registry.snapshot();
    let child_snapshot = snapshot.processes.iter().find(|process| process.id == child).unwrap();
    assert_eq!(child_snapshot.parent, Some(init));
    assert_eq!(
        registry
            .wait(init, WaitSelector::Process(child))
            .unwrap()
            .unwrap()
            .status,
        ExitStatus::Code(7)
    );
}

#[derive(Default)]
struct HookRecorder {
    cancellations: Mutex<Vec<ThreadId>>,
    pending: Mutex<Vec<(ThreadId, bool)>>,
}

impl CancellationSink for HookRecorder {
    type Error = ();

    fn request_cancellation(&self, thread: ThreadId) -> Result<(), Self::Error> {
        self.cancellations.lock().unwrap().push(thread);
        Ok(())
    }
}

impl SignalPendingSink for HookRecorder {
    type Error = ();

    fn pending_changed(&self, thread: ThreadId, pending: bool) -> Result<(), Self::Error> {
        self.pending.lock().unwrap().push((thread, pending));
        Ok(())
    }
}

#[test]
fn cancellation_and_signal() {
    let (registry, _, thread) = Fixture::registry(2, 2);
    let hooks = HookRecorder::default();
    registry.request_cancellation(thread).unwrap().deliver(&hooks).unwrap();
    registry
        .set_signal_pending(thread, true)
        .unwrap()
        .deliver(&hooks)
        .unwrap();
    assert_eq!(*hooks.cancellations.lock().unwrap(), vec![thread]);
    assert_eq!(*hooks.pending.lock().unwrap(), vec![(thread, true)]);
    let snapshot = registry.snapshot();
    assert!(snapshot.threads[0].cancellation_pending);
    assert!(snapshot.threads[0].signal_pending);
}

#[test]
fn capacities_fail_without() {
    let (registry, _, source) = Fixture::registry(1, 2);
    assert!(matches!(
        registry.begin_fork_process(source),
        Err(TaskError::ProcessLimit)
    ));
    let clone = registry.begin_clone_thread(source).unwrap();
    assert!(matches!(
        registry.begin_clone_thread(source),
        Err(TaskError::ThreadLimit)
    ));
    registry.rollback_clone_thread(clone).unwrap();
    assert_eq!(registry.snapshot().threads.len(), 1);

    let excessive_groups = ProcessCredentials::new(1, 1, &[1, 2, 3, 4, 5, 6, 7, 8, 9], 16).unwrap();
    assert_eq!(
        registry.replace_credentials(registry.snapshot().init.unwrap(), excessive_groups),
        Err(TaskError::GroupLimit)
    );
}

#[test]
fn concurrent_clone_and() {
    let (registry, _, source) = Fixture::registry(2, 129);
    let registry = Arc::new(registry);
    let workers: Vec<_> = (0..64)
        .map(|_| {
            let registry = Arc::clone(&registry);
            thread::spawn(move || {
                let plan = registry.begin_clone_thread(source).unwrap();
                let thread = registry.commit_clone_thread(plan).unwrap();
                registry.set_thread_blocked(thread, true).unwrap();
                registry.set_thread_blocked(thread, false).unwrap();
                registry.exit_thread(thread, ExitStatus::Code(0)).unwrap();
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let snapshot = registry.snapshot();
    assert_eq!(snapshot.processes.len(), 1);
    assert_eq!(snapshot.threads.len(), 1);
    assert_eq!(snapshot.processes[0].threads, vec![source]);
}

#[test]
fn deterministic_fork_exit() {
    let (registry, init, source) = Fixture::registry(2, 2);
    let mut stale = Vec::new();
    let mut previous_sequence = 0;
    for step in 0..2_000 {
        let plan = registry.begin_fork_process(source).unwrap();
        let child = plan.process();
        registry.commit_fork_process(plan).unwrap();
        registry
            .exit_process(child, ExitStatus::Code((step & 0xff) as u8))
            .unwrap();
        let event = registry.wait(init, WaitSelector::Process(child)).unwrap().unwrap();
        assert!(event.sequence > previous_sequence);
        previous_sequence = event.sequence;
        registry.reap(init, child).unwrap();
        stale.push(child);
        assert_eq!(
            registry.wait(init, WaitSelector::Process(child)),
            Err(TaskError::NoChildren)
        );
    }
    assert!(stale.windows(2).all(|pair| pair[0] != pair[1]));
}
