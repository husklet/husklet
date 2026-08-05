use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
mod model;
mod validate;

#[cfg(test)]
mod test;

pub use model::{
    ProcessCheckpointReference, TaskExternalCheckpoint, TaskExternalRestore, TaskRegistryImage, TaskResourceKey,
    ThreadCheckpointReference,
};

use super::{Process, ProcessGroup, Session, Slot, State, TaskRegistry, Thread};
use crate::signal::{PendingSignals, SIGNAL_COUNT, SignalProcessState, SignalThreadState};
use crate::{
    ProcessGroupId, ProcessId, ProcessLifecycle, ProcessLimits, RegistrySnapshot, SessionId, SignalAction,
    SignalThreadSnapshot, TaskError, ThreadId, ThreadLifecycle,
};
pub const TASK_CHECKPOINT_VERSION: u32 = 17;
impl TaskRegistry {
    pub fn freeze_checkpoint(&self) {
        self.activity.freeze();
        drop(self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    }

    pub fn thaw_checkpoint(&self) {
        self.activity.thaw();
    }

    pub fn image(&self, external: &dyn TaskExternalCheckpoint) -> Result<TaskRegistryImage, TaskError> {
        let registry = self.checkpoint_snapshot()?;
        let processes = registry
            .processes
            .iter()
            .map(|process| external.snapshot_process(process.id))
            .collect::<Result<Vec<_>, _>>()?;
        let threads = registry
            .threads
            .iter()
            .map(|thread| external.snapshot_thread(thread.id))
            .collect::<Result<Vec<_>, _>>()?;
        let image = TaskRegistryImage {
            version: TASK_CHECKPOINT_VERSION,
            registry,
            processes,
            threads,
        };
        image.validate()?;
        Ok(image)
    }

    pub fn restore(snapshot: &RegistrySnapshot) -> Result<Self, TaskError> {
        Self::validate_snapshot(snapshot)?;
        let mut processes = Self::restore_slots(&snapshot.process_generations);
        let mut threads = Self::restore_slots(&snapshot.thread_generations);
        let mut sessions = Self::restore_slots(&snapshot.session_generations);
        let mut process_groups = Self::restore_slots(&snapshot.process_group_generations);
        for saved in &snapshot.processes {
            let (slot, _) = saved.id.parts().ok_or(TaskError::InvalidSnapshot)?;
            let mut actions = [SignalAction::DEFAULT; SIGNAL_COUNT];
            for (signal, action) in &saved.signals.actions {
                actions[signal.get() as usize - 1] = *action;
            }
            processes[slot].value = Some(Process {
                control_epoch: 0,
                lifecycle: saved.lifecycle,
                parent: saved.parent,
                children: saved.children.iter().copied().collect(),
                threads: saved.threads.iter().copied().collect(),
                leader: saved.leader,
                session: saved.session,
                process_group: saved.process_group,
                terminal_detached: saved.terminal_detached,
                child_class: saved.child_class,
                execed: saved.execed,
                arguments: saved.arguments.clone(),
                name: saved.name,
                credentials: saved.credentials.clone(),
                limits: ProcessLimits::from_entries(&saved.limits)?,
                exit_status: saved.exit_status,
                pending_transaction: None,
                signals: SignalProcessState {
                    actions,
                    pending: PendingSignals::restore(&saved.signals.pending, snapshot.config.max_pending_signals)
                        .map_err(|_| TaskError::InvalidSnapshot)?,
                },
                namespaces: saved.namespaces,
                parent_death_signal: saved.parent_death_signal,
                child_subreaper: saved.child_subreaper,
                cpu_usage: saved.cpu_usage,
                dumpable: saved.dumpable,
                oom_score_adj: saved.oom_score_adj,
                timer_slack: saved.timer_slack,
                thp_disabled: saved.thp_disabled,
                mce_policy: saved.mce_policy,
                personality: saved.personality,
            });
        }
        for saved in &snapshot.threads {
            let (slot, _) = saved.id.parts().ok_or(TaskError::InvalidSnapshot)?;
            threads[slot].value = Some(Thread {
                process: saved.process,
                lifecycle: saved.lifecycle,
                cancellation_pending: saved.cancellation_pending,
                signal_pending: saved.signal_pending,
                pending_transaction: None,
                signals: SignalThreadState {
                    mask: saved.signals.mask,
                    alternate_stack: saved.signals.alternate_stack,
                    pending: PendingSignals::restore(&saved.signals.pending, snapshot.config.max_pending_signals)
                        .map_err(|_| TaskError::InvalidSnapshot)?,
                    deferred: saved.signals.deferred,
                    frames: saved.signals.frames.clone(),
                },
                robust_list: saved.robust_list,
                clear_tid: saved.clear_tid,
                name: saved.name,
                affinity: saved.affinity,
                schedule: saved.schedule,
            });
        }
        for saved in &snapshot.sessions {
            let (slot, _) = saved.id.parts().ok_or(TaskError::InvalidSnapshot)?;
            sessions[slot].value = Some(Session {
                leader: saved.leader,
                process_groups: saved.process_groups.iter().copied().collect(),
                foreground_group: saved.foreground_group,
            });
        }
        for saved in &snapshot.process_groups {
            let (slot, _) = saved.id.parts().ok_or(TaskError::InvalidSnapshot)?;
            process_groups[slot].value = Some(ProcessGroup {
                session: saved.session,
                leader: saved.leader,
                members: saved.members.iter().copied().collect(),
                orphaned: saved.orphaned,
            });
        }
        Ok(Self {
            max_groups: snapshot.config.max_groups,
            max_pending_signals: snapshot.config.max_pending_signals,
            state: Mutex::new(State {
                processes,
                threads,
                sessions,
                process_groups,
                init: snapshot.init,
                waits: VecDeque::from(snapshot.wait_events.clone()),
                wait_reservations: BTreeSet::new(),
                child_events: VecDeque::from(snapshot.child_events.clone()),
                next_transaction: snapshot.next_transaction,
                next_wait_sequence: snapshot.next_wait_sequence,
                wait_epoch: 1,
                next_namespace: snapshot.next_namespace,
                user_namespaces: snapshot
                    .user_namespaces
                    .iter()
                    .map(|namespace| (namespace.id, namespace.clone()))
                    .collect(),
                uts_namespaces: snapshot.uts_namespaces.iter().cloned().collect(),
            }),
            child_ready: std::sync::Condvar::new(),
            activity: Arc::new(crate::RegistryActivity::default()),
            interrupts: Mutex::new(BTreeMap::new()),
            signals: super::signal::Coordination::new(),
            traces: crate::trace::Registry::new(snapshot.config.max_processes),
            topology: crate::CpuTopology::new(snapshot.config.online_cpus)?,
        })
    }

    pub fn validate_snapshot(snapshot: &RegistrySnapshot) -> Result<(), TaskError> {
        let config = snapshot.config;
        let topology = crate::CpuTopology::new(config.online_cpus).map_err(|_| TaskError::InvalidSnapshot)?;
        if config.max_processes == 0
            || config.max_threads == 0
            || config.max_groups == 0
            || config.max_pending_signals == 0
            || config.online_cpus == 0
            || snapshot.process_generations.len() != config.max_processes
            || snapshot.thread_generations.len() != config.max_threads
            || snapshot.session_generations.len() != config.max_processes
            || snapshot.process_group_generations.len() != config.max_processes
            || snapshot.processes.len() > config.max_processes
            || snapshot.threads.len() > config.max_threads
            || snapshot.next_transaction == 0
            || snapshot.next_wait_sequence == 0
            || snapshot.next_namespace < 2
            || snapshot.user_namespaces.is_empty()
            || snapshot.uts_namespaces.is_empty()
        {
            return Err(TaskError::InvalidSnapshot);
        }
        let user_namespaces = snapshot
            .user_namespaces
            .iter()
            .map(|namespace| (namespace.id, namespace))
            .collect::<BTreeMap<_, _>>();
        if user_namespaces.len() != snapshot.user_namespaces.len()
            || user_namespaces
                .values()
                .any(|namespace| !Self::valid_userns(namespace, &user_namespaces, snapshot.next_namespace))
        {
            return Err(TaskError::InvalidSnapshot);
        }
        let processes = Self::process_map(snapshot)?;
        let uts_namespaces = snapshot.uts_namespaces.iter().cloned().collect::<BTreeMap<_, _>>();
        if uts_namespaces.len() != snapshot.uts_namespaces.len()
            || uts_namespaces.iter().any(|(id, value)| {
                id.kind != crate::NamespaceKind::Uts
                    || id.serial == 0
                    || id.serial >= snapshot.next_namespace
                    || value.hostname.len() > crate::UTS_NAME_MAXIMUM
                    || value.domainname.len() > crate::UTS_NAME_MAXIMUM
                    || !user_namespaces.contains_key(&value.owner())
            })
        {
            return Err(TaskError::InvalidSnapshot);
        }
        let threads = Self::thread_map(snapshot)?;
        let sessions = Self::session_map(snapshot)?;
        let groups = Self::group_map(snapshot)?;
        let init = snapshot.init.ok_or(TaskError::InvalidSnapshot)?;
        let pending_limit = config
            .max_pending_signals
            .checked_mul(SIGNAL_COUNT)
            .ok_or(TaskError::InvalidSnapshot)?;
        if processes.get(&init).is_none_or(|process| process.parent.is_some()) {
            return Err(TaskError::InvalidSnapshot);
        }
        for process in processes.values() {
            if matches!(process.lifecycle, ProcessLifecycle::Starting)
                || process.credentials.supplementary_groups().len() > config.max_groups
                || ProcessLimits::from_entries(&process.limits).is_err()
                || process.signals.pending.len() > pending_limit
            {
                return Err(TaskError::InvalidSnapshot);
            }
            Self::validate_actions(&process.signals.actions)?;
            let thread_set: BTreeSet<_> = process.threads.iter().copied().collect();
            let lifecycle_valid = match process.lifecycle {
                ProcessLifecycle::Zombie => thread_set.is_empty() && process.exit_status.is_some(),
                _ => !thread_set.is_empty() && thread_set.contains(&process.leader) && process.exit_status.is_none(),
            };
            let group_valid = match groups.get(&process.process_group) {
                Some(group) => group.session == process.session && group.members.contains(&process.id),
                None => false,
            };
            if thread_set.len() != process.threads.len()
                || !lifecycle_valid
                || !process.namespaces.valid(snapshot.next_namespace)
                || !user_namespaces.contains_key(&process.namespaces.user)
                || !uts_namespaces.contains_key(&process.namespaces.uts)
                || !Self::threads_match_process(process.id, &thread_set, &threads)
                || sessions.get(&process.session).is_none()
                || !group_valid
            {
                return Err(TaskError::InvalidSnapshot);
            }
            if !Self::valid_parent(process.id, process.parent, init, &processes) {
                return Err(TaskError::InvalidSnapshot);
            }
            if !Self::children_match_process(process.id, &process.children, &processes) {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        for thread in threads.values() {
            if thread.lifecycle == ThreadLifecycle::Starting
                || thread
                    .affinity
                    .is_some_and(|affinity| !affinity.is_subset(topology.affinity()))
                || !Self::process_owns_thread(thread.process, thread.id, &processes)
                || thread.signals.pending.len() > pending_limit
                || !Self::valid_signal_frames(&thread.signals)
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        for session in sessions.values() {
            let foreground_valid = match session.foreground_group {
                Some(group) => session.process_groups.contains(&group),
                None => true,
            };
            if !Self::process_matches_session(session.leader, session.id, &processes)
                || !Self::groups_match_session(session.id, &session.process_groups, &groups)
                || !foreground_valid
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        for group in groups.values() {
            if !group.members.contains(&group.leader)
                || !Self::session_owns_group(group.session, group.id, &sessions)
                || !Self::members_match_group(group.id, &group.members, &processes)
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        for event in &snapshot.wait_events {
            if processes.get(&event.parent).is_none()
                || !Self::valid_child_link(event.parent, event.child, None, &processes)
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        for event in &snapshot.child_events {
            if processes.get(&event.parent).is_none()
                || !Self::valid_child_link(event.parent, event.child, Some(event.process_group), &processes)
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        Ok(())
    }

    fn valid_signal_frames(signals: &SignalThreadSnapshot) -> bool {
        if signals.frames.is_empty() {
            return signals.deferred.bits() == 0;
        }
        signals.frames.len() <= crate::SIGNAL_FRAME_MAXIMUM
            && signals.frames[0].deferred.bits() == 0
            && signals
                .frames
                .windows(2)
                .all(|pair| pair[0].deferred.bits() & !pair[1].deferred.bits() == 0)
            && signals
                .frames
                .last()
                .is_some_and(|frame| frame.deferred.bits() & !signals.deferred.bits() == 0)
    }

    fn valid_userns(
        namespace: &crate::UserNamespace,
        namespaces: &BTreeMap<crate::NamespaceId, &crate::UserNamespace>,
        next: u64,
    ) -> bool {
        namespace.id.kind == crate::NamespaceKind::User
            && namespace.id.serial != 0
            && namespace.id.serial < next
            && namespace
                .parent
                .is_none_or(|parent| parent.kind == crate::NamespaceKind::User && namespaces.contains_key(&parent))
    }

    fn restore_slots<T>(generations: &[u16]) -> Vec<Slot<T>> {
        generations
            .iter()
            .map(|generation| Slot {
                generation: *generation,
                value: None,
            })
            .collect()
    }

    fn process_map(snapshot: &RegistrySnapshot) -> Result<BTreeMap<ProcessId, &crate::ProcessSnapshot>, TaskError> {
        let mut values = BTreeMap::new();
        for process in &snapshot.processes {
            let (slot, generation) = process.id.parts().ok_or(TaskError::InvalidSnapshot)?;
            if slot >= snapshot.process_generations.len()
                || process.generation != generation
                || snapshot.process_generations[slot] != generation
                || generation == 0
                || values.insert(process.id, process).is_some()
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        Ok(values)
    }

    fn thread_map(snapshot: &RegistrySnapshot) -> Result<BTreeMap<ThreadId, &crate::ThreadSnapshot>, TaskError> {
        let mut values = BTreeMap::new();
        for thread in &snapshot.threads {
            let (slot, generation) = thread.id.parts().ok_or(TaskError::InvalidSnapshot)?;
            if slot >= snapshot.thread_generations.len()
                || thread.generation != generation
                || snapshot.thread_generations[slot] != generation
                || generation == 0
                || values.insert(thread.id, thread).is_some()
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        Ok(values)
    }

    fn session_map(snapshot: &RegistrySnapshot) -> Result<BTreeMap<SessionId, &crate::SessionSnapshot>, TaskError> {
        let mut values = BTreeMap::new();
        for session in &snapshot.sessions {
            let (slot, generation) = session.id.parts().ok_or(TaskError::InvalidSnapshot)?;
            if slot >= snapshot.session_generations.len()
                || snapshot.session_generations[slot] != generation
                || generation == 0
                || values.insert(session.id, session).is_some()
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        Ok(values)
    }

    fn group_map(
        snapshot: &RegistrySnapshot,
    ) -> Result<BTreeMap<ProcessGroupId, &crate::ProcessGroupSnapshot>, TaskError> {
        let mut values = BTreeMap::new();
        for group in &snapshot.process_groups {
            let (slot, generation) = group.id.parts().ok_or(TaskError::InvalidSnapshot)?;
            if slot >= snapshot.process_group_generations.len()
                || snapshot.process_group_generations[slot] != generation
                || generation == 0
                || values.insert(group.id, group).is_some()
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        Ok(values)
    }

    fn validate_actions(actions: &[(crate::SignalNumber, SignalAction)]) -> Result<(), TaskError> {
        let mut seen = BTreeSet::new();
        for (signal, action) in actions {
            if !seen.insert(*signal)
                || matches!(*signal, crate::SignalNumber::KILL | crate::SignalNumber::STOP)
                    && *action != SignalAction::DEFAULT
            {
                return Err(TaskError::InvalidSnapshot);
            }
        }
        Ok(())
    }

    fn threads_match_process(
        process: ProcessId,
        members: &BTreeSet<ThreadId>,
        threads: &BTreeMap<ThreadId, &crate::ThreadSnapshot>,
    ) -> bool {
        members
            .iter()
            .all(|thread| threads.get(thread).is_some_and(|value| value.process == process))
    }

    fn valid_parent(
        process: ProcessId,
        parent: Option<ProcessId>,
        init: ProcessId,
        processes: &BTreeMap<ProcessId, &crate::ProcessSnapshot>,
    ) -> bool {
        match parent {
            Some(parent) => processes
                .get(&parent)
                .is_some_and(|value| value.children.contains(&process)),
            None => process == init,
        }
    }

    fn process_owns_thread(
        process: ProcessId,
        thread: ThreadId,
        processes: &BTreeMap<ProcessId, &crate::ProcessSnapshot>,
    ) -> bool {
        processes
            .get(&process)
            .is_some_and(|process| process.threads.contains(&thread))
    }

    fn process_matches_session(
        process: ProcessId,
        session: SessionId,
        processes: &BTreeMap<ProcessId, &crate::ProcessSnapshot>,
    ) -> bool {
        processes
            .get(&process)
            .is_some_and(|process| process.session == session)
    }

    fn session_owns_group(
        session: SessionId,
        group: ProcessGroupId,
        sessions: &BTreeMap<SessionId, &crate::SessionSnapshot>,
    ) -> bool {
        sessions
            .get(&session)
            .is_some_and(|session| session.process_groups.contains(&group))
    }

    fn valid_child_link(
        parent: ProcessId,
        child: ProcessId,
        group: Option<ProcessGroupId>,
        processes: &BTreeMap<ProcessId, &crate::ProcessSnapshot>,
    ) -> bool {
        let Some(child) = processes.get(&child) else {
            return false;
        };
        child.parent == Some(parent) && group.is_none_or(|group| child.process_group == group)
    }

    fn children_match_process(
        process: ProcessId,
        children: &[ProcessId],
        processes: &BTreeMap<ProcessId, &crate::ProcessSnapshot>,
    ) -> bool {
        children
            .iter()
            .all(|child| processes.get(child).is_some_and(|value| value.parent == Some(process)))
    }

    fn groups_match_session(
        session: SessionId,
        members: &[ProcessGroupId],
        groups: &BTreeMap<ProcessGroupId, &crate::ProcessGroupSnapshot>,
    ) -> bool {
        members
            .iter()
            .all(|group| groups.get(group).is_some_and(|value| value.session == session))
    }

    fn members_match_group(
        group: ProcessGroupId,
        members: &[ProcessId],
        processes: &BTreeMap<ProcessId, &crate::ProcessSnapshot>,
    ) -> bool {
        members.iter().all(|member| {
            processes
                .get(member)
                .is_some_and(|process| process.process_group == group)
        })
    }
}
