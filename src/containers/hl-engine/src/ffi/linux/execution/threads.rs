use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hl_execution::{ExecutionMachine, ExecutionSnapshot};
use hl_runtime::RuntimeSyscallRouter;
use hl_runtime::{PreparedThread, RuntimeThreadError, RuntimeThreadPort};
use hl_task::{ForkProcessPlan, ProcessId, ThreadId};

struct SetState {
    prepared: BTreeMap<ThreadId, ThreadContext>,
    machines: BTreeMap<ThreadId, ThreadRun>,
    ownership: BTreeMap<ThreadId, RunOwnership>,
    previous: Option<ThreadId>,
    parked: BTreeSet<ThreadId>,
    syscall_parked: BTreeSet<ThreadId>,
    stop_gates: BTreeMap<ProcessId, u64>,
    gated: BTreeMap<ThreadId, (ProcessId, u64, u64)>,
    control_epochs: BTreeMap<ProcessId, u64>,
    reserved: usize,
    next_generation: u64,
    cancellation: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RunOwnership {
    Ready,
    Running,
    Waiter,
    Retired,
}

struct ThreadContext {
    process: ProcessId,
    router: Arc<RuntimeSyscallRouter>,
    cancellation: Arc<super::readiness::Cancellation>,
    space: Arc<super::space::AddressSpace>,
}

pub(super) struct ThreadRun {
    pub(super) thread: ThreadId,
    pub(super) process: ProcessId,
    pub(super) cpu_account: Option<Arc<hl_task::CpuAccount>>,
    pub(super) generation: u64,
    pub(super) machine: Arc<ExecutionMachine>,
    pub(super) router: Arc<RuntimeSyscallRouter>,
    pub(super) cancellation: Arc<super::readiness::Cancellation>,
    pub(super) space: Arc<super::space::AddressSpace>,
    pub(super) interrupt: Arc<crate::native::InterruptToken>,
}

struct ContinuationEpoch {
    value: AtomicU64,
    pending: AtomicU64,
}

impl ContinuationEpoch {
    const fn new() -> Self {
        Self {
            value: AtomicU64::new(1),
            pending: AtomicU64::new(0),
        }
    }

    fn invalidate(&self) {
        let _ = self
            .value
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| epoch.checked_add(1));
    }

    fn capture(&self) -> Option<u64> {
        if self.pending.load(Ordering::Acquire) != 0 {
            return None;
        }
        let epoch = self.value.load(Ordering::Acquire);
        (epoch != u64::MAX && self.pending.load(Ordering::Acquire) == 0).then_some(epoch)
    }

    fn request(&self) -> ContinuationRequest<'_> {
        let active = self
            .pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| pending.checked_add(1))
            .is_ok();
        self.invalidate();
        ContinuationRequest { epoch: self, active }
    }
}

struct ContinuationRequest<'a> {
    epoch: &'a ContinuationEpoch,
    active: bool,
}

impl Drop for ContinuationRequest<'_> {
    fn drop(&mut self) {
        if self.active {
            self.epoch.invalidate();
            self.epoch.pending.fetch_sub(1, Ordering::Release);
        }
    }
}

/// Lock-free evidence that one exact running thread remained the sole runnable
/// generation after the scheduler admitted its native activation.
#[must_use = "a scheduler continuation must be checked before extending an activation"]
pub(super) struct SchedulerContinuation {
    epoch: Arc<ContinuationEpoch>,
    captured: u64,
    cancellation: Arc<super::readiness::Cancellation>,
    interrupt: Arc<crate::native::InterruptToken>,
}

impl SchedulerContinuation {
    pub(super) fn is_current(&self) -> bool {
        self.epoch.value.load(Ordering::Acquire) == self.captured
            && self.epoch.pending.load(Ordering::Acquire) == 0
            && self.cancellation.signal().is_none()
            && !self.interrupt.is_set()
    }
}

pub(super) struct ThreadSet {
    capacity: usize,
    state: Arc<Mutex<SetState>>,
    tasks: Option<Arc<hl_task::TaskRegistry>>,
    counter: Option<Arc<dyn hl_execution::ArchitecturalCounter>>,
    continuation: Arc<ContinuationEpoch>,
    lost_completions: AtomicU64,
}

/// Why a waiter completion could not return its run to `Running`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ResumeReject {
    /// No live record answers to this run: the thread was reclaimed, or a newer
    /// process/generation owns the id. Discarding the completion is correct.
    Retired,
    /// The record still matches this run, so the completion belongs to a thread
    /// that is alive and will never be rescheduled by it.
    Live(Option<RunOwnership>),
    Invalid,
}

