//! Engine runtime state and cross-domain integration.

#![forbid(unsafe_code)]

mod aio;

mod assembly;
mod atomic_read;
mod checkpoint;
mod epoll;
mod event;
mod filesystem;
mod fork;
mod memfd;
mod memory;
mod network;
mod path_host;
mod pidfd;
#[path = "process/capability.rs"]
mod process_capability;
#[path = "process/identity.rs"]
mod process_identity;
#[path = "process/itimer.rs"]
mod process_itimer;
#[path = "process/pidfd.rs"]
mod process_pidfd;
#[path = "process/syscalls.rs"]
mod process_syscalls;
mod procfs;
mod ptrace;
mod seccomp;
mod signal;
#[path = "terminal.rs"]
mod terminal_signals;
mod unix_path;
mod working_directory;
pub use pidfd::{ProcessHandle, ProcessHandleError, ProcessHandleRegistry};
pub use ptrace::{PtraceCatalog, PtracePort, RuntimeSafepoint, TraceBoundary, TraceExchange, TraceWake};
mod namespace_handle;
pub use namespace_handle::{NamespaceHandle, NamespaceHandleError, NamespaceHandleRegistry};
mod architecture_thread;
mod descriptor;
mod exec;
mod execution;
#[path = "lifecycle_assembly.rs"]
mod exit_assembly;
#[path = "exit_runtime.rs"]
mod exit_runtime;
mod futex_port;
#[cfg(test)]
#[path = "futex/port_test.rs"]
mod futex_port_tests;
#[cfg(test)]
#[path = "futex/syscall_test.rs"]
mod futex_syscall_tests;
mod ipc;
mod loader;
mod lock_exit;
#[path = "process/control.rs"]
mod process_control;
#[path = "process/dispatch.rs"]
mod process_dispatch;
#[path = "process/exec.rs"]
mod process_exec;
#[path = "process/fork.rs"]
mod process_fork;
#[path = "process/image.rs"]
mod process_image;
#[path = "process/namespace.rs"]
mod process_namespace;
#[path = "process/prctl.rs"]
mod process_prctl;
#[path = "process/retire.rs"]
mod process_retire;
#[path = "process/schedule.rs"]
mod process_schedule;
#[path = "process/sysinfo.rs"]
mod process_sysinfo;
#[path = "process/time.rs"]
mod process_time;
#[path = "process/timer.rs"]
mod process_timer;
#[path = "process/uname.rs"]
mod process_uname;
mod system;
pub use hl_ipc::{MqLimits, MqNamespace};
pub use system::{ResourceSnapshot, SystemAuthority};
#[path = "process/wait.rs"]
mod process_wait;
mod robust;
mod runtime_socket;
mod syscall_router;
mod task_exec;
mod thread;
pub use descriptor::Exit as DescriptorExit;
pub use descriptor::{Exec as DescriptorExec, ImageSlot as DescriptorImageSlot, PreparedDescriptorExec};
pub use exec::{CurrentDescriptorTable, VfsImageSource, VfsSourceFactory, VfsSourceFactory as VfsImageSourceFactory};
pub use exec::{
    Role as ExecRole, Runtime as ExecRuntime, RuntimeDependencies as ExecRuntimeDependencies,
    RuntimeDependenciesBuilder as ExecRuntimeDependenciesBuilder,
};
pub use execution::RuntimeExecutionMemory;
pub use execution::{RuntimeExecutionLoop, RuntimeExecutionOutcome};
pub use execution::{RuntimeSyscallTrap, RuntimeTrapOutcome, dispatch_runtime_syscall};
pub use exit_assembly::ExitRuntimeDependencies;
pub use exit_runtime::{
    ExitParticipant, ExitRuntime, ExitRuntimeError, PreparedExitParticipant, RegistryExitFinalizer, TaskExitFinalizer,
};
pub use futex_port::{FutexInterruptionSource, SafeRuntimeFutex};
pub use hl_vfs::{
    Procfs, ProcfsAddressSpaceView, ProcfsCpuModel, ProcfsCpuTicks, ProcfsError, ProcfsMemoryRegionLabel, ProcfsMemoryRegionView,
    ProcfsMemoryView, ProcfsNodeKind,
};
pub use hl_vfs::{ProcfsInternetSocketView, ProcfsNetworkInterfaceView, ProcfsNetworkView, ProcfsUnixSocketView};
pub use ipc::ExitHandler as IpcExitHandler;
pub use loader::{
    ExecLoadContext, ExecutionImageBuilder, Image as LoaderExecImage, Participant as LoaderExecParticipant,
    PreparedLoaderExec, SourceFactory, SpaceFactory,
};
pub use lock_exit::VfsLockExit;
pub use memory::Exit as MemoryExit;
pub use process_exec::{ExecKey, ExecQueue, PreparedExec, RuntimeExecError, RuntimeExecPort};
pub use process_fork::{
    RejectingForkPort, RuntimeForkError, RuntimeForkPort, RuntimeForkResult, Trap as ProcessForkTrap,
};
pub use process_image::{
    Image as ProcessImage, PreparedExecParticipant, PreparedProcessImage, PreparedRuntimeExec, RejectingExecPort,
    RuntimeExecParticipant, SafeRuntimeExec,
};
pub use process_itimer::{AlarmRegistry, AlarmScheduler};
pub use process_schedule::RuntimeYieldPort;
pub use process_syscalls::{RuntimeProcessSyscalls, RuntimeReapPort};
pub use process_time::{
    CpuClockPort, ResourceUsageScope, RobustExitPort, RuntimeFutexPort, RuntimeSleepOutcome, RuntimeSleepPort,
};
pub use process_timer::TimerRegistry;
pub use procfs::{
    CpuPolicy as ProcfsCpuPolicy, CpuPort as ProcfsCpuPort, DescriptorTarget as ProcfsDescriptorTarget, MemoryPort as ProcfsMemoryPort,
    MountPort as ProcfsMountPort, NetworkPort as ProcfsNetworkPort, StatMetrics as ProcfsStatMetrics,
    ResourcePort as ProcfsResourcePort, StatPort as ProcfsStatPort, TaskProcfs,
};
pub use robust::{ExitHandler as RobustExitHandler, Wake as RobustWake};
pub use runtime_socket::RuntimeSocketRegistry;
pub(crate) use runtime_socket::{RuntimeSocket, RuntimeSocketKind};
pub use signal::{FramePort, PreparedFramePublication};
pub use syscall_router::{
    RouterDependencies, RuntimeSyscallRouter, RuntimeTerminal, SignalBoundaryOutcome, SignalBoundaryPort, SyscallRecord,
};
pub use task_exec::{TaskExecParticipant, linux_comm};
pub use thread::{
    CloneError as ThreadCloneError, ClonePlan as ThreadClonePlan, CloneRuntime as ThreadCloneRuntime,
    CloneTrap as ThreadCloneTrap, CloneTrapPort as ThreadCloneTrapPort, ContextPort as ThreadContextPort,
    PreparedThread, RuntimeError as RuntimeThreadError, RuntimePort as RuntimeThreadPort,
};

