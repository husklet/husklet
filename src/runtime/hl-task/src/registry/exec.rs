use std::sync::Arc;

use crate::signal::{SignalProcessState, SignalThreadState};
use crate::{
    AlternateStack, ProcessId, ProcessLifecycle, RobustListRegistration, SignalAction, SignalDisposition, TaskError,
    TaskRegistry, ThreadId,
};

struct ThreadExecState {
    identifier: ThreadId,
    signals: SignalThreadState,
    robust_list: Option<RobustListRegistration>,
    clear_tid: Option<u64>,
    name: [u8; 16],
    affinity: Option<crate::CpuAffinity>,
    schedule: crate::SchedulingProfile,
}

struct TaskExecState {
    execed: bool,
    arguments: Vec<Vec<u8>>,
    name: [u8; 16],
    signals: SignalProcessState,
    threads: Vec<ThreadExecState>,
    credentials: crate::ProcessCredentials,
    dumpable: bool,
}

/// Generation-qualified, reversible task-state transition for exec.
pub struct PreparedTaskExec {
    registry: Arc<TaskRegistry>,
    process: ProcessId,
    caller: ThreadId,
    thread: ThreadId,
    transaction: u64,
    previous: Option<TaskExecState>,
    retired: Vec<(ThreadId, super::Thread)>,
    finished: bool,
    name: [u8; 16],
    arguments: Vec<Vec<u8>>,
}

impl TaskRegistry {
    pub fn prepare_exec(self: &Arc<Self>, process: ProcessId, thread: ThreadId) -> Result<PreparedTaskExec, TaskError> {
        let name = self
            .snapshot()
            .threads
            .into_iter()
            .find(|candidate| candidate.id == thread)
            .ok_or(TaskError::InvalidThread)?
            .name;
        self.prepare_named(process, thread, name)
    }

    pub fn prepare_named(
        self: &Arc<Self>,
        process: ProcessId,
        thread: ThreadId,
        name: [u8; 16],
    ) -> Result<PreparedTaskExec, TaskError> {
        let arguments = self
            .snapshot()
            .processes
            .into_iter()
            .find(|candidate| candidate.id == process)
            .ok_or(TaskError::InvalidProcess)?
            .arguments;
        self.prepare_image(process, thread, name, arguments)
    }

    pub fn prepare_image(
        self: &Arc<Self>,
        process: ProcessId,
        thread: ThreadId,
        name: [u8; 16],
        arguments: Vec<Vec<u8>>,
    ) -> Result<PreparedTaskExec, TaskError> {
        let mut state = self.lock();
        let task = Self::thread(&state, thread)?;
        if task.process != process || task.pending_transaction.is_some() {
            return Err(TaskError::WrongProcess);
        }
        let process_state = Self::process(&state, process)?;
        if process_state.pending_transaction.is_some()
            || !matches!(
                process_state.lifecycle,
                ProcessLifecycle::Running | ProcessLifecycle::Stopped
            )
        {
            return Err(TaskError::InvalidLifecycle);
        }
        let leader = process_state.leader;
        let transaction = Self::next_transaction(&mut state);
        Self::process_mut(&mut state, process)?.pending_transaction = Some(transaction);
        Self::thread_mut(&mut state, thread)?.pending_transaction = Some(transaction);
        Ok(PreparedTaskExec {
            registry: Arc::clone(self),
            process,
            caller: thread,
            thread: leader,
            transaction,
            previous: None,
            retired: Vec::new(),
            finished: false,
            name,
            arguments,
        })
    }
}

impl PreparedTaskExec {
    #[must_use]
    pub const fn resulting_thread(&self) -> ThreadId {
        self.thread
    }

