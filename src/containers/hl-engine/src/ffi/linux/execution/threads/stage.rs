//! Staged image and thread publication adapters.

use std::sync::Arc;

use hl_execution::ExecutionSnapshot;
use hl_runtime::{PreparedThread, RuntimeThreadError, RuntimeThreadPort};
use hl_task::{ForkProcessPlan, ThreadId};

use super::{PreparedImage, RunOwnership, ThreadContext, ThreadSet, ThreadStage};

impl hl_runtime::VforkWake for ThreadSet {
    fn resume(&self, parent: ThreadId) -> Result<(), ()> {
        ThreadSet::resume(self, parent).map_err(|_| ())
    }
}

impl PreparedImage {
    pub(in crate::ffi::linux::execution) fn publish(&mut self) -> Result<(), RuntimeThreadError> {
        if self.published {
            return Err(RuntimeThreadError::Duplicate);
        }
        let _request = self.continuation.request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation.request();
        let current = state.machines.get(&self.caller).ok_or(RuntimeThreadError::Missing)?;
        if current.generation != self.caller_generation
            || state.ownership.get(&self.caller) != Some(&RunOwnership::Running)
        {
            return Err(RuntimeThreadError::Missing);
        }
        let process = current.process;
        if state.machines.values().any(|run| {
            run.process == process
                && run.thread != self.caller
                && state.ownership.get(&run.thread) == Some(&RunOwnership::Running)
        }) {
            return Err(RuntimeThreadError::Invalid);
        }
        let siblings = state
            .machines
            .iter()
            .filter_map(|(thread, run)| (run.process == process).then_some(*thread))
            .collect::<Vec<_>>();
        for thread in siblings {
            let run = state.machines.remove(&thread).expect("selected runnable exists");
            let ownership = state.ownership.remove(&thread).unwrap_or(RunOwnership::Ready);
            state.parked.remove(&thread);
            state.syscall_parked.remove(&thread);
            self.previous.push((run, ownership));
        }
        let candidate = self.candidate.take().ok_or(RuntimeThreadError::Missing)?;
        if let Some(signal) = state.cancellation {
            let _ = candidate.interrupt.set(true);
            candidate.cancellation.request(signal);
        }
        state.machines.insert(self.target, candidate);
        state.ownership.insert(self.target, RunOwnership::Ready);
        state.previous = None;
        state.reserved -= 1;
        self.published = true;
        Ok(())
    }

    pub(in crate::ffi::linux::execution) fn rollback(&mut self) {
        if !self.published {
            return;
        }
        let _request = self.continuation.request();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.continuation.invalidate();
        self.candidate = state.machines.remove(&self.target);
        state.ownership.remove(&self.target);
        for (run, ownership) in self.previous.drain(..) {
            state.ownership.insert(run.thread, ownership);
            state.machines.insert(run.thread, run);
        }
        state.previous = None;
        state.reserved += 1;
        self.published = false;
    }

    pub(in crate::ffi::linux::execution) fn finish(&mut self) {
        for (run, _) in self.previous.drain(..) {
            run.cancellation.request(9);
        }
        self.complete = true;
    }
}

impl Drop for PreparedImage {
    fn drop(&mut self) {
        if self.published && !self.complete {
            self.rollback();
        }
        if self.candidate.take().is_some() {
            let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.reserved = state.reserved.saturating_sub(1);
        }
    }
}

impl RuntimeThreadPort for ThreadSet {
    fn stage(
        &self,
        thread: ThreadId,
        snapshot: ExecutionSnapshot,
    ) -> Result<Box<dyn PreparedThread>, RuntimeThreadError> {
        self.stage_inner(thread, snapshot, true)
            .map(|staged| Box::new(staged) as Box<dyn PreparedThread>)
    }

    fn terminate(&self, thread: ThreadId) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        if state.ownership.get(&thread) == Some(&RunOwnership::Running) {
            // A thread id alone is not proof that its active generation has
            // returned from execution. Defer reclamation to release().
            state.ownership.insert(thread, RunOwnership::Retired);
            if let Some(run) = state.machines.get(&thread) {
                // Both blocking domains, as in `cancel_all`: releasing only the
                // host syscall leaves a compute-bound guest running forever.
                let _ = run.interrupt.set(true);
                run.cancellation.request(9);
            }
            return Ok(());
        }
        let removed = state.machines.remove(&thread).ok_or(RuntimeThreadError::Missing);
        if removed.is_ok() {
            state.ownership.remove(&thread);
            state.parked.remove(&thread);
            state.syscall_parked.remove(&thread);
            state.gated.remove(&thread);
        }
        drop(state);
        if let Ok(run) = &removed {
            run.cancellation.request(9);
            if let Some(tasks) = &self.tasks {
                tasks.unregister_interrupt(thread);
            }
        }
        removed.map(|_| ())
    }
}

impl PreparedThread for ThreadStage {
    fn publish(mut self: Box<Self>) {
        let _request = self.continuation.request();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.continuation.invalidate();
        state.reserved -= 1;
        let run = self.run.take().expect("staged runnable");
        if let Some(signal) = state.cancellation {
            let _ = run.interrupt.set(true);
            run.cancellation.request(signal);
        }
        if let Some(epoch) = state.stop_gates.get(&run.process).copied() {
            state.parked.insert(run.thread);
            state.gated.insert(run.thread, (run.process, run.generation, epoch));
        }
        state.machines.insert(self.thread, run);
        state.ownership.insert(self.thread, RunOwnership::Ready);
        self.registered = false;
    }
}

impl ThreadStage {
    pub(in crate::ffi::linux::execution) fn activate_fork(
        &mut self,
        plan: &ForkProcessPlan,
    ) -> Result<(), RuntimeThreadError> {
        if self.registered
            || self.thread != plan.thread()
            || self.run.as_ref().is_none_or(|run| run.process != plan.process())
        {
            return Err(RuntimeThreadError::Invalid);
        }
        let tasks = self.tasks.as_ref().ok_or(RuntimeThreadError::Missing)?;
        let interrupt: Arc<dyn hl_task::InterruptSink> =
            self.run.as_ref().ok_or(RuntimeThreadError::Missing)?.interrupt.clone();
        tasks
            .commit_fork_interrupt(plan, interrupt)
            .map_err(|_| RuntimeThreadError::Invalid)?;
        self.registered = true;
        Ok(())
    }
}

impl Drop for ThreadStage {
    fn drop(&mut self) {
        if self.registered {
            if let Some(tasks) = &self.tasks {
                tasks.unregister_interrupt(self.thread);
            }
            self.registered = false;
        }
        let Some(run) = self.run.take() else { return };
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.reserved -= 1;
        state.prepared.insert(
            self.thread,
            ThreadContext {
                process: run.process,
                router: run.router,
                cancellation: run.cancellation,
                space: run.space,
            },
        );
    }
}
