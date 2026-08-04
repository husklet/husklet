//! Cross-domain fork coordination and child-resource publication.

mod control;
mod descriptor;
mod event;
mod execution;
mod ipc;
mod memory;
mod network;
mod provider;
mod resources;
mod runtime;
mod task;
mod vfork;

pub use control::{
    ArtifactExchange, Cancellation, Context, Coordinator, Error, Event, Outcome, Participant, ParticipantRole, Phase,
};
pub use descriptor::DescriptorForkParticipant;
pub use event::EventForkParticipant;
pub use execution::ExecutionForkParticipant;
pub use ipc::{IpcForkChild, IpcForkParticipant};
pub use memory::{MemoryChildMapping, MemoryForkHost, MemoryForkParticipant, PrivateFutexReset};
pub use network::NetworkForkParticipant;
pub use provider::ProviderForkParticipant;
pub use resources::{
    ChildResourceCatalog, ChildResourceError, ChildResources, PreparedChildResources, ReadyChildResources,
};
pub use runtime::{Runtime, RuntimeDependencies};
pub use task::TaskForkParticipant;
pub use vfork::{VforkError, VforkParentToken, VforkWake};

#[cfg(test)]
mod control_test;
#[cfg(test)]
mod event_test;
#[cfg(test)]
mod ipc_test;
#[cfg(test)]
mod network_test;
#[cfg(test)]
mod resource_test;
#[cfg(test)]
mod resources_test;
#[cfg(test)]
mod runtime_test;