    pub fn publish(&mut self) -> Result<(), TaskError> {
        if self.previous.is_some() || self.finished {
            return Err(TaskError::InvalidPlan);
        }
        let mut state = self.registry.lock();
        let process = TaskRegistry::process(&state, self.process)?;
        let caller = TaskRegistry::thread(&state, self.caller)?;
        if process.pending_transaction != Some(self.transaction)
            || caller.pending_transaction != Some(self.transaction)
            || caller.process != self.process
        {
            return Err(TaskError::InvalidPlan);
        }
        let thread_ids = process.threads.iter().copied().collect::<Vec<_>>();
        let previous = TaskExecState {
            execed: process.execed,
            arguments: process.arguments.clone(),
            name: process.name,
            signals: process.signals.clone(),
            credentials: process.credentials.clone(),
            dumpable: process.dumpable,
            threads: thread_ids
                .iter()
                .map(|identifier| {
                    let value = TaskRegistry::thread(&state, *identifier)?;
                    Ok(ThreadExecState {
                        identifier: *identifier,
                        signals: value.signals.clone(),
                        robust_list: value.robust_list,
                        clear_tid: value.clear_tid,
                        name: value.name,
                        affinity: value.affinity,
                        schedule: value.schedule,
                    })
                })
                .collect::<Result<Vec<_>, TaskError>>()?,
        };
        let process = TaskRegistry::process_mut(&mut state, self.process)?;
        for action in &mut process.signals.actions {
            if action.disposition != SignalDisposition::Ignore {
                *action = SignalAction::DEFAULT;
            }
        }
        process.execed = true;
        process.arguments.clone_from(&self.arguments);
        process.name = self.name;
        process.credentials.reset_setid_for_exec();
        let ambient = process.credentials.capabilities.ambient;
        process.credentials.capabilities.permitted |= ambient;
        process.credentials.capabilities.effective |= ambient;
        process.dumpable = true;
        process.threads.clear();
        process.threads.insert(self.thread);
        if self.caller != self.thread {
            let caller = TaskRegistry::thread_slot_mut(&mut state, self.caller)?
                .value
                .take()
                .ok_or(TaskError::InvalidThread)?;
            let leader = TaskRegistry::thread_slot_mut(&mut state, self.thread)?
                .value
                .replace(caller)
                .ok_or(TaskError::InvalidThread)?;
            self.retired.push((self.thread, leader));
        }
        for identifier in thread_ids {
            if identifier != self.thread && identifier != self.caller {
                let slot = TaskRegistry::thread_slot_mut(&mut state, identifier)?;
                let retired = slot.value.take().ok_or(TaskError::InvalidThread)?;
                self.retired.push((identifier, retired));
                continue;
            }
            if identifier != self.thread {
                continue;
            }
            let value = TaskRegistry::thread_mut(&mut state, self.thread)?;
            value.robust_list = None;
            value.clear_tid = None;
            if identifier == self.thread {
                value.signals.alternate_stack = AlternateStack::Disabled;
                value.name = self.name;
            }
        }
        self.previous = Some(previous);
        Ok(())
    }

    pub fn rollback(&mut self) {
        let retired = std::mem::take(&mut self.retired);
        let mut state = self.registry.lock();
        if let Some(previous) = self.previous.take() {
            self.restore_caller_identity(&mut state);
            Self::restore_retired(&mut state, retired);
            self.restore(&mut state, previous);
        }
        self.release(&mut state);
        self.finished = true;
    }

    fn restore_caller_identity(&self, state: &mut super::State) {
        if self.caller == self.thread {
            return;
        }
        let caller = TaskRegistry::thread_slot_mut(state, self.thread)
            .ok()
            .and_then(|slot| slot.value.take());
        let Some(caller) = caller else {
            return;
        };
        if let Ok(slot) = TaskRegistry::thread_slot_mut(state, self.caller) {
            slot.value = Some(caller);
        }
    }

    fn restore_retired(state: &mut super::State, retired: Vec<(ThreadId, super::Thread)>) {
        for (identifier, thread) in retired {
            let Ok(slot) = TaskRegistry::thread_slot_mut(state, identifier) else {
                continue;
            };
            slot.value = Some(thread);
        }
    }

    pub fn finish(&mut self) {
        let mut state = self.registry.lock();
        let retired = self.retired.iter().map(|(thread, _)| *thread).collect::<Vec<_>>();
        self.previous = None;
        self.retired.clear();
        self.release(&mut state);
        self.finished = true;
        drop(state);
        for thread in retired {
            self.registry.unregister_interrupt(thread);
        }
        self.registry.rebind_interrupt(self.caller, self.thread);
        let _ = self.registry.acknowledge_interrupt(self.thread);
        let _ = self.registry.trace_stop(self.process, crate::TraceStop::Exec);
    }

    fn release(&self, state: &mut super::State) {
        if let Ok(process) = TaskRegistry::process_mut(state, self.process)
            && process.pending_transaction == Some(self.transaction) {
                process.pending_transaction = None;
            }
        if let Ok(thread) = TaskRegistry::thread_mut(state, self.thread)
            && thread.pending_transaction == Some(self.transaction) {
                thread.pending_transaction = None;
            }
        if let Ok(caller) = TaskRegistry::thread_mut(state, self.caller)
            && caller.pending_transaction == Some(self.transaction) {
                caller.pending_transaction = None;
            }
    }

    fn restore(&self, state: &mut super::State, previous: TaskExecState) {
        if let Ok(process) = TaskRegistry::process_mut(state, self.process) {
            process.execed = previous.execed;
            process.arguments = previous.arguments;
            process.name = previous.name;
            process.signals = previous.signals;
            process.credentials = previous.credentials;
            process.dumpable = previous.dumpable;
            process.threads = previous.threads.iter().map(|thread| thread.identifier).collect();
        }
        for previous_thread in previous.threads {
            let Ok(thread) = TaskRegistry::thread_mut(state, previous_thread.identifier) else {
                continue;
            };
            thread.signals = previous_thread.signals;
            thread.robust_list = previous_thread.robust_list;
            thread.clear_tid = previous_thread.clear_tid;
            thread.name = previous_thread.name;
            thread.affinity = previous_thread.affinity;
            thread.schedule = previous_thread.schedule;
        }
    }
}

impl Drop for PreparedTaskExec {
    fn drop(&mut self) {
        if !self.finished {
            self.rollback();
        }
    }
}
