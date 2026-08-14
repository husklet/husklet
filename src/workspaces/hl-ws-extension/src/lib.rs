//! The workspace extension protocol.
//!
//! An extension is an image run as a sidecar beside a workspace, given a
//! private socket that is both a programmatic interface to the workspace and a
//! channel for the interface it draws.
//!
//! This crate owns the protocol and nothing else. It opens no socket, spawns no
//! container, and calls no service: every host effect is reached through a port
//! declared in [`port`] and implemented by the application that embeds this.
//! That is what lets the whole protocol — framing, channels, backpressure,
//! capability enforcement, and dispatch — be exercised with in-memory ports and
//! no runtime at all.
//!
//! The capability check is structural rather than conventional. A port is
//! obtainable only from [`Authority`], so a dispatcher cannot reach a host
//! service without naming the capability it needs, and a missing check fails to
//! compile instead of failing review.

mod authority;
mod capability;
mod channel;
mod frame;
mod handshake;
mod manifest;
mod path;
pub mod port;

pub use authority::{Authority, Denial, Permit, Reason};
pub use capability::{Capability, Grant};
pub use channel::{Channels, Permission, Purpose};
pub use frame::{ChannelId, Flags, Frame, Kind, Malformed};
pub use handshake::{Compatibility, Hello, Limits, Welcome, PROTOCOL};
pub use manifest::{Activation, ExtensionName, Invalid, Manifest, Presentation, Resources};
pub use path::{Refusal, RelativePath};
