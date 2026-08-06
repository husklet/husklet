use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use crate::signal::{SignalProcessState, SignalThreadState};
use crate::{
    ChildClass, ChildEvent, CloneThreadPlan, CpuAffinity, ExitStatus, ForkProcessPlan, ProcessCredentials,
    ProcessGroupId, ProcessId, ProcessLifecycle, ProcessLimits, RegistryConfig, SessionId, TaskError, ThreadId,
    ThreadLifecycle, WaitEvent,
};

mod activity;
mod affinity;
mod cancellation;
mod checkpoint;
mod exec;
mod interrupt;
mod job;
mod mutation;
mod namespace;
mod reparent;
mod robust;
mod schedule;
mod signal;
mod snapshot;
mod state;
mod tid;
mod trace;

#[cfg(test)]
mod exec_test;
#[cfg(test)]
mod test;

pub(crate) use activity::Activity;
pub use checkpoint::{
    ProcessCheckpointReference, TASK_CHECKPOINT_VERSION, TaskExternalCheckpoint, TaskExternalRestore,
    TaskRegistryImage, TaskResourceKey, ThreadCheckpointReference,
};
pub use exec::PreparedTaskExec;

#[derive(Clone, Copy, Eq, PartialEq)]
struct InitSlots {
    process: ProcessId,
    thread: ThreadId,
    session: SessionId,
    process_group: ProcessGroupId,
    transaction: u64,
}

/// An unpublished reservation for the initial process and its identity graph.
///
/// Dropping the reservation aborts it. Reserved identities consume their
/// generations, but no process, thread, session, group, or namespace mutation
/// becomes guest-visible before [`InitReservation::commit`].
#[must_use = "the initial process reservation must be committed or dropped"]
pub struct InitReservation<'registry> {
    registry: &'registry TaskRegistry,
    slots: InitSlots,
    credentials: Option<ProcessCredentials>,
    limits: Option<ProcessLimits>,
    finished: bool,
}

impl InitReservation<'_> {
    pub fn commit(mut self) -> Result<(ProcessId, ThreadId), TaskError> {
        let credentials = self.credentials.take().ok_or(TaskError::InvalidPlan)?;
        let limits = self.limits.take().ok_or(TaskError::InvalidPlan)?;
        let result = self.registry.commit_init(self.slots, credentials, limits);
        self.finished = result.is_ok();
        result
    }
}

impl Drop for InitReservation<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.registry.abort_init(self.slots);
        }
    }
}

struct Slot<T> {
    generation: u16,
    value: Option<T>,
}

struct Process {
    control_epoch: u64,
    lifecycle: ProcessLifecycle,
    parent: Option<ProcessId>,
    children: BTreeSet<ProcessId>,
    threads: BTreeSet<ThreadId>,
    leader: ThreadId,
    session: SessionId,
    process_group: ProcessGroupId,
    /// `true` after this process alone has disassociated from its session's
    /// controlling terminal. The terminal-to-session binding is owned by the
    /// terminal catalog; this is Linux's per-process `signal_struct::tty`
    /// association.
    terminal_detached: bool,
    child_class: ChildClass,
    execed: bool,
    arguments: Vec<Vec<u8>>,
    name: [u8; 16],
    credentials: ProcessCredentials,
    limits: ProcessLimits,
    exit_status: Option<ExitStatus>,
    pending_transaction: Option<u64>,
    signals: SignalProcessState,
    namespaces: crate::NamespaceSet,
    parent_death_signal: u32,
    child_subreaper: bool,
    cpu_usage: crate::CpuUsage,
    cpu_account: Arc<crate::CpuAccount>,
    dumpable: bool,
    oom_score_adj: i16,
    timer_slack: u64,
    thp_disabled: bool,
    mce_policy: u32,
    personality: u32,
}

struct Session {
    leader: ProcessId,
    process_groups: BTreeSet<ProcessGroupId>,
    foreground_group: Option<ProcessGroupId>,
}

