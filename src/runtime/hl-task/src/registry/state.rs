use std::ops::{Deref, DerefMut};
use std::sync::MutexGuard;

use super::{Process, ProcessGroup, Session, Slot, State, TaskRegistry, Thread};
use crate::{
    ChildEvent, ChildEventKind, ExitStatus, ForkProcessPlan, ProcessGroupId, ProcessId, ProcessLifecycle, SessionId,
    SignalDisposition, TaskError, ThreadId, ThreadLifecycle, WaitEvent, WaitSelector,
};

impl TaskRegistry {
    pub fn charge_cpu(&self, process: ProcessId, nanoseconds: u64) -> Result<(), TaskError> {
        self.cpu_account(process)?.charge(nanoseconds);
        Ok(())
    }

    pub fn cpu_account(&self, process: ProcessId) -> Result<std::sync::Arc<crate::CpuAccount>, TaskError> {
        Ok(std::sync::Arc::clone(
            &Self::process(&self.lock(), process)?.cpu_account,
        ))
    }

    pub fn cpu_usage(&self, process: ProcessId) -> Result<crate::CpuUsage, TaskError> {
        let state = self.lock();
        let process = Self::process(&state, process)?;
        Ok(crate::CpuUsage {
            self_nanoseconds: process.cpu_account.nanoseconds(),
            children_nanoseconds: process.cpu_usage.children_nanoseconds,
        })
    }

    pub(super) fn ensure_process_unreserved(state: &State, process: ProcessId) -> Result<(), TaskError> {
        if Self::process(state, process)?.pending_transaction.is_some() {
            return Err(TaskError::InvalidLifecycle);
        }
        Ok(())
    }

    pub(super) fn ensure_thread_unreserved(state: &State, thread: ThreadId) -> Result<ProcessId, TaskError> {
        let value = Self::thread(state, thread)?;
        let process = value.process;
        if value.pending_transaction.is_some() || Self::process(state, process)?.pending_transaction.is_some() {
            return Err(TaskError::InvalidLifecycle);
        }
        Ok(process)
    }

    pub(super) fn validate_wait_selector(
        state: &State,
        parent: ProcessId,
        selector: WaitSelector,
    ) -> Result<(), TaskError> {
        let parent_state = Self::process(state, parent)?;
        if parent_state.children.is_empty() {
            return Err(TaskError::NoChildren);
        }
        if let WaitSelector::Process(child) = selector {
            let child_state = Self::process(state, child)?;
            if child_state.parent != Some(parent) {
                return Err(TaskError::NotWaitable);
            }
        }
        Ok(())
    }

    pub(super) fn make_zombie(
        state: &mut State,
        process: ProcessId,
        status: ExitStatus,
        max_pending: usize,
    ) -> Result<Vec<ProcessId>, TaskError> {
        let parent = Self::process(state, process)?.parent;
        let orphaned = Self::reparent_children(state, process, max_pending)?;
        if let Some(parent) = parent {
            const SA_NOCLDWAIT: u64 = 2;
            let action = Self::process(state, parent)?.signals.actions[16];
            let auto_reap = action.disposition == SignalDisposition::Ignore || action.flags & SA_NOCLDWAIT != 0;
            if auto_reap {
                let child = Self::process(state, process)?;
                let child_usage = child
                    .cpu_account
                    .nanoseconds()
                    .saturating_add(child.cpu_usage.children_nanoseconds);
                let usage = &mut Self::process_mut(state, parent)?.cpu_usage.children_nanoseconds;
                *usage = usage.saturating_add(child_usage);
                Self::queue_child_signal(state, parent, process, ChildEventKind::Exited(status), max_pending)?;
                Self::process_mut(state, parent)?.children.remove(&process);
                Self::detach_group_member(state, process)?;
                Self::release_process(state, process)?;
                state.wait_epoch = state.wait_epoch.wrapping_add(1).max(1);
                return Ok(orphaned);
            }
        }
        let process_state = Self::process_mut(state, process)?;
        process_state.lifecycle = ProcessLifecycle::Zombie;
        process_state.exit_status = Some(status);
        if let Some(parent) = parent {
            let sequence = state.next_wait_sequence;
            state.next_wait_sequence = state.next_wait_sequence.wrapping_add(1).max(1);
            state.wait_epoch = state.wait_epoch.wrapping_add(1).max(1);
            state.waits.push_back(WaitEvent {
                parent,
                child: process,
                status,
                sequence,
            });
            let process_state = Self::process(state, process)?;
            state.child_events.push_back(ChildEvent {
                parent,
                child: process,
                process_group: process_state.process_group,
                class: process_state.child_class,
                kind: ChildEventKind::Exited(status),
                sequence,
            });
            Self::queue_child_signal(state, parent, process, ChildEventKind::Exited(status), max_pending)?;
        }
        Ok(orphaned)
    }

