//! Bounded Linux process/thread identity, lifecycle, and wait semantics.
//!
//! Guest execution, host processes, signal frames, futexes, and syscall
//! marshalling are deliberately outside this foundation.

#![forbid(unsafe_code)]

mod affinity;
mod child_wait;
mod credentials;
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
mod snapshot;
mod trace;
pub(crate) use registry::Activity as RegistryActivity;

pub use affinity::{CpuAffinity, CpuTopology};
pub use child_wait::{
    ChildClass, ChildClassSelector, ChildEvent, ChildEventKind, ChildSelector, ChildWaitOptions, ChildWaitResult,
    PreparedChildWait, PreparedWaitSelection, WaitEvent, WaitSelector,
};
pub use credentials::{CapabilitySets, ProcessCredentials, SetIdAuthority};
pub use fork_model::{
    FORK_WIRE_VERSION, ForkCloneFlags, ForkEntityId, ForkModelError, ForkRequest, ForkWireSnapshot,
    MAX_FORK_PARTICIPANTS,
};
pub use identity::{ProcessGroupId, ProcessId, SessionId, ThreadId};
pub use model::{
    CancellationEvent, CloneThreadPlan, CpuAccount, CpuUsage, Denial, ExitStatus, ForkProcessPlan, ProcessLifecycle,
    RegistryConfig, SignalPendingEvent, TaskError, ThreadLifecycle,
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
    InitReservation, PreparedTaskExec, ProcessCheckpointReference, TASK_CHECKPOINT_VERSION, TaskExternalCheckpoint,
    TaskExternalRestore, TaskRegistry, TaskRegistryImage, TaskResourceKey, ThreadCheckpointReference,
};
pub use resource::{Limit, ProcessLimits, Resource};
pub use robust_list::{ROBUST_LIST_HEAD_SIZE, RobustExitCleanup, RobustListRegistration};
pub use schedule::SchedulingProfile;
pub use signal::{
    AlternateStack, DeliveryAction, PendingTarget, PreparedForcedDelivery, PreparedSignalWait, SIGNAL_FRAME_MAXIMUM,
    SignalAction, SignalDisposition, SignalExecPlan, SignalForkPlan, SignalFrameScope, SignalInfo, SignalMask,
    SignalNumber, SignalProcessSnapshot, SignalQueueError, SignalThreadSnapshot, SignalThreadTarget,
};
pub use snapshot::{
    ProcessGroupSnapshot, ProcessObservation, ProcessSnapshot, RegistrySnapshot, SessionSnapshot, ThreadSnapshot,
};
pub use trace::{
    LinkFault, TraceDenial, TraceError, TraceEvent, TraceImage, TraceLinkId, TracePermission, TraceResume,
    TraceSnapshot, TraceStop, TraceSubject, TraceWait,
};
