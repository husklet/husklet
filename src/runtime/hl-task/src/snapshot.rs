//! Value-only views of registry state produced for checkpoint and observation.
use crate::{
    ChildClass, ChildEvent, CpuUsage, ExitStatus, Limit, ProcessCredentials, ProcessGroupId, ProcessId,
    ProcessLifecycle, RegistryConfig, Resource, SessionId, SignalProcessSnapshot, SignalThreadSnapshot, ThreadId,
    ThreadLifecycle, WaitEvent,
};
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
    pub terminal_detached: bool,
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

/// Coherent current-process state used by simple Linux identity and control
/// operations without cloning process topology or signal queues.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessObservation {
    pub parent: Option<ProcessId>,
    pub credentials: ProcessCredentials,
    pub parent_death_signal: u32,
    pub child_subreaper: bool,
    pub dumpable: bool,
    pub timer_slack: u64,
    pub thp_disabled: bool,
    pub mce_policy: u32,
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