pub(super) struct ThreadStage {
    state: Arc<Mutex<SetState>>,
    tasks: Option<Arc<hl_task::TaskRegistry>>,
    thread: ThreadId,
    run: Option<ThreadRun>,
    registered: bool,
    continuation: Arc<ContinuationEpoch>,
}

pub(super) struct PreparedImage {
    state: Arc<Mutex<SetState>>,
    caller: ThreadId,
    caller_generation: u64,
    target: ThreadId,
    candidate: Option<ThreadRun>,
    previous: Vec<(ThreadRun, RunOwnership)>,
    published: bool,
    complete: bool,
    continuation: Arc<ContinuationEpoch>,
}

impl ThreadSet {
    pub(super) fn active_processes(&self) -> BTreeSet<ProcessId> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .machines
            .values()
            .map(|run| run.process)
            .collect()
    }
    #[cfg(test)]
    pub(super) fn with_state_lock_for_test<T>(&self, operation: impl FnOnce() -> T) -> T {
        let _state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        operation()
    }

    #[cfg(test)]
    pub(super) fn saturate_continuation_for_test(&self) {
        self.continuation.pending.store(u64::MAX, Ordering::Release);
        drop(self.continuation.request());
    }

    fn continuation_request(&self) -> ContinuationRequest<'_> {
        self.continuation.request()
    }

    pub(super) fn stage_fork(
        &self,
        plan: &ForkProcessPlan,
        snapshot: ExecutionSnapshot,
    ) -> Result<ThreadStage, RuntimeThreadError> {
        self.stage_inner(plan.thread(), snapshot, false)
    }

    fn stage_inner(
        &self,
        thread: ThreadId,
        snapshot: ExecutionSnapshot,
        register: bool,
    ) -> Result<ThreadStage, RuntimeThreadError> {
        let machine = Arc::new(self.machine(snapshot)?);
        let interrupt = Arc::new(crate::native::InterruptToken::create().map_err(|()| RuntimeThreadError::Invalid)?);
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        if state.machines.contains_key(&thread) {
            return Err(RuntimeThreadError::Duplicate);
        }
        if !state.prepared.contains_key(&thread) {
            return Err(RuntimeThreadError::Missing);
        }
        let generation = Self::generation(&mut state)?;
        if register && let Some(tasks) = &self.tasks {
            let sink: Arc<dyn hl_task::InterruptSink> = interrupt.clone();
            tasks
                .register_interrupt(thread, sink)
                .map_err(|_| RuntimeThreadError::Invalid)?;
        }
        let prepared = state.prepared.remove(&thread).expect("validated prepared thread");
        let cpu_account = self
            .tasks
            .as_ref()
            .and_then(|tasks| tasks.cpu_account(prepared.process).ok());
        let run = ThreadRun {
            thread,
            process: prepared.process,
            cpu_account,
            generation,
            machine,
            router: prepared.router,
            cancellation: prepared.cancellation,
            space: prepared.space,
            interrupt,
        };
        state.reserved += 1;
        Ok(ThreadStage {
            state: Arc::clone(&self.state),
            tasks: self.tasks.clone(),
            thread,
            run: Some(run),
            registered: register && self.tasks.is_some(),
            continuation: Arc::clone(&self.continuation),
        })
    }

    fn copy_run(run: &ThreadRun) -> ThreadRun {
        ThreadRun {
            thread: run.thread,
            process: run.process,
            cpu_account: run.cpu_account.clone(),
            generation: run.generation,
            machine: Arc::clone(&run.machine),
            router: Arc::clone(&run.router),
            cancellation: Arc::clone(&run.cancellation),
            space: Arc::clone(&run.space),
            interrupt: Arc::clone(&run.interrupt),
        }
    }

    pub(super) fn is_only_runnable(&self, thread: ThreadId) -> bool {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .machines
            .keys()
            .all(|candidate| *candidate == thread || state.parked.contains(candidate))
    }

    pub(super) fn continuation(&self, run: &ThreadRun) -> Option<SchedulerContinuation> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let current = state.machines.get(&run.thread)?;
        if current.process != run.process
            || current.generation != run.generation
            || state.ownership.get(&run.thread) != Some(&RunOwnership::Running)
            || state
                .machines
                .keys()
                .any(|candidate| *candidate != run.thread && !state.parked.contains(candidate))
        {
            return None;
        }
        let captured = self.continuation.capture()?;
        Some(SchedulerContinuation {
            epoch: Arc::clone(&self.continuation),
            captured,
            cancellation: Arc::clone(&run.cancellation),
            interrupt: Arc::clone(&run.interrupt),
        })
    }

    pub(super) fn acknowledge_interrupt(&self, thread: ThreadId) -> Result<bool, RuntimeThreadError> {
        self.tasks.as_ref().map_or(Ok(false), |tasks| {
            tasks
                .acknowledge_interrupt(thread)
                .map_err(|_| RuntimeThreadError::Invalid)
        })
    }

    pub(super) fn prepare_image(
        &self,
        caller: ThreadId,
        target: ThreadId,
        router: Arc<RuntimeSyscallRouter>,
        cancellation: Arc<super::readiness::Cancellation>,
        space: Arc<super::space::AddressSpace>,
        snapshot: ExecutionSnapshot,
    ) -> Result<PreparedImage, RuntimeThreadError> {
        let machine = Arc::new(self.machine(snapshot)?);
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let Some((current_process, caller_generation, interrupt, cpu_account)) =
            state.machines.get(&caller).map(|run| {
                (
                    run.process,
                    run.generation,
                    Arc::clone(&run.interrupt),
                    run.cpu_account.clone(),
                )
            })
        else {
            return Err(RuntimeThreadError::Missing);
        };
        if state.reserved == self.capacity {
            return Err(RuntimeThreadError::Missing);
        }
        let generation = Self::generation(&mut state)?;
        state.reserved += 1;
        drop(state);
        Ok(PreparedImage {
            state: Arc::clone(&self.state),
            caller,
            caller_generation,
            target,
            candidate: Some(ThreadRun {
                thread: target,
                process: current_process,
                cpu_account,
                generation,
                machine,
                router,
                cancellation,
                space,
                interrupt,
            }),
            previous: Vec::new(),
            published: false,
            complete: false,
            continuation: Arc::clone(&self.continuation),
        })
    }

    pub(super) fn find(&self, thread: ThreadId) -> Option<ThreadRun> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.machines.get(&thread).map(Self::copy_run)
    }

    pub(super) fn replace_router(
        &self,
        thread: ThreadId,
        router: Arc<RuntimeSyscallRouter>,
    ) -> Result<(), RuntimeThreadError> {
        let _request = self.continuation_request();
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        let _request = self.continuation_request();
        let run = state.machines.get_mut(&thread).ok_or(RuntimeThreadError::Missing)?;
        run.router = router;
        Ok(())
    }

    pub(super) fn new(capacity: usize) -> Result<Self, RuntimeThreadError> {
        if capacity == 0 {
            return Err(RuntimeThreadError::Capacity);
        }
        Ok(Self {
            capacity,
            tasks: None,
            counter: None,
            continuation: Arc::new(ContinuationEpoch::new()),
            lost_completions: AtomicU64::new(0),
            state: Arc::new(Mutex::new(SetState {
                machines: BTreeMap::new(),
                ownership: BTreeMap::new(),
                prepared: BTreeMap::new(),
                previous: None,
                parked: BTreeSet::new(),
                syscall_parked: BTreeSet::new(),
                stop_gates: BTreeMap::new(),
                gated: BTreeMap::new(),
                control_epochs: BTreeMap::new(),
                reserved: 0,
                next_generation: 1,
                cancellation: None,
            })),
        })
    }

    pub(super) fn with_tasks(capacity: usize, tasks: Arc<hl_task::TaskRegistry>) -> Result<Self, RuntimeThreadError> {
        let mut threads = Self::new(capacity)?;
        threads.tasks = Some(tasks);
        Ok(threads)
    }

    pub(super) fn with_counter(
        capacity: usize,
        tasks: Arc<hl_task::TaskRegistry>,
        counter: Arc<dyn hl_execution::ArchitecturalCounter>,
    ) -> Result<Self, RuntimeThreadError> {
        let mut threads = Self::with_tasks(capacity, tasks)?;
        threads.counter = Some(counter);
        Ok(threads)
    }

    fn machine(&self, snapshot: ExecutionSnapshot) -> Result<ExecutionMachine, RuntimeThreadError> {
        match &self.counter {
            Some(counter) => ExecutionMachine::new_with_counter(snapshot, Arc::clone(counter)),
            None => ExecutionMachine::new(snapshot),
        }
        .map_err(|_| RuntimeThreadError::Invalid)
    }

    pub(super) fn prepare(
        &self,
        thread: ThreadId,
        process: ProcessId,
        router: Arc<RuntimeSyscallRouter>,
        cancellation: Arc<super::readiness::Cancellation>,
        space: Arc<super::space::AddressSpace>,
    ) -> Result<(), RuntimeThreadError> {
        let mut state = self.state.lock().map_err(|_| RuntimeThreadError::Invalid)?;
        if state.machines.contains_key(&thread) || state.prepared.contains_key(&thread) {
            return Err(RuntimeThreadError::Duplicate);
        }
        if state.machines.len() + state.prepared.len() + state.reserved == self.capacity {
            return Err(RuntimeThreadError::Capacity);
        }
        state.prepared.insert(
            thread,
            ThreadContext {
                process,
                router,
                cancellation,
                space,
            },
        );
        Ok(())
    }

    pub(super) fn discard(&self, thread: ThreadId) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .prepared
            .remove(&thread);
    }

    pub(super) fn next(&self) -> Option<ThreadRun> {
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
    pub(super) fn claim(&self, thread: ThreadId, generation: u64) -> Result<ThreadRun, RuntimeThreadError> {
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

    pub(super) fn release(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
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

    pub(super) fn is_parked(&self, run: &ThreadRun) -> bool {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.machines.get(&run.thread).is_some_and(|current| {
            current.process == run.process
                && current.generation == run.generation
                && state.ownership.get(&run.thread) == Some(&RunOwnership::Ready)
                && state.parked.contains(&run.thread)
        })
    }

    pub(super) fn terminate_run(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
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

    pub(super) fn park(&self, thread: ThreadId) -> Result<(), RuntimeThreadError> {
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

    pub(super) fn resume(&self, thread: ThreadId) -> Result<(), RuntimeThreadError> {
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
    pub(super) fn lost_completions(&self) -> u64 {
        self.lost_completions.load(Ordering::Relaxed)
    }

    pub(super) fn resume_run(&self, run: &ThreadRun) -> Result<(), ResumeReject> {
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
    pub(super) fn abort_waiter(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
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

    pub(super) fn park_syscall(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
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

    pub(super) fn interrupt_signals(&self) {
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

    fn generation(state: &mut SetState) -> Result<u64, RuntimeThreadError> {
        let generation = state.next_generation;
        state.next_generation = generation.checked_add(1).ok_or(RuntimeThreadError::Capacity)?;
        Ok(generation)
    }

    pub(super) fn cancel_all(&self, signal: i32) {
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

    pub(super) fn deliver_process_signal(&self, signal: i32) -> Result<(), RuntimeThreadError> {
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

    pub(super) fn signal(&self) -> Option<(Arc<RuntimeSyscallRouter>, i32)> {
        let state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.machines.values().find_map(|run| {
            run.cancellation
                .signal()
                .map(|signal| (Arc::clone(&run.router), signal))
        })
    }

    pub(super) fn is_empty(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .machines
            .is_empty()
    }

    pub(super) fn has_parked(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .parked
            .is_empty()
    }

    #[cfg(test)]
    pub(super) fn cancel_parked_process(&self, process: ProcessId) -> bool {
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

    pub(super) fn install_stop_gate(&self, run: &ThreadRun, epoch: u64) -> Result<bool, RuntimeThreadError> {
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

    pub(super) fn process_control(&self, event: hl_task::SignalActivityEvent) {
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
            let release = state
                .gated
                .iter()
                .filter_map(|(thread, (owner, generation, _))| (*owner == process).then_some((*thread, *generation)))
                .collect::<Vec<_>>();
            for (thread, generation) in release {
                state.gated.remove(&thread);
                if state
                    .machines
                    .get(&thread)
                    .is_some_and(|run| run.process == process && run.generation == generation)
                {
                    // A waiter lane still owes a completion for a syscall-parked
                    // run, and `resume_run` is the only transition that may
                    // unpark it; clearing the park here strands it in `Waiter`.
                    if !state.syscall_parked.contains(&thread) {
                        state.parked.remove(&thread);
                    }
                }
            }
        }
        drop(state);
        if let Some(cancellation) = wake {
            cancellation.wake();
        }
    }

    pub(super) fn terminate_all(&self) {
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

    pub(super) fn terminate_group(&self, run: &ThreadRun) -> Result<(), RuntimeThreadError> {
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

impl hl_runtime::VforkWake for ThreadSet {
    fn resume(&self, parent: ThreadId) -> Result<(), ()> {
        ThreadSet::resume(self, parent).map_err(|_| ())
    }
}

impl PreparedImage {
    pub(super) fn publish(&mut self) -> Result<(), RuntimeThreadError> {
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

    pub(super) fn rollback(&mut self) {
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

    pub(super) fn finish(&mut self) {
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
    pub(super) fn activate_fork(&mut self, plan: &ForkProcessPlan) -> Result<(), RuntimeThreadError> {
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
