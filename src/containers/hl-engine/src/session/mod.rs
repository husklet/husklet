//! Authenticated bounded sessions over a byte stream.

mod frame;
mod handshake;

pub use frame::{Direction, Frame, FrameError, FrameKind, Session};
pub use handshake::{HandshakeError, Limits, Secret, accept, connect};