    pub(super) fn release_live_thread(
        state: &mut State,
        process: ProcessId,
        thread: ThreadId,
    ) -> Result<(), TaskError> {
        Self::process_mut(state, process)?.threads.remove(&thread);
        Self::release_thread(state, thread)
    }

    pub(super) fn validate_fork_plan(state: &State, plan: &ForkProcessPlan) -> Result<(), TaskError> {
        let process = Self::process(state, plan.process)?;
        let thread = Self::thread(state, plan.thread)?;
        if process.lifecycle != ProcessLifecycle::Starting
            || process.pending_transaction != Some(plan.transaction)
            || thread.lifecycle != ThreadLifecycle::Starting
            || thread.pending_transaction != Some(plan.transaction)
            || thread.process != plan.process
            || process.parent != Some(plan.parent)
        {
            return Err(TaskError::InvalidPlan);
        }
        Ok(())
    }

    pub(super) fn next_transaction(state: &mut State) -> u64 {
        let transaction = state.next_transaction;
        state.next_transaction = state.next_transaction.wrapping_add(1).max(1);
        transaction
    }

    pub(super) fn slots<T>(capacity: usize) -> Vec<Slot<T>> {
        (0..capacity)
            .map(|_| Slot {
                generation: 0,
                value: None,
            })
            .collect()
    }

    pub(super) fn allocate_leader(state: &mut State) -> Result<(ProcessId, ThreadId), TaskError> {
        let (slot, _) = state
            .processes
            .iter()
            .enumerate()
            .filter(|(slot, entry)| {
                entry.value.is_none()
                    && entry.generation != u16::MAX
                    && state
                        .threads
                        .get(*slot)
                        .is_some_and(|thread| thread.value.is_none() && thread.generation != u16::MAX)
                    && state.sessions[*slot].value.is_none()
                    && state.process_groups[*slot].value.is_none()
            })
            // Prefer a slot that has been used fewer times. Besides spreading
            // generation exhaustion across the table, this matches Linux's
            // delayed PID reuse: a just-reaped child must not immediately
            // become the next child with the same guest-visible PID while
            // unused PID slots remain available.
            .min_by_key(|(slot, entry)| (entry.generation, *slot))
            .ok_or(TaskError::ProcessLimit)?;
        let process = &mut state.processes[slot];
        process.generation += 1;
        let thread = &mut state.threads[slot];
        thread.generation += 1;
        Ok((
            ProcessId::new(slot as u32, process.generation),
            ThreadId::new(slot as u32, thread.generation),
        ))
    }

    pub(super) fn allocate_thread(state: &mut State) -> Result<ThreadId, TaskError> {
        let (slot, entry) = state
            .threads
            .iter_mut()
            .enumerate()
            .filter(|(slot, entry)| {
                entry.value.is_none()
                    && entry.generation != u16::MAX
                    && state.processes.get(*slot).is_none_or(|process| process.value.is_none())
            })
            // Linux advances through its shared task-ID namespace instead of
            // immediately recycling a just-exited thread while unused IDs
            // remain. Process leaders use the same policy above.
            .min_by_key(|(slot, entry)| (entry.generation, *slot))
            .ok_or(TaskError::ThreadLimit)?;
        entry.generation += 1;
        Ok(ThreadId::new(slot as u32, entry.generation))
    }

    pub(super) fn install_process(state: &mut State, id: ProcessId, process: Process) -> Result<(), TaskError> {
        Self::process_slot_mut(state, id)?.value = Some(process);
        Ok(())
    }

    pub(super) fn install_thread(state: &mut State, id: ThreadId, thread: Thread) -> Result<(), TaskError> {
        Self::thread_slot_mut(state, id)?.value = Some(thread);
        Ok(())
    }

