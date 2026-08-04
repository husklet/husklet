use std::{error::Error, fmt};

use crate::{
    CancellationSink, Limit, ProcessGroupId, ProcessId, Resource, SessionId, SignalNumber, SignalPendingSink, ThreadId,
};
use crate::{SignalProcessSnapshot, SignalThreadSnapshot};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistryConfig {
    pub max_processes: usize,
    pub max_threads: usize,
    pub max_groups: usize,
    pub max_pending_signals: usize,
    pub online_cpus: usize,
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            max_processes: 1024,
            max_threads: 4096,
            max_groups: 32,
            max_pending_signals: 1024,
            online_cpus: 1,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessCredentials {
    pub real_user: u32,
    pub effective_user: u32,
    pub saved_user: u32,
    pub filesystem_user: u32,
    pub real_group: u32,
    pub effective_group: u32,
    pub saved_group: u32,
    pub filesystem_group: u32,
    pub capabilities: CapabilitySets,
    pub capability_bounding: u64,
    pub secure_bits: u32,
    pub keep_capabilities: bool,
    pub no_new_privileges: bool,
    /// Set-id authority is distinct from the guest-visible capability persona.
    pub setid_permitted: bool,
    /// Effective SETUID/SETGID authority, re-raised only from `setid_permitted`.
    pub setid_effective: bool,
    groups: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilitySets {
    pub effective: u64,
    pub permitted: u64,
    pub inheritable: u64,
    pub ambient: u64,
}

impl CapabilitySets {
    pub const SUPPORTED: u64 = (1_u64 << 41) - 1;
    pub const CONTAINER: u64 = 0x0000_0000_a804_25fb;
    pub const KILL: u64 = 1 << 5;
    pub const CHANGE_OWNER: u64 = 1;
    pub const DAC_OVERRIDE: u64 = 1 << 1;
    pub const DAC_READ_SEARCH: u64 = 1 << 2;
    pub const OWNER_OVERRIDE: u64 = 1 << 3;
    pub const PRESERVE_SET_ID: u64 = 1 << 4;
    pub const SET_GROUP: u64 = 1 << 6;
    pub const SET_USER: u64 = 1 << 7;
    pub const SYS_ADMIN: u64 = 1 << 21;

    #[must_use]
    pub const fn initial(user: u32) -> Self {
        let permitted = if user == 0 { Self::CONTAINER } else { 0 };
        Self {
            effective: permitted,
            permitted,
            inheritable: 0,
            ambient: 0,
        }
    }
}

impl ProcessCredentials {
    pub fn new(user: u32, group: u32, groups: &[u32], max_groups: usize) -> Result<Self, TaskError> {
        if groups.len() > max_groups {
            return Err(TaskError::GroupLimit);
        }
        Ok(Self {
            real_user: user,
            effective_user: user,
            saved_user: user,
            filesystem_user: user,
            real_group: group,
            effective_group: group,
            saved_group: group,
            filesystem_group: group,
            capabilities: CapabilitySets::initial(user),
            capability_bounding: if user == 0 { CapabilitySets::CONTAINER } else { 0 },
            secure_bits: 0,
            keep_capabilities: false,
            no_new_privileges: false,
            setid_permitted: user == 0,
            setid_effective: user == 0,
            groups: groups.to_vec(),
        })
    }

    #[must_use]
    pub fn supplementary_groups(&self) -> &[u32] {
        &self.groups
    }

    #[must_use]
    pub const fn has_capability(&self, capability: u64) -> bool {
        self.capabilities.effective & capability != 0
    }

    #[must_use]
    pub const fn may_setid(&self) -> bool {
        self.setid_effective
    }

    pub fn refresh_setid(&mut self) {
        if self.effective_user == 0 {
            self.setid_permitted = true;
            self.setid_effective = true;
            return;
        }
        self.setid_effective = false;
        if self.real_user != 0 && self.saved_user != 0 && !self.keep_capabilities {
            self.setid_permitted = false;
        }
    }

    pub fn raise_setid(&mut self) {
        self.setid_effective = self.setid_permitted;
    }

    pub fn reset_setid_for_exec(&mut self) {
        self.keep_capabilities = false;
        self.setid_permitted = self.effective_user == 0;
        self.setid_effective = self.setid_permitted;
    }

    pub fn replace_groups(&mut self, groups: &[u32], max_groups: usize) -> Result<(), TaskError> {
        if groups.len() > max_groups {
            return Err(TaskError::GroupLimit);
        }
        self.groups = groups.to_vec();
        Ok(())
    }
}

#[cfg(test)]
mod credential_test {
    use super::*;

    #[test]
    fn nonroot_cannot_bootstrap_setid() {
        let mut credentials = ProcessCredentials::new(501, 20, &[], 8).unwrap();
        credentials.capabilities.effective = CapabilitySets::CONTAINER;
        credentials.capabilities.permitted = CapabilitySets::CONTAINER;
        credentials.keep_capabilities = true;
        credentials.raise_setid();
        assert!(!credentials.may_setid());
    }

    #[test]
    fn keepcaps_preserves_setid_permission() {
        let mut credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        credentials.keep_capabilities = true;
        credentials.real_user = 501;
        credentials.effective_user = 501;
        credentials.saved_user = 501;
        credentials.refresh_setid();
        assert!(credentials.setid_permitted);
        assert!(!credentials.setid_effective);
        credentials.raise_setid();
        assert!(credentials.may_setid());
    }

    #[test]
    fn ordinary_drop_discards_setid() {
        let mut credentials = ProcessCredentials::new(0, 0, &[], 8).unwrap();
        credentials.real_user = 501;
        credentials.effective_user = 501;
        credentials.saved_user = 501;
        credentials.refresh_setid();
        assert!(!credentials.setid_permitted);
        credentials.raise_setid();
        assert!(!credentials.may_setid());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessLifecycle {
    Starting,
    Running,
    Stopped,
    Exiting,
    Zombie,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadLifecycle {
    Starting,
    Runnable,
    Blocked,
    Exiting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitStatus {
    Code(u8),
    Signal { signal: u8, dumped_core: bool },
}

impl ExitStatus {
    #[must_use]
    pub const fn wait_status(self) -> u32 {
        match self {
            Self::Code(code) => (code as u32) << 8,
            Self::Signal { signal, dumped_core } => signal as u32 | if dumped_core { 0x80 } else { 0 },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitSelector {
    Any,
    Process(ProcessId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildClass {
    Standard,
    Clone,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildClassSelector {
    Standard,
    Clone,
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildSelector {
    Any,
    Process(ProcessId),
    ProcessGroup(ProcessGroupId),
    SameProcessGroup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildWaitOptions {
    pub no_hang: bool,
    pub report_stopped: bool,
    pub report_continued: bool,
    pub keep_waitable: bool,
    pub class: ChildClassSelector,
}

impl Default for ChildWaitOptions {
    fn default() -> Self {
        Self {
            no_hang: false,
            report_stopped: false,
            report_continued: false,
            keep_waitable: false,
            class: ChildClassSelector::Standard,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildEventKind {
    Exited(ExitStatus),
    Stopped(SignalNumber),
    Continued,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CpuUsage {
    pub self_nanoseconds: u64,
    pub children_nanoseconds: u64,
}

impl CpuUsage {
    #[must_use]
    pub const fn total_nanoseconds(self) -> u64 {
        self.self_nanoseconds.saturating_add(self.children_nanoseconds)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildEvent {
    pub parent: ProcessId,
    pub child: ProcessId,
    pub process_group: ProcessGroupId,
    pub class: ChildClass,
    pub kind: ChildEventKind,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildWaitResult {
    Event(ChildEvent),
    NoChange,
    WouldBlock,
}

#[must_use = "prepared wait selection must be committed or aborted"]
pub struct PreparedWaitSelection<'registry> {
    pub(crate) registry: &'registry crate::TaskRegistry,
    pub(crate) parent: ProcessId,
    pub(crate) event: ChildEvent,
    pub(crate) keep_waitable: bool,
    pub(crate) sequence: u64,
    pub(crate) finished: bool,
}

impl PreparedWaitSelection<'_> {
    #[must_use]
    pub const fn event(&self) -> ChildEvent {
        self.event
    }

    pub fn usage(&self) -> Result<CpuUsage, TaskError> {
        self.registry.cpu_usage(self.event.child)
    }

    pub fn commit(mut self) -> Result<ChildEvent, TaskError> {
        let result = self
            .registry
            .commit_wait_selection(self.parent, self.event, self.keep_waitable, self.sequence);
        self.finished = true;
        result
    }
}

impl Drop for PreparedWaitSelection<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.registry.release_wait_reservation(self.sequence);
        }
    }
}

pub enum PreparedChildWait<'registry> {
    Selection(PreparedWaitSelection<'registry>),
    NoChange,
    WouldBlock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitEvent {
    pub parent: ProcessId,
    pub child: ProcessId,
    pub status: ExitStatus,
    pub sequence: u64,
}

#[derive(Debug)]
#[must_use = "a clone plan must be committed or rolled back"]
pub struct CloneThreadPlan {
    pub(crate) source: ThreadId,
    pub(crate) thread: ThreadId,
    pub(crate) transaction: u64,
}

impl CloneThreadPlan {
    #[must_use]
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }

    #[must_use]
    pub const fn source(&self) -> ThreadId {
        self.source
    }
}

#[derive(Debug)]
#[must_use = "a fork plan must be committed or rolled back"]
pub struct ForkProcessPlan {
    pub(crate) parent: ProcessId,
    pub(crate) process: ProcessId,
    pub(crate) thread: ThreadId,
    pub(crate) transaction: u64,
}

impl ForkProcessPlan {
    #[must_use]
    pub const fn parent(&self) -> ProcessId {
        self.parent
    }

    #[must_use]
    pub const fn process(&self) -> ProcessId {
        self.process
    }

    #[must_use]
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancellationEvent {
    thread: ThreadId,
}

impl CancellationEvent {
    pub(crate) const fn new(thread: ThreadId) -> Self {
        Self { thread }
    }

    pub fn deliver<S: CancellationSink>(self, sink: &S) -> Result<(), S::Error> {
        sink.request_cancellation(self.thread)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalPendingEvent {
    thread: ThreadId,
    pending: bool,
}

impl SignalPendingEvent {
    pub(crate) const fn new(thread: ThreadId, pending: bool) -> Self {
        Self { thread, pending }
    }

    pub fn deliver<S: SignalPendingSink>(self, sink: &S) -> Result<(), S::Error> {
        sink.pending_changed(self.thread, self.pending)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSnapshot {
    pub id: ProcessId,
    pub generation: u16,
    pub lifecycle: ProcessLifecycle,
    pub parent: Option<ProcessId>,
    pub children: Vec<ProcessId>,
    pub threads: Vec<ThreadId>,
    pub leader: ThreadId,
    pub session: SessionId,
    pub process_group: ProcessGroupId,
    pub child_class: ChildClass,
    pub execed: bool,
    pub arguments: Vec<Vec<u8>>,
    pub name: [u8; 16],
    pub credentials: ProcessCredentials,
    pub limits: Vec<(Resource, Limit)>,
    pub exit_status: Option<ExitStatus>,
    pub signals: SignalProcessSnapshot,
    pub namespaces: crate::NamespaceSet,
    pub parent_death_signal: u32,
    pub child_subreaper: bool,
    pub cpu_usage: CpuUsage,
    pub dumpable: bool,
    pub oom_score_adj: i16,
    pub timer_slack: u64,
    pub thp_disabled: bool,
    pub mce_policy: u32,
    pub personality: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadSnapshot {
    pub id: ThreadId,
    pub generation: u16,
    pub process: ProcessId,
    pub lifecycle: ThreadLifecycle,
    pub cancellation_pending: bool,
    pub signal_pending: bool,
    pub signals: SignalThreadSnapshot,
    pub robust_list: Option<crate::RobustListRegistration>,
    pub clear_tid: Option<u64>,
    pub name: [u8; 16],
    pub affinity: Option<crate::CpuAffinity>,
    pub schedule: crate::SchedulingProfile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrySnapshot {
    pub config: RegistryConfig,
    pub process_generations: Vec<u16>,
    pub thread_generations: Vec<u16>,
    pub session_generations: Vec<u16>,
    pub process_group_generations: Vec<u16>,
    pub init: Option<ProcessId>,
    pub processes: Vec<ProcessSnapshot>,
    pub threads: Vec<ThreadSnapshot>,
    pub wait_events: Vec<WaitEvent>,
    pub child_events: Vec<ChildEvent>,
    pub sessions: Vec<SessionSnapshot>,
    pub process_groups: Vec<ProcessGroupSnapshot>,
    pub next_transaction: u64,
    pub next_wait_sequence: u64,
    pub next_namespace: u64,
    pub user_namespaces: Vec<crate::UserNamespace>,
    pub uts_namespaces: Vec<(crate::NamespaceId, crate::UtsIdentity)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSnapshot {
    pub id: SessionId,
    pub leader: ProcessId,
    pub process_groups: Vec<ProcessGroupId>,
    pub foreground_group: Option<ProcessGroupId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessGroupSnapshot {
    pub id: ProcessGroupId,
    pub session: SessionId,
    pub leader: ProcessId,
    pub members: Vec<ProcessId>,
    pub orphaned: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskError {
    InvalidCapacity,
    ProcessLimit,
    ThreadLimit,
    SignalQueueLimit,
    GroupLimit,
    InvalidProcess,
    InvalidThread,
    InvalidSession,
    InvalidProcessGroup,
    WrongProcess,
    InvalidLifecycle,
    ProcessExeced,
    SessionLeader,
    InvalidPlan,
    InvalidLimit,
    HasChildren,
    NoChildren,
    NotWaitable,
    InitExited,
    InvalidSnapshot,
    PermissionDenied,
}

impl fmt::Display for TaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "task registry failure: {self:?}")
    }
}

impl Error for TaskError {}
