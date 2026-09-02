//! The workspace extension protocol.
//!
//! An extension is an image run as a sidecar beside a workspace, given a
//! private socket that is both a programmatic interface to the workspace and a
//! channel for the interface it draws.
//!
//! This crate owns what a workspace call means and nothing else. Framing,
//! channels, backpressure, and the capability check are `hl-rpc`'s and are
//! re-exported here. It opens no socket, spawns no container, and calls no
//! service: every host effect is reached through a port declared in [`port`]
//! and implemented by the application that embeds this. That is what lets the
//! whole protocol be exercised with in-memory ports and no runtime at all.
//!
//! The capability check is structural rather than conventional. A port is
//! obtainable only from [`Authority`], so a dispatcher cannot reach a host
//! service without naming the capability it needs, and a missing check fails to
//! compile instead of failing review.

mod capability;
pub mod codec;
mod installation;
mod manifest;
pub mod port;
mod request;
mod session;
mod subscription;

pub use capability::{Capability, Grant};
pub use codec::Coding;
pub use hl_rpc::{
    Authority, ChannelId, Channels, Compatibility, Denial, Emission, Flags, Frame, Hello, Kind, Limits, Malformed,
    Parcel, Permission, Permit, Purpose, Reason, Refusal, RelativePath, Streams, Transit, Wire, PROTOCOL,
};
pub use installation::{Disposition, Installation, Objection, Record, Stage, Summary, Update, UpdateFailure};
pub use manifest::{
    Activation, ExtensionName, Invalid, Manifest, PaneProvider, PaneSelection, Presentation, Resources,
};
pub use port::{HostError, PaneSemanticAction, PaneSemanticTree, SemanticActionKind, SemanticNode};
pub use port::{NetworkSummary, VolumeSummary, WorkspaceConfiguration, WorkspaceMount, WorkspaceTerminal};
pub use request::{Failure, Reply, Request, Topic, WorkspaceInfo};
pub use session::{Services, Session, SurfaceFrame, SurfaceMutation};
pub use subscription::{
    PaneChange, PaneChangeKind, PointerPhase, Snapshot, Subscriptions, WorkspaceEvent, WorkspaceEventBatch,
};

/// The host's opening frame, carrying this domain's grant.
pub type Welcome = hl_rpc::Welcome<Capability>;

/// Pending output for one session, keyed by this domain's topics.
pub type Outbox = hl_rpc::Outbox<Topic>;

/// One queued message on this domain's topics.
pub type Message = hl_rpc::Message<Topic>;
