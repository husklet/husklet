//! Authenticated bounded sessions over a byte stream.

mod frame;
mod handshake;

pub use frame::{Direction, FrameKind, Session};
pub use handshake::{Limits, Secret, accept, connect};