    pub(super) fn install_session(state: &mut State, id: SessionId, session: Session) -> Result<(), TaskError> {
        Self::session_slot_mut(state, id)?.value = Some(session);
        Ok(())
    }

    pub(super) fn install_process_group(
        state: &mut State,
        id: ProcessGroupId,
        group: ProcessGroup,
    ) -> Result<(), TaskError> {
        Self::group_slot_mut(state, id)?.value = Some(group);
        Ok(())
    }

    pub(super) fn release_process(state: &mut State, id: ProcessId) -> Result<(), TaskError> {
        Self::process_slot_mut(state, id)?.value = None;
        Ok(())
    }

    pub(super) fn release_thread(state: &mut State, id: ThreadId) -> Result<(), TaskError> {
        Self::thread_slot_mut(state, id)?.value = None;
        Ok(())
    }

    pub(super) fn release_session(state: &mut State, id: SessionId) -> Result<(), TaskError> {
        Self::session_slot_mut(state, id)?.value = None;
        Ok(())
    }

    pub(super) fn release_process_group(state: &mut State, id: ProcessGroupId) -> Result<(), TaskError> {
        Self::group_slot_mut(state, id)?.value = None;
        Ok(())
    }

    pub(super) fn process(state: &State, id: ProcessId) -> Result<&Process, TaskError> {
        Self::process_slot(state, id)?
            .value
            .as_ref()
            .ok_or(TaskError::InvalidProcess(id))
    }

    pub(super) fn process_mut(state: &mut State, id: ProcessId) -> Result<&mut Process, TaskError> {
        Self::process_slot_mut(state, id)?
            .value
            .as_mut()
            .ok_or(TaskError::InvalidProcess(id))
    }

    pub(super) fn thread(state: &State, id: ThreadId) -> Result<&Thread, TaskError> {
        Self::thread_slot(state, id)?
            .value
            .as_ref()
            .ok_or(TaskError::InvalidThread)
    }

    pub(super) fn thread_mut(state: &mut State, id: ThreadId) -> Result<&mut Thread, TaskError> {
        Self::thread_slot_mut(state, id)?
            .value
            .as_mut()
            .ok_or(TaskError::InvalidThread)
    }

    pub(super) fn session(state: &State, id: SessionId) -> Result<&Session, TaskError> {
        Self::session_slot(state, id)?
            .value
            .as_ref()
            .ok_or(TaskError::InvalidSession)
    }

    pub(super) fn session_mut(state: &mut State, id: SessionId) -> Result<&mut Session, TaskError> {
        Self::session_slot_mut(state, id)?
            .value
            .as_mut()
            .ok_or(TaskError::InvalidSession)
    }

    pub(super) fn process_group(state: &State, id: ProcessGroupId) -> Result<&ProcessGroup, TaskError> {
        Self::process_group_slot(state, id)?
            .value
            .as_ref()
            .ok_or(TaskError::InvalidProcessGroup)
    }

    pub(super) fn process_group_mut(state: &mut State, id: ProcessGroupId) -> Result<&mut ProcessGroup, TaskError> {
        Self::group_slot_mut(state, id)?
            .value
            .as_mut()
            .ok_or(TaskError::InvalidProcessGroup)
    }

    fn process_slot(state: &State, id: ProcessId) -> Result<&Slot<Process>, TaskError> {
        let (slot, generation) = id.parts().ok_or(TaskError::InvalidProcess(id))?;
        let entry = state.processes.get(slot).ok_or(TaskError::InvalidProcess(id))?;
        if entry.generation != generation {
            return Err(TaskError::InvalidProcess(id));
        }
        Ok(entry)
    }

    fn process_slot_mut(state: &mut State, id: ProcessId) -> Result<&mut Slot<Process>, TaskError> {
        let (slot, generation) = id.parts().ok_or(TaskError::InvalidProcess(id))?;
        let entry = state.processes.get_mut(slot).ok_or(TaskError::InvalidProcess(id))?;
        if entry.generation != generation {
            return Err(TaskError::InvalidProcess(id));
        }
        Ok(entry)
    }

    fn thread_slot(state: &State, id: ThreadId) -> Result<&Slot<Thread>, TaskError> {
        let (slot, generation) = id.parts().ok_or(TaskError::InvalidThread)?;
        let entry = state.threads.get(slot).ok_or(TaskError::InvalidThread)?;
        if entry.generation != generation {
            return Err(TaskError::InvalidThread);
        }
        Ok(entry)
    }

