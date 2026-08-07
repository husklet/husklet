//! Run claim, release, and park transitions for the thread set.

use hl_runtime::RuntimeThreadError;
use hl_task::ThreadId;

use std::ops::Bound::{Excluded, Unbounded};
use std::sync::atomic::Ordering;

use super::RunOwnership;
use super::{ResumeReject, ThreadRun, ThreadSet};

impl ThreadSet {
    pub(in crate::ffi::linux::execution) fn next(&self) -> Option<ThreadRun> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _request = self.continuation_request();
        let selected_thread = state
            .previous
            .and_then(|previous| {
                state
                    .machines
                    .range((Excluded(previous), Unbounded))
                    .find(|(thread, _)| {
                        !state.parked.contains(thread) && state.ownership.get(thread) == Some(&RunOwnership::Ready)
                    })
            })
            .or_else(|| {
                state.machines.iter().find(|(thread, _)| {
                    !state.parked.contains(thread) && state.ownership.get(thread) == Some(&RunOwnership::Ready)
                })
            })
            .map(|(thread, _)| *thread);
        let selected = selected_thread.and_then(|thread| {
            state.ownership.insert(thread, RunOwnership::Running);
            state.machines.get(&thread).map(Self::copy_run)
        });
        if let Some(run) = &selected {
            state.previous = Some(run.thread);
        }
        selected
    }

    #[cfg(test)]
    pub(in crate::ffi::linux::execution) fn claim(
        &self,
        thread: ThreadId,
        generation: u64,
    ) -> Result<ThreadRun, RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        let run = state.machines.get(&thread).ok_or(RuntimeThreadError::Missing)?;
        if run.generation != generation
            || state.parked.contains(&thread)
            || state.ownership.get(&thread) != Some(&RunOwnership::Ready)
        {
            return Err(RuntimeThreadError::Missing);
        }
        let run = Self::copy_run(run);
        state.ownership.insert(thread, RunOwnership::Running);
        Ok(run)
    }

    pub(in crate::ffi::linux::execution) fn release(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        let current = state.machines.get(&run.thread).ok_or(RuntimeThreadError::Missing)?;
        if current.process != run.process || current.generation != run.generation {
            return Err(RuntimeThreadError::Missing);
        }
        match state.ownership.get(&run.thread) {
            Some(RunOwnership::Running) => {
                state.ownership.insert(run.thread, RunOwnership::Ready);
            }
            Some(RunOwnership::Retired) => {
                state.machines.remove(&run.thread);
                state.ownership.remove(&run.thread);
                state.parked.remove(&run.thread);
                state.syscall_parked.remove(&run.thread);
                state.gated.remove(&run.thread);
                drop(state);
                if let Some(tasks) = &self.tasks {
                    tasks.unregister_interrupt(run.thread);
                }
            }
            _ => return Err(RuntimeThreadError::Missing),
        }
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn is_parked(&self, run: &ThreadRun) -> bool {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.machines.get(&run.thread).is_some_and(|current| {
            current.process == run.process
                && current.generation == run.generation
                && state.ownership.get(&run.thread) == Some(&RunOwnership::Ready)
                && state.parked.contains(&run.thread)
        })
    }

    pub(in crate::ffi::linux::execution) fn terminate_run(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        let current = state.machines.get(&run.thread).ok_or(RuntimeThreadError::Missing)?;
        if current.process != run.process
            || current.generation != run.generation
            || state.ownership.get(&run.thread) != Some(&RunOwnership::Running)
        {
            return Err(RuntimeThreadError::Missing);
        }
        state.machines.remove(&run.thread);
        state.ownership.remove(&run.thread);
        state.parked.remove(&run.thread);
        state.syscall_parked.remove(&run.thread);
        state.gated.remove(&run.thread);
        drop(state);
        if let Some(tasks) = &self.tasks {
            tasks.unregister_interrupt(run.thread);
        }
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn park(&self, thread: ThreadId) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        if !state.machines.contains_key(&thread)
            || state.ownership.get(&thread) != Some(&RunOwnership::Running)
            || !state.parked.insert(thread)
        {
            return Err(RuntimeThreadError::Missing);
        }
        state.ownership.insert(thread, RunOwnership::Ready);
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn resume(&self, thread: ThreadId) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        if !state.machines.contains_key(&thread)
            || state.ownership.get(&thread) != Some(&RunOwnership::Ready)
            || !state.parked.remove(&thread)
        {
            return Err(RuntimeThreadError::Missing);
        }
        let blocked = state.syscall_parked.remove(&thread);
        drop(state);
        if blocked && let Some(tasks) = &self.tasks {
            tasks
                .set_thread_blocked(thread, false)
                .map_err(|_| RuntimeThreadError::Invalid)?;
        }
        Ok(())
    }

    /// Count of completions this set refused for a thread that was still live —
    /// each one is a runnable task the scheduler will never see again.
    pub(in crate::ffi::linux::execution) fn lost_completions(&self) -> u64 {
        self.lost_completions.load(Ordering::Relaxed)
    }

    pub(in crate::ffi::linux::execution) fn resume_run(&self, run: &ThreadRun) -> Result<(), ResumeReject> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| ResumeReject::Invalid)?;
        let _request = self.continuation_request();
        let Some(current) = state.machines.get(&run.thread) else {
            return Err(ResumeReject::Retired);
        };
        if current.process != run.process || current.generation != run.generation {
            return Err(ResumeReject::Retired);
        }
        let ownership = state.ownership.get(&run.thread).copied();
        if ownership == Some(RunOwnership::Retired) {
            return Err(ResumeReject::Retired);
        }
        if ownership != Some(RunOwnership::Waiter) || !state.parked.remove(&run.thread) {
            drop(state);
            self.lost_completions.fetch_add(1, Ordering::Relaxed);
            return Err(ResumeReject::Live(ownership));
        }
        state.ownership.insert(run.thread, RunOwnership::Running);
        let blocked = state.syscall_parked.remove(&run.thread);
        drop(state);
        if blocked && let Some(tasks) = &self.tasks {
            tasks
                .set_thread_blocked(run.thread, false)
                .map_err(|_| ResumeReject::Invalid)?;
        }
        Ok(())
    }

    /// Cancels a waiter handoff that was rejected before a worker accepted it.
    /// Only the exact generation-qualified waiter can be returned to Running;
    /// a stale token or an active execution owner is never reclaimed.
    pub(in crate::ffi::linux::execution) fn abort_waiter(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        let current = state.machines.get(&run.thread).ok_or(RuntimeThreadError::Missing)?;
        if current.process != run.process
            || current.generation != run.generation
            || state.ownership.get(&run.thread) != Some(&RunOwnership::Waiter)
        {
            return Err(RuntimeThreadError::Missing);
        }
        state.parked.remove(&run.thread);
        let blocked = state.syscall_parked.remove(&run.thread);
        state.ownership.insert(run.thread, RunOwnership::Running);
        drop(state);
        if blocked && let Some(tasks) = &self.tasks {
            let _ = tasks.set_thread_blocked(run.thread, false);
        }
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn park_syscall(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        let current = state.machines.get(&run.thread).ok_or(RuntimeThreadError::Missing)?;
        if current.process != run.process
            || current.generation != run.generation
            || state.ownership.get(&run.thread) != Some(&RunOwnership::Running)
            || !state.parked.insert(run.thread)
            || !state.syscall_parked.insert(run.thread)
        {
            return Err(RuntimeThreadError::Missing);
        }
        state.ownership.insert(run.thread, RunOwnership::Waiter);
        drop(state);
        if let Some(tasks) = &self.tasks {
            tasks
                .set_thread_blocked(run.thread, true)
                .map_err(|_| RuntimeThreadError::Invalid)?;
            if tasks.restart_interrupted_signal(run.thread).ok().flatten().is_some() {
                run.cancellation.wake();
            }
        }
        Ok(())
    }
}
