//! Cross-domain checkpoint participants and bounded restoration bindings.

mod control;
mod descriptor;
mod event;
mod execution;
mod ipc;
mod memory;
mod memory_wire;
mod network;
mod provider;
mod resource_binding;
mod seccomp;
mod task;

pub use control::{Error, Participant, Phase, Role, RuntimeCheckpointCoordinator};
pub use descriptor::{
    DirectoryObjectCatalog, DirectoryObjectCheckpoint, FileObjectCatalog, FileObjectCheckpoint, ObjectCatalog,
    Participant as DescriptorCheckpointParticipant, Table as DescriptorTable,
};
pub use event::{
    BindingRestore, Catalog, CheckpointCodec, DescriptorRebind, DescriptorReference, ObjectBindings,
    Participant as EventParticipant, ResourceRegistry as EventResourceRegistry, ResourceRestore, WireCodec,
};
pub use execution::ExecutionCheckpointParticipant;
pub use ipc::{
    IPC_CHECKPOINT_BYTES_MAXIMUM, IpcCatalog, IpcCheckpointCodec, IpcCheckpointParticipant, OpenPipe, PipeBindings,
    PipePublication, PipeRegistry, PortableIpcCodec, RegistryError, ResourceRebind,
};
pub use memory::{
    Memory, MemoryCheckpointCodec, MemoryCheckpointParticipant, MemoryResourceRestore, MemoryResourceTransaction,
    MemoryState, PortableMemoryCodec,
};
pub use network::{
    CheckpointHost as NetworkCheckpointHost, NETWORK_CHECKPOINT_BYTES_MAXIMUM, NetworkCatalog, NetworkCheckpointCodec,
    NetworkCheckpointParticipant, ObjectBindings as NetworkObjectBindings, PortableNetworkCodec, ReconnectedSocket,
};
pub use provider::{
    PROVIDER_CHECKPOINT_BYTES_MAXIMUM, PortableProviderCodec, ProviderCheckpointCodec, ProviderCheckpointParticipant,
    ProviderLease, ProviderNamespace, ProviderRegistry, ProviderRegistryError,
};
pub use resource_binding::{TaskBindingError, TaskBindingRestore, TaskResourceCatalog};
pub use task::{
    Codec as TaskCheckpointCodec, Participant as TaskCheckpointParticipant, PortableCodec as PortableTaskCodec,
    Registry as TaskRegistry,
};

#[cfg(test)]
mod control_test;
#[cfg(test)]
mod descriptor_test;
#[cfg(test)]
mod ipc_test;
#[cfg(test)]
mod memory_test;
#[cfg(test)]
mod network_test;
#[cfg(test)]
mod provider_test;
#[cfg(test)]
mod task_test;