    pub(super) fn thread_slot_mut(state: &mut State, id: ThreadId) -> Result<&mut Slot<Thread>, TaskError> {
        let (slot, generation) = id.parts().ok_or(TaskError::InvalidThread)?;
        let entry = state.threads.get_mut(slot).ok_or(TaskError::InvalidThread)?;
        if entry.generation != generation {
            return Err(TaskError::InvalidThread);
        }
        Ok(entry)
    }

    fn session_slot(state: &State, id: SessionId) -> Result<&Slot<Session>, TaskError> {
        let (slot, generation) = id.parts().ok_or(TaskError::InvalidSession)?;
        let entry = state.sessions.get(slot).ok_or(TaskError::InvalidSession)?;
        if entry.generation != generation {
            return Err(TaskError::InvalidSession);
        }
        Ok(entry)
    }

    fn session_slot_mut(state: &mut State, id: SessionId) -> Result<&mut Slot<Session>, TaskError> {
        let (slot, generation) = id.parts().ok_or(TaskError::InvalidSession)?;
        let entry = state.sessions.get_mut(slot).ok_or(TaskError::InvalidSession)?;
        if entry.generation != generation {
            return Err(TaskError::InvalidSession);
        }
        Ok(entry)
    }

    fn process_group_slot(state: &State, id: ProcessGroupId) -> Result<&Slot<ProcessGroup>, TaskError> {
        let (slot, generation) = id.parts().ok_or(TaskError::InvalidProcessGroup)?;
        let entry = state.process_groups.get(slot).ok_or(TaskError::InvalidProcessGroup)?;
        if entry.generation != generation {
            return Err(TaskError::InvalidProcessGroup);
        }
        Ok(entry)
    }

    fn group_slot_mut(state: &mut State, id: ProcessGroupId) -> Result<&mut Slot<ProcessGroup>, TaskError> {
        let (slot, generation) = id.parts().ok_or(TaskError::InvalidProcessGroup)?;
        let entry = state
            .process_groups
            .get_mut(slot)
            .ok_or(TaskError::InvalidProcessGroup)?;
        if entry.generation != generation {
            return Err(TaskError::InvalidProcessGroup);
        }
        Ok(entry)
    }

    pub(super) fn refresh_orphaned_groups(state: &mut State) -> Result<Vec<ProcessId>, TaskError> {
        let groups = state
            .process_groups
            .iter()
            .enumerate()
            .filter_map(|(slot, entry)| {
                let group = entry.value.as_ref()?;
                Some((slot, group.session, group.members.clone()))
            })
            .collect::<Vec<_>>();
        let mut stopped = Vec::new();
        for (slot, session, members) in groups {
            let orphaned = Self::group_is_orphaned(state, session, &members);
            let Some(group) = state.process_groups[slot].value.as_mut() else {
                continue;
            };
            let became_orphaned = !group.orphaned && orphaned;
            group.orphaned = orphaned;
            if became_orphaned {
                stopped.extend(
                    members
                        .iter()
                        .copied()
                        .filter(|member| Self::process_is_stopped(state, *member)),
                );
            }
        }
        Ok(stopped)
    }

    fn process_is_stopped(state: &State, process: ProcessId) -> bool {
        Self::process(state, process).is_ok_and(|process| process.lifecycle == ProcessLifecycle::Stopped)
    }

    fn group_is_orphaned(state: &State, session: SessionId, members: &std::collections::BTreeSet<ProcessId>) -> bool {
        for member in members {
            let Ok(process) = Self::process(state, *member) else {
                continue;
            };
            let Some(parent) = process.parent else {
                continue;
            };
            let Ok(parent) = Self::process(state, parent) else {
                continue;
            };
            if parent.session == session && parent.process_group != process.process_group {
                return false;
            }
        }
        true
    }

    pub(super) fn lock(&self) -> Guard<'_> {
        let admission = self.activity.admit();
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Guard {
            state,
            _admission: admission,
        }
    }
}

pub(super) struct Guard<'a> {
    state: MutexGuard<'a, State>,
    _admission: super::activity::ActivityAdmission,
}

impl Deref for Guard<'_> {
    type Target = State;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl DerefMut for Guard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}