pub use assembly::{
    AssemblyCheckpointError, ExecSlot, HostCapacityPlan, RuntimeAssembly, RuntimeAssemblyConfig, RuntimeAssemblyError,
    RuntimeDomain,
};
pub use checkpoint::ExecutionCheckpointParticipant;
pub use checkpoint::{
    BindingRestore as EventBindingRestore, Catalog as CheckpointEventCatalog, CheckpointCodec as EventCheckpointCodec,
    DescriptorRebind as DescriptorEventRebind, DescriptorReference, EventParticipant as EventCheckpointParticipant,
    EventResourceRegistry, ObjectBindings as EventObjectBindings, ResourceRestore as EventResourceRestore,
    WireCodec as EventWireCodec,
};
pub use checkpoint::{
    DescriptorCheckpointParticipant, DescriptorTable as CheckpointDescriptorTable, FileObjectCatalog,
    FileObjectCheckpoint, ObjectCatalog as DescriptorObjectCatalog,
};
pub use checkpoint::{
    Error as CheckpointError, Participant as CheckpointParticipant, Phase as CheckpointPhase, Role as CheckpointRole,
    RuntimeCheckpointCoordinator,
};
pub use checkpoint::{
    IPC_CHECKPOINT_BYTES_MAXIMUM, IpcCatalog as CheckpointIpcCatalog, IpcCheckpointCodec, IpcCheckpointParticipant,
    OpenPipe as IpcOpenPipe, PipeBindings as IpcPipeBindings, PipePublication as IpcPipePublication,
    PipeRegistry as IpcPipeRegistry, PortableIpcCodec, RegistryError as IpcRegistryError,
    ResourceRebind as IpcResourceRebind,
};
pub use checkpoint::{
    Memory as CheckpointMemory, MemoryCheckpointCodec, MemoryCheckpointParticipant, MemoryResourceRestore,
    MemoryResourceTransaction, MemoryState as CheckpointMemoryState, PortableMemoryCodec,
};
pub use checkpoint::{
    NETWORK_CHECKPOINT_BYTES_MAXIMUM, NetworkCatalog as CheckpointNetworkCatalog, NetworkCheckpointCodec,
    NetworkCheckpointHost, NetworkCheckpointParticipant, NetworkObjectBindings, PortableNetworkCodec,
    ReconnectedSocket,
};
pub use checkpoint::{
    PROVIDER_CHECKPOINT_BYTES_MAXIMUM, PortableProviderCodec, ProviderCheckpointCodec, ProviderCheckpointParticipant,
    ProviderLease, ProviderNamespace as CheckpointProviderNamespace, ProviderRegistry, ProviderRegistryError,
};
pub use checkpoint::{
    PortableTaskCodec, TaskCheckpointCodec, TaskCheckpointParticipant, TaskRegistry as CheckpointTaskRegistry,
};
pub use checkpoint::{TaskBindingError, TaskBindingRestore, TaskResourceCatalog};
pub use epoll::{
    Control, Control as EpollControl, ControlError, ControlError as EpollControlError, DescriptorTableId,
    RuntimeDescriptorTable,
};
pub use epoll::{
    EdgeSnapshot, EdgeSnapshot as EpollEdgeSnapshot, GraphError, GraphError as EpollGraphError, GraphSnapshot,
    GraphSnapshot as EpollGraphSnapshot, OwnershipGraph, OwnershipGraph as EpollOwnershipGraph,
};
pub use event::RuntimeEventSyscalls;
pub use event::{
    OperationError, OperationError as EventOperationError, OperationRegistry,
    OperationRegistry as EventOperationRegistry,
};
pub use event::{SignalEventSource, SourceError, SourceError as EventSourceError, TimerEventSource, WatchEventSource};
pub use filesystem::{
    AsyncSignalPort, BackingChangePort, DnotifyError, DnotifyPort, FileSizeLimitPort, PipeCancellationPort, PipeSignalPort,
    RuntimeFilesystemSyscalls,
    RuntimePipeCancellation, SocketIoctlPort, VectorDirection, VectorError, VectorPosition, VectorRequest,
    VectorTerminal,
};
pub use fork::DescriptorForkParticipant;
pub use fork::EventForkParticipant;
pub use fork::ExecutionForkParticipant;
pub use fork::NetworkForkParticipant;
pub use fork::ProviderForkParticipant;
pub use fork::TaskForkParticipant;
pub use fork::{
    ArtifactExchange as ForkArtifactExchange, Cancellation as ForkCancellation, Context as ForkContext,
    Coordinator as ForkCoordinator, Error as ForkError, Event as ForkEvent, Outcome as ForkOutcome,
    Participant as ForkParticipant, ParticipantRole as ForkParticipantRole, Phase as ForkPhase,
};
pub use fork::{
    ChildResourceCatalog as ForkChildResourceCatalog, ChildResourceError as ForkChildResourceError,
    ChildResources as ForkChildResources, PreparedChildResources, ReadyChildResources,
};
pub use fork::{IpcForkChild, IpcForkParticipant};
pub use fork::{MemoryChildMapping, MemoryForkHost, MemoryForkParticipant, PrivateFutexReset};
pub use fork::{Runtime as ForkRuntime, RuntimeDependencies as ForkRuntimeDependencies};
pub use fork::{VforkError, VforkParentToken, VforkWake};
pub use hl_event::{EventCatalog, EventResourceKey, TimerClockSource};
pub use hl_ipc::IpcCatalog;
pub use hl_terminal::{
    Bindings as TerminalBindings, Catalog as TerminalCatalog, Description as TerminalDescription,
    Endpoint as TerminalEndpoint, ForegroundGroup as TerminalForegroundGroup, PairId as TerminalId,
    Signal as TerminalSignal, SignalSink as TerminalSignalSink,
};
pub use hl_vfs::{
    Access, AccessIdentity, AdvisoryLockCoordinator, BUILTIN_DEVICES, BuiltinDescription, Capabilities, DeviceEntropy,
    DeviceId, DeviceKind, DeviceOpenCapability, FileIdentity, FileKind, FileMetadata, FileTimestamp, FilesystemKind,
    FilesystemStats, GuestName, GuestPath, GuestPathBytes, Identity, Kind, LockCancellation, LockError, LockRange,
    Metadata, MountError, MountKind, MountNamespace, MountRoute, MountSourceId, NodeHandle, NodeKind, OpenDirectory,
    OpenIntent, Permissions, ProcessLockOwner, RangeLockKind, RangeLockRequest, RangeWhence, ReadOnlyPaths,
    ResolveConstraints, ResolveError, ResolveHostError, ResolveRequest, Resolver, Timestamp, VfsHost, XattrFlags,
    XattrName,
};
pub use ipc::BlockingWait;
pub use ipc::MemoryLifecycle;
pub use ipc::RuntimeIpcLifecycle;
pub use ipc::RuntimeIpcSyscalls;
pub use ipc::{
    CommittedBindingSet, ForkBinding, MappingError, MemoryBinding, MemoryBinding as SharedMemoryBinding,
    MemoryMappings, MemoryPort, PreparedBindingSet,
};
pub use ipc::{CommittedFork as CommittedRuntimeSharedMemoryFork, PreparedFork as PreparedRuntimeSharedMemoryFork};
pub use ipc::{EmptyIpcExec, ExecParticipant, ExecParticipant as IpcExecParticipant};
pub use memfd::{MemfdBindings, Registry as MemfdRegistry};
pub use memory::RuntimeMemorySyscalls;
pub use memory::{BRK_BACKING_IDENTITY, BrkRegion, BrkSnapshot};
pub use memory::{DescriptorMappingSource, RuntimeMemoryError, RuntimeMemoryHost};
pub use network::RuntimeNetworkSyscalls;
pub use network::{
    AcceptedSocket, CreatedSocket, DescriptorTransfer, HostControl, HostImport, HostReceive, HostSend, HostSendResult,
    ImportedDescription, ImportedTransfer, PreparedTransfer, ReceivedDatagram, RuntimeNetworkError, RuntimeNetworkHost,
    SocketCredentials, SocketIoctl, TransferCommitError, TransferPublication,
};
pub use network::{SafeNetworkWait, SocketWait};
pub use path_host::{
    DirectoryBaseLease, ExecutablePath, PreparedPathMutation, PreparedPathOpen, PreparedXattrMutation,
    ResolvedMetadata, ResolvedPathLease, RuntimePathError, RuntimePathHost, RuntimeXattrMutation,
};
pub use seccomp::RuntimeSyscalls as RuntimeSeccompSyscalls;
pub use seccomp::{
    Control as SeccompControl, ControlError as SeccompControlError, InstallTransaction as SeccompInstallTransaction,
    ListenerRequest as SeccompListenerRequest, PolicySnapshot as SeccompPolicySnapshot, PrctlPort as SeccompPrctlPort,
    RestoreTransaction as SeccompRestoreTransaction,
};
pub use signal::TaskSignalQueue;
pub use terminal_signals::TerminalSignals;
pub use unix_path::{PreparedUnixSocketPathBind, PreparedUnixSocketPathUnlink, UnixSocketPathPort};
pub use working_directory::{DirectorySnapshot, WorkingDirectory};

#[cfg(test)]
pub(crate) use ipc::test_support;
#[cfg(test)]
#[path = "thread/architecture_test.rs"]
mod architecture_thread_tests;
#[cfg(test)]
#[path = "process/image_test.rs"]
mod process_image_tests;
#[cfg(test)]
#[path = "router_test.rs"]
mod syscall_router_tests;
pub use aio::RuntimeAioSyscalls;
pub use hl_aio::Catalog as AioCatalog;
mod fs_context;
pub use fs_context::FsContext;
