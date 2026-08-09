//! Signal delivery, stop gates, and group termination for the thread set.

use std::sync::Arc;

use hl_runtime::{RuntimeSyscallRouter, RuntimeThreadError};
#[cfg(test)]
use hl_task::ProcessId;

use super::{RunOwnership, SetState, ThreadRun, ThreadSet};

/// Lifts the stop gate off every thread the process still holds gated.
///
/// A waiter lane still owes a completion for a syscall-parked run, and `resume_run` is the only
/// transition that may unpark it, so a parked run keeps its park here rather than losing it.
fn release_gated_threads(state: &mut SetState, process: hl_task::ProcessId) {
    let release = state
        .gated
        .iter()
        .filter_map(|(thread, (owner, generation, _))| (*owner == process).then_some((*thread, *generation)))
        .collect::<Vec<_>>();
    for (thread, generation) in release {
        state.gated.remove(&thread);
        let owned = state
            .machines
            .get(&thread)
            .is_some_and(|run| run.process == process && run.generation == generation);
        if owned && !state.syscall_parked.contains(&thread) {
            state.parked.remove(&thread);
        }
    }
}

impl ThreadSet {
    pub(in crate::ffi::linux::execution) fn interrupt_signals(&self) {
        let Some(tasks) = &self.tasks else {
            return;
        };
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let parked = state
            .syscall_parked
            .iter()
            .filter_map(|thread| {
                state
                    .machines
                    .get(thread)
                    .map(|run| (*thread, Arc::clone(&run.cancellation)))
            })
            .collect::<Vec<_>>();
        drop(state);
        for (thread, cancellation) in parked {
            if tasks.restart_interrupted_signal(thread).ok().flatten().is_some() {
                cancellation.wake();
            }
        }
    }

    pub(super) fn generation(state: &mut SetState) -> Result<u64, RuntimeThreadError> {
        let generation = state.next_generation;
        state.next_generation = generation.checked_add(1).ok_or(RuntimeThreadError::Capacity)?;
        Ok(generation)
    }

    pub(in crate::ffi::linux::execution) fn cancel_all(&self, signal: i32) {
        let _request = self.continuation_request();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _request = self.continuation_request();
        let signal = *state.cancellation.get_or_insert(signal);
        for run in state.machines.values() {
            // Cancellation has two independently blocking execution domains.
            // The readiness token releases host syscalls, while the native
            // interrupt forces translated code back to the scheduler where
            // the terminal signal is observed.  Waking only one domain can
            // leave a compute-bound guest running forever.
            let _ = run.interrupt.set(true);
            run.cancellation.request(signal);
        }
    }

