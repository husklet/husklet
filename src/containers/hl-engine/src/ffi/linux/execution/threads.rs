use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hl_execution::{ExecutionMachine, ExecutionSnapshot};
use hl_runtime::RuntimeSyscallRouter;
use hl_runtime::RuntimeThreadError;
use hl_task::{ForkProcessPlan, ProcessId, ThreadId};

mod run;
mod signal;
mod stage;

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
}
