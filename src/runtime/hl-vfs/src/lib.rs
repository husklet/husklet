//! Guest filesystem namespace decisions and host-neutral open plans.

#![forbid(unsafe_code)]

mod advisory;
mod device;
mod directory_description;
mod file;
mod mutation;
mod namespace;
mod open;
mod overlay;
mod path;
mod permission;
mod procfs;
mod readonly;
mod resolver;
mod xattr;

pub use advisory::LockCoordinator as AdvisoryLockCoordinator;
pub use advisory::PreparedLockExit;
pub use advisory::{
    FlockMode, FlockOwnerToken, FlockSnapshot, LockCancellation, LockError, LockRange,
    LockSnapshot as AdvisoryLockSnapshot, ProcessLockOwner, RangeConflict, RangeLockKind, RangeLockRequest,
    RangeLockSnapshot, RangeWhence,
};
pub use device::{BUILTIN_DEVICES, BuiltinDevice};
pub use device::{BuiltinDescription, Entropy as DeviceEntropy};
pub use device::{
    Error as DeviceError, Host as DeviceHost, Id as DeviceId, Node as DeviceNode, NodeId as DeviceNodeId,
    NodeKind as DeviceKind, ObjectToken as DeviceObjectToken, OpenCapability as DeviceOpenCapability,
    ProjectedObjectId, Scope as DeviceScope,
};
pub use device::{Registration as DeviceRegistration, Registry as DeviceRegistry, Snapshot as DeviceSnapshot};
pub use directory_description::{VfsDirectoryDescription, VfsDirectoryEntry, VfsDirectoryHost};
pub use file::{FileTransfer, SeekPosition, VfsFileDescription, VfsFileHost, VfsFileToken};
pub use file::{FilesystemKind, FilesystemStats, FilesystemStatsError};
pub use file::{
    Identity, Identity as FileIdentity, Kind, Kind as FileKind, Metadata, Metadata as FileMetadata, MetadataError,
    Permissions, Timestamp, Timestamp as FileTimestamp,
};
pub use mutation::VfsMutations;
pub use mutation::{
    Action as MutationAction, Error as MutationError, HostError as MutationHostError, PinnedParent, RenameFlags,
    TimestampUpdate, VfsMutationHost, VfsTransaction, WatchEvent,
};
pub use namespace::{MountError, MountId, MountKind, MountNamespace, MountRoute, MountSnapshot, MountSourceId};
pub use open::{OpenDecision, OpenDirectory, OpenIntent, OpenPlan, OpenRequest, OverlayAction, SyntheticFilesystem};
pub use overlay::Overlay;
pub use overlay::{
    CopyContent, CopyUpPlan, CreatePlan, DirectoryEntry, Error as OverlayError, Host as OverlayHost, Layer, LayerEntry,
    Lookup as OverlayLookup, MergedDirectory, MutationHandle, NodeKind as OverlayNodeKind, NodeMetadata,
};
pub use path::{GuestName, GuestPath, GuestPathBytes, PathError};
pub use permission::{Access, AccessError, AccessIdentity, Capabilities, Umask};
pub use procfs::{
    AddressSpaceView as ProcfsAddressSpaceView, CgroupView as ProcfsCgroupView, CpuModel as ProcfsCpuModel,
    CpuTicks as ProcfsCpuTicks, CpuView as ProcfsCpuView, DescriptorView as ProcfsDescriptorView, Error as ProcfsError,
    InternetSocketView as ProcfsInternetSocketView, LimitResource as ProcfsLimitResource, LimitView as ProcfsLimitView,
    MemoryRegionLabel as ProcfsMemoryRegionLabel, MemoryRegionView as ProcfsMemoryRegionView,
    MemoryView as ProcfsMemoryView, MountEntry as ProcfsMountEntry, MountView as ProcfsMountView,
    NetworkInterfaceView as ProcfsNetworkInterfaceView, NetworkView as ProcfsNetworkView, NodeKind as ProcfsNodeKind,
    ProcessState as ProcfsProcessState, ProcessView as ProcfsProcessView, Procfs, Source as ProcfsSource,
    StatError as ProcfsStatError, StatInput as ProcfsStatInput, StatState as ProcfsStatState,
    StatView as ProcfsStatView, SystemView as ProcfsSystemView, UnixSocketView as ProcfsUnixSocketView,
    UtsView as ProcfsUtsView,
};
pub use readonly::{ReadOnlyError, ReadOnlyPaths};
pub use resolver::{
    NodeHandle, NodeKind, ResolveConstraints, ResolveError, ResolveHostError, ResolveRequest, ResolvedComponent,
    ResolvedParent, Resolver, VfsHost,
};
pub use xattr::{
    XATTR_LIST_MAXIMUM, XATTR_NAME_MAXIMUM, XATTR_VALUE_MAXIMUM, XattrError, XattrFlags, XattrHost, XattrMutation,
    XattrName, Xattrs,
};

#[cfg(test)]
mod directory_test;
#[cfg(test)]
mod metadata_test;
#[cfg(test)]
mod open_test;
#[cfg(test)]
mod vfs_test;
#[cfg(test)]
mod xattr_test;
