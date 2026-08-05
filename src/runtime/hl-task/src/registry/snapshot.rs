use super::{State, TaskRegistry};
use crate::{
    ProcessGroupId, ProcessGroupSnapshot, ProcessId, ProcessObservation, ProcessSnapshot, RegistryConfig,
    RegistrySnapshot, SessionId, SessionSnapshot, SignalAction, SignalNumber, SignalProcessSnapshot,
    SignalThreadSnapshot, ThreadId, ThreadSnapshot,
};

impl TaskRegistry {
    /// Observes the state shared by simple current-process Linux operations.
    pub fn process_observation(&self, id: ProcessId) -> Result<ProcessObservation, crate::TaskError> {
        let state = self.lock();
        let process = Self::process(&state, id)?;
        Ok(ProcessObservation {
            parent: process.parent,
            credentials: process.credentials.clone(),
            parent_death_signal: process.parent_death_signal,
            child_subreaper: process.child_subreaper,
            dumpable: process.dumpable,
            timer_slack: process.timer_slack,
            thp_disabled: process.thp_disabled,
            mce_policy: process.mce_policy,
        })
    }

    /// Captures one generation-qualified process without scanning or cloning
    /// unrelated registry slots.
    pub fn process_snapshot(&self, id: ProcessId) -> Result<ProcessSnapshot, crate::TaskError> {
        let state = self.lock();
        let process = Self::process(&state, id)?;
        Ok(Self::snapshot_process(id, process))
    }

    pub fn snapshot(&self) -> RegistrySnapshot {
        let state = self.lock();
        self.snapshot_locked(&state)
    }

    pub fn checkpoint_snapshot(&self) -> Result<RegistrySnapshot, crate::TaskError> {
        if !self.activity.frozen() {
            return Err(crate::TaskError::InvalidLifecycle);
        }
        if !self
            .signals
            .reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
        {
            return Err(crate::TaskError::InvalidLifecycle);
        }
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(self.snapshot_locked(&state))
    }

    fn snapshot_locked(&self, state: &State) -> RegistrySnapshot {
        let processes = state
            .processes
            .iter()
            .enumerate()
            .filter_map(|(slot, entry)| {
                let process = entry.value.as_ref()?;
                let id = ProcessId::new(slot as u32, entry.generation);
                Some(Self::snapshot_process(id, process))
            })
            .collect();
        let threads = state
            .threads
            .iter()
            .enumerate()
            .filter_map(|(slot, entry)| {
                let thread = entry.value.as_ref()?;
                Some(ThreadSnapshot {
                    id: ThreadId::new(slot as u32, entry.generation),
                    generation: entry.generation,
                    process: thread.process,
                    lifecycle: thread.lifecycle,
                    cancellation_pending: thread.cancellation_pending,
                    signal_pending: thread.signal_pending,
                    signals: SignalThreadSnapshot {
                        mask: thread.signals.mask,
                        alternate_stack: thread.signals.alternate_stack,
                        pending: thread.signals.pending.snapshot(),
                        deferred: thread.signals.deferred,
                        frames: thread.signals.frames.clone(),
                    },
                    robust_list: thread.robust_list,
                    clear_tid: thread.clear_tid,
                    name: thread.name,
                    affinity: thread.affinity,
                    schedule: thread.schedule,
                })
            })
            .collect();
        let sessions = state
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(slot, entry)| {
                let session = entry.value.as_ref()?;
                Some(SessionSnapshot {
                    id: SessionId::new(slot as u32, entry.generation),
                    leader: session.leader,
                    process_groups: session.process_groups.iter().copied().collect(),
                    foreground_group: session.foreground_group,
                })
            })
            .collect();
        let process_groups = state
            .process_groups
            .iter()
            .enumerate()
            .filter_map(|(slot, entry)| {
                let group = entry.value.as_ref()?;
                Some(ProcessGroupSnapshot {
                    id: ProcessGroupId::new(slot as u32, entry.generation),
                    session: group.session,
                    leader: group.leader,
                    members: group.members.iter().copied().collect(),
                    orphaned: group.orphaned,
                })
            })
            .collect();
        RegistrySnapshot {
            config: RegistryConfig {
                max_processes: state.processes.len(),
                max_threads: state.threads.len(),
                max_groups: self.max_groups,
                max_pending_signals: self.max_pending_signals,
                online_cpus: self.topology.online(),
            },
            process_generations: state.processes.iter().map(|slot| slot.generation).collect(),
            thread_generations: state.threads.iter().map(|slot| slot.generation).collect(),
            session_generations: state.sessions.iter().map(|slot| slot.generation).collect(),
            process_group_generations: state.process_groups.iter().map(|slot| slot.generation).collect(),
            init: state.init,
            processes,
            threads,
            wait_events: state.waits.iter().copied().collect(),
            child_events: state.child_events.iter().copied().collect(),
            sessions,
            process_groups,
            next_transaction: state.next_transaction,
            next_wait_sequence: state.next_wait_sequence,
            next_namespace: state.next_namespace,
            user_namespaces: state.user_namespaces.values().cloned().collect(),
            uts_namespaces: state
                .uts_namespaces
                .iter()
                .map(|(id, identity)| (*id, identity.clone()))
                .collect(),
        }
    }

    fn snapshot_process(id: ProcessId, process: &super::Process) -> ProcessSnapshot {
        ProcessSnapshot {
            id,
            generation: id.wire_parts().1,
            lifecycle: process.lifecycle,
            parent: process.parent,
            children: process.children.iter().copied().collect(),
            threads: process.threads.iter().copied().collect(),
            leader: process.leader,
            session: process.session,
            process_group: process.process_group,
            terminal_detached: process.terminal_detached,
            child_class: process.child_class,
            execed: process.execed,
            arguments: process.arguments.clone(),
            name: process.name,
            credentials: process.credentials.clone(),
            limits: process.limits.entries(),
            exit_status: process.exit_status,
            signals: SignalProcessSnapshot {
                actions: process
                    .signals
                    .actions
                    .iter()
                    .enumerate()
                    .filter_map(|(index, action)| {
                        let number = u8::try_from(index + 1).ok()?;
                        (*action != SignalAction::DEFAULT).then_some((SignalNumber::new(number).ok()?, *action))
                    })
                    .collect(),
                pending: process.signals.pending.snapshot(),
            },
            namespaces: process.namespaces,
            parent_death_signal: process.parent_death_signal,
            child_subreaper: process.child_subreaper,
            cpu_usage: process.cpu_usage,
            dumpable: process.dumpable,
            oom_score_adj: process.oom_score_adj,
            timer_slack: process.timer_slack,
            thp_disabled: process.thp_disabled,
            mce_policy: process.mce_policy,
            personality: process.personality,
        }
    }
}
