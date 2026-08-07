//! Projected-resource provider protocol and lifetime.
//!
//! It owns the provider handle namespace and the host-neutral framed client
//! protocol. Concrete socket, file, and readiness adapters remain outside.

mod authority;
mod checkpoint;
mod checkpoint_activity;
mod client;
mod epoll_registry;
mod file;
mod namespace;
mod protocol;
mod transport;
mod tree;

pub use authority::{FileAuthority, FileBackend, FileInfo, FileObject, FileWire, ServerLimits};
pub use checkpoint::{
    PROVIDER_CHECKPOINT_EVENT_BYTE_MAXIMUM, PROVIDER_CHECKPOINT_FILE_MAXIMUM, PROVIDER_CHECKPOINT_PATH_BYTE_MAXIMUM,
    PROVIDER_CHECKPOINT_VERSION, ProviderCheckpointCapture, ProviderCheckpointImage, ProviderCheckpointReconnect,
    ProviderClientCheckpoint, ProviderFileCheckpoint, ProviderRemoteRestore, ProviderResourceKey,
    ProviderResourceReference, ProviderSubscriptionCheckpoint,
};
pub use client::model::{ClientLimits, Direction, FrameFault, ProviderError, Reply, RequestId, Ticket};
pub use client::subscription::{
    EventObserver, ProviderEvent, SubscriptionIdentity, SubscriptionKey, SubscriptionSnapshot,
};
pub use client::{Provider, ProviderSubscription};
pub use epoll_registry::{
    CallbackLease, EpollRegistry, ReadyEvent, RegistryError, RegistrySnapshot, WatchConfig, WatchIdentity,
    WatchSnapshot, WatchToken,
};
pub use file::model::{CallArgument, FileAccess, FileError, FileMetadata, FileRebind, FileSnapshot, ReplyOperation};
pub use file::{ProjectedFile, ProjectedFiles};
pub use namespace::{
    Close, Handle, HandleKind, HandleNamespace, HandleReservation, NamespaceError, NamespaceForkPlan, NamespaceLimits,
    NamespaceSnapshot, RemoteId, SnapshotEntry, TransferCapability,
};
pub use protocol::FrameKind;
pub use transport::{ProviderTransport, TransportError};
pub use tree::{TreeAuthority, TreeKind, TreeObject, TreeOpen, TreeRoot, TreeStat, Wire as TreeWire};

#[cfg(test)]
mod test;

#[cfg(test)]
mod protocol_test;

#[cfg(test)]
mod test_support;

#[cfg(test)]
#[path = "file/test.rs"]
mod file_test;

#[cfg(test)]
#[path = "file/race_test.rs"]
mod file_race_test;

#[cfg(test)]
mod subscription_test;

#[cfg(test)]
mod registry_test;

#[cfg(test)]
mod checkpoint_test;
