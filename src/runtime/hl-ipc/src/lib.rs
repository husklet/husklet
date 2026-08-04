//! Non-socket Linux IPC objects and namespaces.

#![forbid(unsafe_code)]

mod catalog;
mod checkpoint;
mod checkpoint_activity;
mod message;
mod pipe;
mod posix_queue;
mod semaphore;
mod sysv;

pub use catalog::{IpcCatalog, IpcCatalogError, PreparedPipe};
pub use checkpoint::{
    IPC_CHECKPOINT_VERSION, IPC_PIPE_MAXIMUM, IpcCatalogRestore, IpcCheckpointError, IpcCheckpointImage,
    IpcCheckpointRebind, IpcPipeId, IpcResourceKey, PipeCheckpoint, PipeEndpointBinding, PipeEndpointKind,
    SharedBackingAccess, SharedBackingCheckpoint, SharedBackingKey, TaskCheckpoint,
};
pub use message::model::{
    MSG_COPY, MSG_EXCEPT, MSG_NOERROR, MSG_NOWAIT, MessageError, MessageLimits, MessageQueueId, MessageQueueMetadata,
    MessageQueueSnapshot, MessageReceive, MessageSnapshot, MsgGetRequest, QueueSnapshot,
};
pub use message::queue::MessageQueueNamespace;
pub use message::receive::PreparedMessageReceive;
pub use pipe::snapshot::{NamedFifoSnapshot, PipeSnapshot};
pub use pipe::transfer::{PipeTransfer, PipeTransferMode};
pub use pipe::{
    DEFAULT_PIPE_CAPACITY, MAX_PIPE_CAPACITY, NamedFifo, NamedFifoCatalog, NamedFifoKey, NamedFifoOpen,
    NamedFifoOpenError, NamedFifoStatus, NamedFifoWait, PIPE_BUF, PIPE_CAPACITY_GRANULE, Pipe, PipeCreateError,
    PipeEndpoint, PipeStatus,
};
pub use posix_queue::{
    MqAccess, MqAttributes, MqDescription, MqError, MqEvent, MqLimits, MqNamespace, MqOpen, MqReceipt,
};
pub use semaphore::model::{
    IPC_NOWAIT, SEM_UNDO, SemGetRequest, SemaphoreError, SemaphoreId, SemaphoreLimits, SemaphoreMetadata,
    SemaphoreOperation, SemaphoreSetSnapshot, SemaphoreSnapshot,
};
pub use semaphore::{
    CommittedSemaphoreExit, CommittedSemaphoreFork, PreparedSemaphoreExec, PreparedSemaphoreExit,
    PreparedSemaphoreFork, SemaphoreNamespace,
};
pub use sysv::id::{MESSAGE_QUEUE_IDENTIFIERS, SEMAPHORE_IDENTIFIERS, SHARED_MEMORY_IDENTIFIERS};
pub use sysv::memory::{
    CommittedMemoryExec, CommittedMemoryFork, ForkAttachmentPlan, OwnedPreparedMemoryFork, PreparedMemoryExec,
    PreparedMemoryFork, SharedMemoryNamespace,
};
pub use sysv::model::{
    AttachPlan, Credentials, IPC_PRIVATE, InheritedAttachment, IpcKey, SHM_EXEC, SHM_RDONLY, SHM_REMAP, SHM_RND,
    SharedMemoryError, SharedMemoryId, SharedMemoryLimits, SharedMemoryLockIntent, SharedMemoryMetadata,
    SharedMemorySnapshot, ShmGetRequest,
};

#[cfg(test)]
mod catalog_test;
#[cfg(test)]
mod exec_test;
#[cfg(test)]
#[path = "message/queue_test.rs"]
mod message_queue_test;
#[cfg(test)]
#[path = "pipe/transfer_test.rs"]
mod pipe_transfer_test;
#[cfg(test)]
#[path = "semaphore/test.rs"]
mod semaphore_test;
#[cfg(test)]
#[path = "sysv/memory_test.rs"]
mod sysv_memory_test;
#[cfg(test)]
mod test;
