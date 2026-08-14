//! Socket protocol plumbing, with no domain knowledge at all.
//!
//! Framing, channels, backpressure, capability enforcement, and route
//! composition, none of which know what a container or a pane is. A library
//! that has routes declares them, together with the capabilities they require,
//! and an application that binds a socket merges several such libraries into
//! one server.
//!
//! The capability check is structural rather than conventional. A port is
//! obtainable only from [`Authority`], so a route cannot reach a host service
//! without naming the capability it needs, and a missing check fails to compile
//! instead of failing review. What the capabilities *are* is not decided here:
//! each domain declares its own type and implements [`Capability`] for it.

mod authority;
mod capability;
mod channel;
mod coding;
mod frame;
mod handshake;
mod name;
mod outbox;
mod path;
mod router;
mod session;
mod subscription;
mod transport;

pub use authority::{Authority, Denial, Permit, Reason};
pub use capability::{Capability, CapabilityKey, Grant, Warrant};
pub use channel::{Channels, Permission, Purpose};
pub use coding::{Coding, payload};
pub use frame::{ChannelId, Flags, Frame, Kind, Malformed};
pub use handshake::{Compatibility, Hello, Limits, PROTOCOL, Welcome};
pub use name::{PeerName, Rejection};
pub use outbox::{Emission, Message, Outbox};
pub use path::{Refusal, RelativePath};
pub use router::{Answer, Call, Collision, Method, Outcome, Route, Router, Routes, Unserved};
pub use session::{Session, Topic};
pub use subscription::{Parcel, Streams, Subscriptions};
pub use transport::{Transit, Wire};
