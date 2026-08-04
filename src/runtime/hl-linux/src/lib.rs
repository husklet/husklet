//! Linux syscall personality, ABI marshalling, and errno conversion.

#![forbid(unsafe_code)]

// The Linux personality owns selection of guest ABI geometry. Consumers use
// this re-export without acquiring a direct dependency on the ISA package.
pub use hl_isa::GuestArchitecture;

mod aio;
mod control;
mod errno;
mod event;
mod filesystem;
mod futex;
mod guest_memory;
mod marshalling;
mod memory;
mod mqueue;
mod network;
mod process;
mod ptrace;
mod seccomp;
mod signal;
mod stat_encoding;
mod status;
mod syscall;
mod system_info;
mod sysv;
mod uts;

pub use aio::{
    Abi as AioAbi, ControlBlock as AioControlBlock, Event as AioEvent, IOCB_FLAG_RESFD,
    MarshalError as AioMarshalError, Opcode as AioOpcode, StagedEvents as StagedAioEvents,
};
pub use control::PrctlPlan;
pub use errno::Errno;
pub use event::{
    Abi as EventAbi, CreationFlags, EpollControlPlan, EpollOperation, EpollWaitPlan, Error as EventMarshalError,
    InotifyWatchPlan, StagedEventCopyout, TimerSetPlan,
};
pub use filesystem::Abi as FilesystemAbi;
pub use filesystem::{
    AbiError as FilesystemMarshalError, AccessPlan, OpenAbiPlan, PathOperand, ResolveFlags, StatOutputKind,
    Target as FilesystemTarget,
};
pub use filesystem::{
    DirectoryRecord, FileLock, FsMutationPlan, LockType, StagedFilesystemCopyout, TimestampChange, XattrPlan,
};
pub use filesystem::{STATFS_SIZE, StatfsEncoder};
pub use futex::{
    ClockIdentity, Error as FutexMarshalError, Error as TimeFutexMarshalError, IntervalTimer, StagedTimeCopyout,
    TimeFutexAbi, TimeQueryPlan, TimerEvent, TimerPlan,
};
pub use futex::{Operation as FutexOperation, Plan as FutexPlan, RobustListPlan, WaitVector as FutexWaitVector};
pub use guest_memory::{CopyProgress, GuestAccess, GuestFault, GuestMemory};
pub use marshalling::{
    GuestIovec, GuestMarshaller, IOV_MAXIMUM, IovecPlan, MAX_RW_COUNT, MarshalError, USER_ADDRESS_LIMIT, VectorTransfer,
};
pub use memory::StagedMemoryCopyout;
pub use memory::{
    Abi as MemoryAbi, AbiError as MemoryMarshalError, Advice, AdvicePlan, LockAllPlan, MapSource, MemfdPlan, MmapPlan,
    MremapPlan, MsyncPlan, RangePlan as MemoryRangePlan, UnlockAllPlan,
};
pub use mqueue::{
    MqAbi, MqAttributes, MqError as MqMarshalError, MqEvent, MqNotify, MqReceiveDestination, MqStagedAttributes,
    MqTimespec,
};
pub use network::{
    Abi as NetworkAbi, Error as NetworkMarshalError, GuestMessageHeader, GuestNetworkAddress, GuestSocketOption,
    MessageCopyout, MessageCopyoutResult, MessageImport,
};
pub use process::{
    AFFINITY_BYTES, Abi as ProcessAbi, AffinityMask, ClonePlan, Error as ProcessMarshalError, ExecPlan, IdentityChange,
    ResourceUsage, StagedProcessCopyout, WaitKind, WaitPlan,
};
pub use ptrace::{
    NT_PRSTATUS, Options as PtraceOptions, Plan as PtracePlan, Request as PtraceRequest, Resume as PtraceResume,
};
pub use seccomp::{
    Action, Action as SeccompAction, BpfInstruction, BpfProgram, Data, Data as SeccompData,
    SECCOMP_MAXIMUM_INSTRUCTIONS, VmError, VmError as SeccompVmError,
};
pub use seccomp::{
    Baseline as SeccompBaseline, Decision, Decision as SeccompDecision, FilterInstallFlags, FilterInstallPlan,
    KillScope, KillScope as SeccompKillScope, Mode, Mode as SeccompMode, Policy, Policy as SeccompPolicy, PolicyError,
    PolicyError as SeccompPolicyError, PolicyImage as SeccompPolicyImage, Status as SeccompStatus, TrapPlan,
    TrapPlan as SeccompTrapPlan,
};
pub use signal::{
    AARCH64_SIGNAL_FRAME_SIZE, Aarch64SignalMachine, FrameCodec as SignalFrameCodec, FrameContext as SignalRestore,
    FrameError as SignalFrameError, FrameImage as SignalFrameImage, FrameRequest as SignalFrameRequest,
    Machine as SignalMachine, X86_SIGNAL_FRAME_SIZE, X86SignalMachine,
};
pub use signal::{
    Abi as SignalAbi, AbiError as SignalMarshalError, MaskOperation, StagedSignalCopyout, Target as SignalTarget,
    WaitPlan as SignalWaitPlan,
};
pub use stat_encoding::{
    STATX_BASIC_STATS, STATX_BTIME, STATX_MNT_ID, STATX_SIZE, StatEncoder, StatEncodingError, StatxExtensions,
};
pub use status::Status;
pub use syscall::{
    AioSyscalls, DescriptorIoSyscalls, Dispatcher as SyscallDispatcher, Disposition as SyscallDisposition,
    EventSyscalls, Family as SyscallFamily, FilesystemSyscalls, IpcSyscalls, MemorySyscalls, NetworkSyscalls,
    NumberTranslation, Operation as SyscallOperation, Ports as SyscallPorts, Route as SyscallRoute, SeccompDispatch,
    SeccompSyscalls, TaskSignalTimeSyscalls,
};
pub use syscall::{
    CANONICAL_SYSCALLS, RETAINED_DISPATCH_ORACLE, RETAINED_NUMBER_ORACLE, X86_LEGACY_SYSCALLS, X86_TRANSLATIONS,
};
pub use syscall::{
    Frame as SyscallFrame, FrameDecoder as SyscallFrameDecoder, FrameError, LinuxResult, RegisterView, RestartKind,
};
pub use system_info::{SYSTEM_INFO_SIZE, SystemInfo};
pub use sysv::{
    Abi as SysvAbi, AbiError as SysvMarshalError, Identifier as SysvIdentifier, MemoryAttachPlan,
    MemoryAttachPlan as SharedMemoryAttachPlan, MemoryControlPlan, MemoryControlPlan as SharedMemoryControlPlan,
    MemoryGetPlan, MemoryGetPlan as SharedMemoryGetPlan, MessageControlPlan, MessageGetPlan, MessageReceivePlan,
    MessageSendPlan, RawIndex as SysvRawIndex, SemaphoreControlPlan, SemaphoreGetPlan, SemaphoreOperatePlan,
    SemaphoreOperation, StagedSysvCopyout,
};
pub use sysv::{
    GETALL, GETNCNT, GETPID, GETVAL, GETZCNT, IPC_CREAT, IPC_EXCL, IPC_INFO, IPC_NOWAIT, IPC_RMID, IPC_SET, IPC_STAT,
    IpcCommand, IpcPermissions, MSG_EXCEPT, MSG_INFO, MSG_NOERROR, MSG_NOWAIT, MSG_STAT, MSG_STAT_ANY, MessageInfo,
    MessageQueueStatus, SEM_INFO, SEM_STAT, SEM_STAT_ANY, SEM_UNDO, SETALL, SETVAL, SHM_EXEC, SHM_INFO, SHM_LOCK,
    SHM_RDONLY, SHM_REMAP, SHM_RND, SHM_STAT, SHM_STAT_ANY, SHM_UNLOCK, SemaphoreInfo, SemaphoreStatus,
    SharedMemoryInfo, SharedMemoryStatus, ShmInfo,
};
pub use uts::{UTS_FIELD_SIZE, UTS_SIZE, UtsName};

#[cfg(test)]
mod marshalling_test;
#[cfg(test)]
mod stat_test;
#[cfg(test)]
mod sysinfo_test;
