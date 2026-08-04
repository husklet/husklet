//! Bounded Linux process/thread identity, lifecycle, and wait semantics.
//!
//! Guest execution, host processes, signal frames, futexes, and syscall
//! marshalling are deliberately outside this foundation.

#![forbid(unsafe_code)]

mod affinity;
mod fork_model;
mod identity;
mod model;
mod namespace;
mod port;
mod registry;
mod resource;
mod robust_list;
mod schedule;
mod signal;
mod trace;
pub(crate) use registry::Activity as RegistryActivity;

pub use affinity::{CpuAffinity, CpuTopology};
pub use fork_model::{
    FORK_WIRE_VERSION, ForkCloneFlags, ForkEntityId, ForkModelError, ForkRequest, ForkWireSnapshot,
    MAX_FORK_PARTICIPANTS,
};
pub use identity::{ProcessGroupId, ProcessId, SessionId, ThreadId};
pub use model::{
    CancellationEvent, CapabilitySets, ChildClass, ChildClassSelector, ChildEvent, ChildEventKind, ChildSelector,
    ChildWaitOptions, ChildWaitResult, CloneThreadPlan, CpuUsage, ExitStatus, ForkProcessPlan, PreparedChildWait,
    PreparedWaitSelection, ProcessCredentials, ProcessGroupSnapshot, ProcessLifecycle, ProcessSnapshot, RegistryConfig,
    RegistrySnapshot, SessionSnapshot, SetIdAuthority, SignalPendingEvent, TaskError, ThreadLifecycle, ThreadSnapshot,
    WaitEvent, WaitSelector,
};
pub use namespace::{
    IdMap, IdRange, MAX_ID_RANGES, MapError, NamespaceId, NamespaceKind, NamespaceSet, SetgroupsState,
    UTS_NAME_MAXIMUM, UserNamespace, UtsIdentity,
};
pub use port::{
    CancellationSink, ForegroundGroupEvent, InterruptSink, PreparedTerminalTransition, ProcessControlAction,
    SignalActivityEvent, SignalActivityKind, SignalActivitySubscription, SignalActivityWake, SignalPendingSink,
    TerminalControl, TerminalTransition, TerminalTransitionEffects,
};
pub use registry::{
    PreparedTaskExec, ProcessCheckpointReference, TASK_CHECKPOINT_VERSION, TaskExternalCheckpoint, TaskExternalRestore,
    TaskRegistry, TaskRegistryImage, TaskResourceKey, ThreadCheckpointReference,
};
pub use resource::{Limit, ProcessLimits, Resource};
pub use robust_list::{ROBUST_LIST_HEAD_SIZE, RobustExitCleanup, RobustListRegistration};
pub use schedule::SchedulingProfile;
pub use signal::{
    AlternateStack, DeliveryAction, PendingTarget, PreparedForcedDelivery, PreparedSignalWait, SIGNAL_FRAME_MAXIMUM,
    SignalAction, SignalDisposition, SignalExecPlan, SignalForkPlan, SignalFrameScope, SignalInfo, SignalMask,
    SignalNumber, SignalProcessSnapshot, SignalQueueError, SignalThreadSnapshot,
};
pub use trace::{
    TraceError, TraceEvent, TraceImage, TraceLinkId, TracePermission, TraceResume, TraceSnapshot, TraceStop, TraceWait,
};