struct ProcessGroup {
    session: SessionId,
    leader: ProcessId,
    members: BTreeSet<ProcessId>,
    orphaned: bool,
}

struct Thread {
    process: ProcessId,
    lifecycle: ThreadLifecycle,
    cancellation_pending: bool,
    signal_pending: bool,
    pending_transaction: Option<u64>,
    signals: SignalThreadState,
    robust_list: Option<crate::RobustListRegistration>,
    clear_tid: Option<u64>,
    name: [u8; 16],
    affinity: Option<CpuAffinity>,
    schedule: crate::SchedulingProfile,
}

struct State {
    processes: Vec<Slot<Process>>,
    threads: Vec<Slot<Thread>>,
    sessions: Vec<Slot<Session>>,
    process_groups: Vec<Slot<ProcessGroup>>,
    init: Option<ProcessId>,
    init_reservation: Option<InitSlots>,
    waits: VecDeque<WaitEvent>,
    wait_reservations: BTreeSet<u64>,
    child_events: VecDeque<ChildEvent>,
    next_transaction: u64,
    next_wait_sequence: u64,
    wait_epoch: u64,
    next_namespace: u64,
    user_namespaces: BTreeMap<crate::NamespaceId, crate::UserNamespace>,
    uts_namespaces: BTreeMap<crate::NamespaceId, crate::UtsIdentity>,
}

/// Per-runtime, concurrency-safe process and thread registry.
pub struct TaskRegistry {
    max_groups: usize,
    max_pending_signals: usize,
    state: Mutex<State>,
    child_ready: Condvar,
    activity: Arc<crate::RegistryActivity>,
    interrupts: Mutex<BTreeMap<ThreadId, std::sync::Weak<dyn crate::InterruptSink>>>,
    signals: signal::Coordination,
    traces: crate::trace::Registry,
    topology: crate::CpuTopology,
}

impl TaskRegistry {
    pub fn new(config: RegistryConfig) -> Result<Self, TaskError> {
        if config.max_processes == 0
            || config.max_threads == 0
            || config.max_groups == 0
            || config.max_pending_signals == 0
            || config.online_cpus == 0
            || config.max_processes > u32::MAX as usize
            || config.max_threads > u32::MAX as usize
        {
            return Err(TaskError::InvalidCapacity);
        }
        let topology = crate::CpuTopology::new(config.online_cpus)?;
        Ok(Self {
            max_groups: config.max_groups,
            max_pending_signals: config.max_pending_signals,
            state: Mutex::new(State {
                processes: Self::slots(config.max_processes),
                threads: Self::slots(config.max_threads),
                sessions: Self::slots(config.max_processes),
                process_groups: Self::slots(config.max_processes),
                init: None,
                init_reservation: None,
                waits: VecDeque::new(),
                wait_reservations: BTreeSet::new(),
                child_events: VecDeque::new(),
                next_transaction: 1,
                next_wait_sequence: 1,
                wait_epoch: 1,
                next_namespace: 2,
                user_namespaces: Self::initial_users(),
                uts_namespaces: BTreeMap::from([(crate::NamespaceSet::initial().uts, crate::UtsIdentity::initial())]),
            }),
            child_ready: Condvar::new(),
            activity: Arc::new(crate::RegistryActivity::default()),
            interrupts: Mutex::new(BTreeMap::new()),
            signals: signal::Coordination::new(),
            traces: crate::trace::Registry::new(config.max_processes),
            topology,
        })
    }

    #[must_use]
    pub const fn topology(&self) -> crate::CpuTopology {
        self.topology
    }

    pub fn create_init(
        &self,
        credentials: ProcessCredentials,
        limits: ProcessLimits,
    ) -> Result<(ProcessId, ThreadId), TaskError> {
        self.begin_create_init(credentials, limits)?.commit()
    }

