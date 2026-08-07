use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
mod model;
mod snapshot;
mod validate;

#[cfg(test)]
mod test;

pub use model::{
    ProcessCheckpointReference, TaskExternalCheckpoint, TaskExternalRestore, TaskRegistryImage, TaskResourceKey,
    ThreadCheckpointReference,
};

use super::{Process, ProcessGroup, Session, Slot, State, TaskRegistry, Thread};
use crate::signal::{PendingSignals, SIGNAL_COUNT, SignalProcessState, SignalThreadState};
use crate::{ProcessGroupId, ProcessId, ProcessLimits, RegistrySnapshot, SessionId, SignalAction, TaskError, ThreadId};
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
                cpu_account: Arc::new(crate::CpuAccount::restored(saved.cpu_usage.self_nanoseconds)),
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
                init_reservation: None,
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
}