    pub(in crate::ffi::linux::execution) fn deliver_process_signal(
        &self,
        signal: i32,
    ) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let signal = hl_task::SignalNumber::new(u8::try_from(signal).map_err(|_| RuntimeThreadError::Invalid)?)
            .map_err(|_| RuntimeThreadError::Invalid)?;
        let (tasks, process) = {
            let tasks = self.tasks.clone().ok_or(RuntimeThreadError::Invalid)?;
            let state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
            let process = state
                .machines
                .values()
                .next()
                .map(|run| run.process)
                .ok_or(RuntimeThreadError::Missing)?;
            (tasks, process)
        };
        tasks
            .enqueue_signal(
                hl_task::PendingTarget::Process(process),
                hl_task::SignalInfo::bare(signal),
            )
            .map_err(|_| RuntimeThreadError::Invalid)?;
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn signal(&self) -> Option<(Arc<RuntimeSyscallRouter>, i32)> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.machines.values().find_map(|run| {
            run.cancellation
                .signal()
                .map(|signal| (Arc::clone(&run.router), signal))
        })
    }

    pub(in crate::ffi::linux::execution) fn is_empty(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .machines
            .is_empty()
    }

    pub(in crate::ffi::linux::execution) fn has_parked(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .parked
            .is_empty()
    }

    #[cfg(test)]
    pub(in crate::ffi::linux::execution) fn cancel_parked_process(&self, process: ProcessId) -> bool {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let selected = state.syscall_parked.iter().find_map(|thread| {
            state
                .machines
                .get(thread)
                .filter(|run| run.process == process)
                .map(|run| Arc::clone(&run.cancellation))
        });
        drop(state);
        if let Some(cancellation) = selected {
            cancellation.wake();
            true
        } else {
            false
        }
    }

    pub(in crate::ffi::linux::execution) fn install_stop_gate(
        &self,
        run: &ThreadRun,
        epoch: u64,
    ) -> Result<bool, RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        let current = state.machines.get(&run.thread).ok_or(RuntimeThreadError::Missing)?;
        if current.process != run.process || current.generation != run.generation {
            return Err(RuntimeThreadError::Missing);
        }
        if state.control_epochs.get(&run.process).copied().unwrap_or(0) > epoch {
            return Ok(false);
        }
        if state.machines.values().any(|candidate| {
            candidate.process == run.process
                && candidate.thread != run.thread
                && state.ownership.get(&candidate.thread) == Some(&RunOwnership::Running)
        }) {
            // The C engine interrupts peers and waits for them to leave
            // run_guest before changing process-wide execution state.
            return Err(RuntimeThreadError::Invalid);
        }
        state.stop_gates.insert(run.process, epoch);
        let members = state
            .machines
            .values()
            .filter(|candidate| candidate.process == run.process)
            .map(|candidate| (candidate.thread, candidate.generation))
            .collect::<Vec<_>>();
        for (thread, generation) in members {
            if thread == run.thread && state.ownership.get(&thread) == Some(&RunOwnership::Running) {
                state.ownership.insert(thread, RunOwnership::Ready);
            }
            state.parked.insert(thread);
            state.gated.insert(thread, (run.process, generation, epoch));
        }
        Ok(true)
    }

    pub(in crate::ffi::linux::execution) fn process_control(&self, event: hl_task::SignalActivityEvent) {
        let hl_task::SignalActivityKind::ProcessControl { process, action } = event.kind else {
            return;
        };
        let _request = self.continuation_request();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _request = self.continuation_request();
        state
            .control_epochs
            .entry(process)
            .and_modify(|epoch| *epoch = (*epoch).max(event.control_epoch))
            .or_insert(event.control_epoch);
        let wake = if action == hl_task::ProcessControlAction::Kill {
            state.syscall_parked.iter().find_map(|thread| {
                state
                    .machines
                    .get(thread)
                    .filter(|run| run.process == process)
                    .map(|run| Arc::clone(&run.cancellation))
            })
        } else {
            None
        };
        if state
            .stop_gates
            .get(&process)
            .is_some_and(|stop| event.control_epoch > *stop)
        {
            state.stop_gates.remove(&process);
            release_gated_threads(&mut state, process);
        }
        drop(state);
        if let Some(cancellation) = wake {
            cancellation.wake();
        }
    }

    pub(in crate::ffi::linux::execution) fn terminate_all(&self) {
        let _request = self.continuation_request();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _request = self.continuation_request();
        let threads = state.machines.keys().copied().collect::<Vec<_>>();
        let mut removed = Vec::new();
        for thread in threads {
            if matches!(
                state.ownership.get(&thread),
                Some(RunOwnership::Running | RunOwnership::Retired)
            ) {
                state.ownership.insert(thread, RunOwnership::Retired);
                if let Some(run) = state.machines.get(&thread) {
                    let _ = run.interrupt.set(true);
                    run.cancellation.request(9);
                }
            } else {
                if let Some(run) = state.machines.remove(&thread) {
                    let _ = run.interrupt.set(true);
                    run.cancellation.request(9);
                    removed.push(thread);
                }
                state.ownership.remove(&thread);
            }
        }
        state.parked.clear();
        state.syscall_parked.clear();
        state.stop_gates.clear();
        state.gated.clear();
        state.control_epochs.clear();
        state.previous = None;
        drop(state);
        if let Some(tasks) = &self.tasks {
            for thread in removed {
                tasks.unregister_interrupt(thread);
            }
        }
    }

    pub(in crate::ffi::linux::execution) fn terminate_group(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _request = self.continuation_request();
        let current = state.machines.get(&run.thread).ok_or(RuntimeThreadError::Missing)?;
        if current.process != run.process
            || current.generation != run.generation
            || state.ownership.get(&run.thread) != Some(&RunOwnership::Running)
            || state.machines.values().any(|candidate| {
                candidate.process == run.process
                    && candidate.thread != run.thread
                    && state.ownership.get(&candidate.thread) == Some(&RunOwnership::Running)
            })
        {
            return Err(RuntimeThreadError::Invalid);
        }
        let removed = state
            .machines
            .iter()
            .filter_map(|(thread, candidate)| (candidate.process == run.process).then_some(*thread))
            .collect::<Vec<_>>();
        let mut retired = Vec::with_capacity(removed.len());
        for thread in removed {
            if let Some(run) = state.machines.remove(&thread) {
                state.ownership.remove(&thread);
                retired.push(run);
            }
            state.parked.remove(&thread);
            state.syscall_parked.remove(&thread);
            state.gated.remove(&thread);
        }
        state.previous = None;
        state.stop_gates.remove(&run.process);
        state.control_epochs.remove(&run.process);
        drop(state);
        // Group exit owns every removed execution, including calls currently
        // blocked in waiter lanes. Wake those lanes before their ThreadRun is
        // dropped so Pool::stop can join them after schedule returns.
        for run in retired {
            run.cancellation.request(9);
            if let Some(tasks) = &self.tasks {
                tasks.unregister_interrupt(run.thread);
            }
        }
        Ok(())
    }
}