    pub fn begin_create_init(
        &self,
        credentials: ProcessCredentials,
        limits: ProcessLimits,
    ) -> Result<InitReservation<'_>, TaskError> {
        if credentials.supplementary_groups().len() > self.max_groups {
            return Err(TaskError::GroupLimit);
        }
        let mut state = self.lock();
        if state.init.is_some() || state.init_reservation.is_some() {
            return Err(TaskError::InvalidLifecycle);
        }
        let (process, thread) = Self::allocate_leader(&mut state)?;
        let session = match Self::allocate_session(&mut state, process) {
            Ok(session) => session,
            Err(error) => return Err(error),
        };
        let process_group = match Self::allocate_process_group(&mut state, process) {
            Ok(group) => group,
            Err(error) => return Err(error),
        };
        let slots = InitSlots {
            process,
            thread,
            session,
            process_group,
            transaction: Self::next_transaction(&mut state),
        };
        state.init_reservation = Some(slots);
        Ok(InitReservation {
            registry: self,
            slots,
            credentials: Some(credentials),
            limits: Some(limits),
            finished: false,
        })
    }

    fn commit_init(
        &self,
        slots: InitSlots,
        credentials: ProcessCredentials,
        limits: ProcessLimits,
    ) -> Result<(ProcessId, ThreadId), TaskError> {
        let mut state = self.lock();
        if state.init.is_some() || state.init_reservation != Some(slots) {
            return Err(TaskError::InvalidPlan);
        }
        let (process_slot, process_generation) = slots.process.parts().ok_or(TaskError::InvalidPlan)?;
        let (thread_slot, thread_generation) = slots.thread.parts().ok_or(TaskError::InvalidPlan)?;
        let (session_slot, session_generation) = slots.session.parts().ok_or(TaskError::InvalidPlan)?;
        let (group_slot, group_generation) = slots.process_group.parts().ok_or(TaskError::InvalidPlan)?;
        if state
            .processes
            .get(process_slot)
            .is_none_or(|entry| entry.generation != process_generation || entry.value.is_some())
            || state
                .threads
                .get(thread_slot)
                .is_none_or(|entry| entry.generation != thread_generation || entry.value.is_some())
            || state
                .sessions
                .get(session_slot)
                .is_none_or(|entry| entry.generation != session_generation || entry.value.is_some())
            || state
                .process_groups
                .get(group_slot)
                .is_none_or(|entry| entry.generation != group_generation || entry.value.is_some())
        {
            return Err(TaskError::InvalidPlan);
        }
        let initial_user = crate::NamespaceSet::initial().user;
        if !state.user_namespaces.contains_key(&initial_user) {
            return Err(TaskError::InvalidSnapshot);
        }
        let process = slots.process;
        let thread = slots.thread;
        let session = slots.session;
        let process_group = slots.process_group;
        let initial_user_owner = credentials.effective_user;
        Self::install_thread(
            &mut state,
            thread,
            Thread {
                process,
                lifecycle: ThreadLifecycle::Runnable,
                cancellation_pending: false,
                signal_pending: false,
                pending_transaction: None,
                signals: SignalThreadState::new(),
                robust_list: None,
                clear_tid: None,
                name: *b"hl-engine\0\0\0\0\0\0\0",
                affinity: None,
                schedule: crate::SchedulingProfile::OTHER,
            },
        )?;
        let mut threads = BTreeSet::new();
        threads.insert(thread);
        Self::install_process(
            &mut state,
            process,
            Process {
                control_epoch: 0,
                lifecycle: ProcessLifecycle::Running,
                parent: None,
                children: BTreeSet::new(),
                threads,
                leader: thread,
                session,
                process_group,
                terminal_detached: false,
                child_class: ChildClass::Standard,
                execed: false,
                arguments: Vec::new(),
                name: *b"hl-engine\0\0\0\0\0\0\0",
                credentials,
                limits,
                exit_status: None,
                pending_transaction: None,
                signals: SignalProcessState::new(),
                namespaces: crate::NamespaceSet::initial(),
                parent_death_signal: 0,
                child_subreaper: false,
                cpu_usage: crate::CpuUsage::default(),
                cpu_account: Arc::new(crate::CpuAccount::default()),
                dumpable: true,
                oom_score_adj: 0,
                timer_slack: 50_000,
                thp_disabled: false,
                mce_policy: 2,
                personality: 0,
            },
        )?;
        let mut groups = BTreeSet::new();
        groups.insert(process_group);
        Self::install_session(
            &mut state,
            session,
            Session {
                leader: process,
                process_groups: groups,
                foreground_group: Some(process_group),
            },
        )?;
        let mut members = BTreeSet::new();
        members.insert(process);
        Self::install_process_group(
            &mut state,
            process_group,
            ProcessGroup {
                session,
                leader: process,
                members,
                orphaned: true,
            },
        )?;
        state
            .user_namespaces
            .get_mut(&initial_user)
            .expect("initial user namespace validated before publication")
            .owner = initial_user_owner;
        state.init = Some(process);
        state.init_reservation = None;
        Ok((process, thread))
    }

    fn abort_init(&self, slots: InitSlots) {
        let mut state = self.lock();
        if state.init_reservation == Some(slots) {
            state.init_reservation = None;
        }
    }

    /// Publishes the argument vector belonging to the current process image.
    ///
    /// Initial image construction uses this after creating init. Later image
    /// replacement publishes arguments through the exec transaction.
    pub fn publish_arguments(&self, process: ProcessId, arguments: Vec<Vec<u8>>) -> Result<(), TaskError> {
        let mut state = self.lock();
        let process = Self::process_mut(&mut state, process)?;
        if process.pending_transaction.is_some() || process.lifecycle != ProcessLifecycle::Running {
            return Err(TaskError::InvalidLifecycle);
        }
        process.arguments = arguments;
        Ok(())
    }

    pub fn begin_clone_thread(&self, source: ThreadId) -> Result<CloneThreadPlan, TaskError> {
        let mut state = self.lock();
        let process = Self::thread(&state, source)?.process;
        let inherited_mask = Self::thread(&state, source)?.signals.mask;
        let inherited_name = Self::thread(&state, source)?.name;
        let inherited_affinity = Self::thread(&state, source)?.affinity;
        let inherited_schedule = Self::thread(&state, source)?.schedule;
        if Self::process(&state, process)?.lifecycle != ProcessLifecycle::Running
            || Self::process(&state, process)?.pending_transaction.is_some()
            || Self::thread(&state, source)?.pending_transaction.is_some()
        {
            return Err(TaskError::InvalidLifecycle);
        }
        let thread = Self::allocate_thread(&mut state)?;
        let transaction = Self::next_transaction(&mut state);
        Self::install_thread(
            &mut state,
            thread,
            Thread {
                process,
                lifecycle: ThreadLifecycle::Starting,
                cancellation_pending: false,
                signal_pending: false,
                pending_transaction: Some(transaction),
                signals: SignalThreadState {
                    mask: inherited_mask,
                    alternate_stack: crate::AlternateStack::Disabled,
                    pending: crate::signal::PendingSignals::new(),
                    deferred: crate::SignalMask::from_bits(0),
                    frames: Vec::new(),
                },
                robust_list: None,
                clear_tid: None,
                name: inherited_name,
                affinity: inherited_affinity,
                schedule: inherited_schedule,
            },
        )?;
        Self::process_mut(&mut state, process)?.threads.insert(thread);
        Ok(CloneThreadPlan {
            source,
            thread,
            transaction,
        })
    }

    pub fn commit_clone_thread(&self, plan: CloneThreadPlan) -> Result<ThreadId, TaskError> {
        let mut state = self.lock();
        let thread = Self::thread_mut(&mut state, plan.thread)?;
        if thread.lifecycle != ThreadLifecycle::Starting || thread.pending_transaction != Some(plan.transaction) {
            return Err(TaskError::InvalidPlan);
        }
        thread.lifecycle = ThreadLifecycle::Runnable;
        thread.pending_transaction = None;
        Ok(plan.thread)
    }

    pub fn rollback_clone_thread(&self, plan: CloneThreadPlan) -> Result<(), TaskError> {
        let mut state = self.lock();
        let thread = Self::thread(&state, plan.thread)?;
        if thread.lifecycle != ThreadLifecycle::Starting || thread.pending_transaction != Some(plan.transaction) {
            return Err(TaskError::InvalidPlan);
        }
        let process = thread.process;
        self.unregister_interrupt(plan.thread);
        Self::process_mut(&mut state, process)?.threads.remove(&plan.thread);
        Self::release_thread(&mut state, plan.thread)
    }

    pub fn begin_fork_process(&self, source: ThreadId) -> Result<ForkProcessPlan, TaskError> {
        let mut state = self.lock();
        let parent = Self::thread(&state, source)?.process;
        let parent_state = Self::process(&state, parent)?;
        if parent_state.lifecycle != ProcessLifecycle::Running {
            return Err(TaskError::InvalidLifecycle);
        }
        let credentials = parent_state.credentials.clone();
        let limits = parent_state.limits.clone();
        let process_signals = parent_state.signals.fork_copy();
        let arguments = parent_state.arguments.clone();
        let namespaces = parent_state.namespaces;
        let dumpable = parent_state.dumpable;
        let oom_score_adj = parent_state.oom_score_adj;
        let timer_slack = parent_state.timer_slack;
        let thp_disabled = parent_state.thp_disabled;
        let mce_policy = parent_state.mce_policy;
        let personality = parent_state.personality;
        let session = parent_state.session;
        let process_group = parent_state.process_group;
        let terminal_detached = parent_state.terminal_detached;
        let thread_signals = Self::thread(&state, source)?.signals.fork_copy();
        let thread_name = Self::thread(&state, source)?.name;
        let thread_affinity = Self::thread(&state, source)?.affinity;
        let thread_schedule = Self::thread(&state, source)?.schedule.fork_copy();
        let (process, thread) = Self::allocate_leader(&mut state)?;
        let transaction = Self::next_transaction(&mut state);
        Self::install_thread(
            &mut state,
            thread,
            Thread {
                process,
                lifecycle: ThreadLifecycle::Starting,
                cancellation_pending: false,
                signal_pending: false,
                pending_transaction: Some(transaction),
                signals: thread_signals,
                robust_list: None,
                clear_tid: None,
                name: thread_name,
                affinity: thread_affinity,
                schedule: thread_schedule,
            },
        )?;
        let mut threads = BTreeSet::new();
        threads.insert(thread);
        Self::install_process(
            &mut state,
            process,
            Process {
                control_epoch: 0,
                lifecycle: ProcessLifecycle::Starting,
                parent: Some(parent),
                children: BTreeSet::new(),
                threads,
                leader: thread,
                session,
                process_group,
                terminal_detached,
                child_class: ChildClass::Standard,
                execed: false,
                arguments,
                name: thread_name,
                credentials,
                limits,
                exit_status: None,
                pending_transaction: Some(transaction),
                signals: process_signals,
                namespaces,
                parent_death_signal: 0,
                child_subreaper: false,
                cpu_usage: crate::CpuUsage::default(),
                cpu_account: Arc::new(crate::CpuAccount::default()),
                dumpable,
                oom_score_adj,
                timer_slack,
                thp_disabled,
                mce_policy,
                personality,
            },
        )?;
        Ok(ForkProcessPlan {
            parent,
            process,
            thread,
            transaction,
        })
    }

    pub fn commit_fork_process(&self, plan: ForkProcessPlan) -> Result<(ProcessId, ThreadId), TaskError> {
        self.commit_fork(&plan, None)
    }

    pub fn commit_fork_interrupt(
        &self,
        plan: &ForkProcessPlan,
        sink: Arc<dyn crate::InterruptSink>,
    ) -> Result<(ProcessId, ThreadId), TaskError> {
        self.commit_fork(plan, Some(sink))
    }

    fn commit_fork(
        &self,
        plan: &ForkProcessPlan,
        sink: Option<Arc<dyn crate::InterruptSink>>,
    ) -> Result<(ProcessId, ThreadId), TaskError> {
        let mut state = self.lock();
        Self::validate_fork_plan(&state, plan)?;
        let interrupted = sink
            .as_ref()
            .map(|_| Self::interrupt_pending(&state, plan.thread))
            .transpose()?;
        Self::process_mut(&mut state, plan.process)?.lifecycle = ProcessLifecycle::Running;
        Self::process_mut(&mut state, plan.process)?.pending_transaction = None;
        let thread = Self::thread_mut(&mut state, plan.thread)?;
        thread.lifecycle = ThreadLifecycle::Runnable;
        thread.pending_transaction = None;
        Self::process_mut(&mut state, plan.parent)?
            .children
            .insert(plan.process);
        let group = Self::process(&state, plan.process)?.process_group;
        Self::process_group_mut(&mut state, group)?.members.insert(plan.process);
        let orphaned = Self::refresh_orphaned_groups(&mut state)?;
        if let Some(sink) = &sink {
            self.interrupts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(plan.thread, Arc::downgrade(sink));
            sink.set_interrupted(interrupted.expect("interrupt state computed for sink"));
        }
        drop(state);
        self.publish_orphaned(orphaned);
        Ok((plan.process, plan.thread))
    }

    pub fn rollback_fork_process(&self, plan: ForkProcessPlan) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::validate_fork_plan(&state, &plan)?;
        Self::release_thread(&mut state, plan.thread)?;
        Self::release_process(&mut state, plan.process)
    }

    pub fn exit_thread(&self, thread: ThreadId, group_status: ExitStatus) -> Result<(), TaskError> {
        let mut state = self.lock();
        let process = Self::thread(&state, thread)?.process;
        if Self::process(&state, process)?.pending_transaction.is_some()
            || Self::thread(&state, thread)?.pending_transaction.is_some()
        {
            return Err(TaskError::InvalidLifecycle);
        }
        if Some(process) == state.init && Self::process(&state, process)?.threads.len() == 1 {
            return Err(TaskError::InitExited);
        }
        self.unregister_interrupt(thread);
        Self::release_live_thread(&mut state, process, thread)?;
        if Self::process(&state, process)?.threads.is_empty() {
            let orphaned = Self::make_zombie(&mut state, process, group_status, self.max_pending_signals)?;
            self.child_ready.notify_all();
            drop(state);
            self.publish_orphaned(orphaned);
            self.traces.exit(process);
            self.signals.activity.notify(crate::SignalActivityKind::Ordinary, None);
        }
        Ok(())
    }

    pub fn exit_process(&self, process: ProcessId, status: ExitStatus) -> Result<(), TaskError> {
        let mut state = self.lock();
        if Some(process) == state.init {
            return Err(TaskError::InitExited);
        }
        if !matches!(
            Self::process(&state, process)?.lifecycle,
            ProcessLifecycle::Running | ProcessLifecycle::Stopped | ProcessLifecycle::Exiting
        ) {
            return Err(TaskError::InvalidLifecycle);
        }
        Self::process_mut(&mut state, process)?.lifecycle = ProcessLifecycle::Exiting;
        let threads: Vec<_> = Self::process(&state, process)?.threads.iter().copied().collect();
        for thread in threads {
            self.unregister_interrupt(thread);
            Self::release_thread(&mut state, thread)?;
        }
        Self::process_mut(&mut state, process)?.threads.clear();
        let result = Self::make_zombie(&mut state, process, status, self.max_pending_signals);
        if result.is_ok() {
            self.child_ready.notify_all();
        }
        drop(state);
        if let Ok(orphaned) = &result {
            self.publish_orphaned(orphaned.clone());
            self.traces.exit(process);
        }
        self.signals.activity.notify(crate::SignalActivityKind::Ordinary, None);
        result.map(|_| ())
    }
}
