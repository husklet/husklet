//! Transport values + their wire layout: the ioctl ABI ([`abi`]), the 16-byte submit header + ack
//! ([`header`]), a full submit frame ([`frame`]), and the connection handshake bytes ([`handshake`]).
//!
//! Pure values + serialization only — no socket IO (that is [`super::adapter`]) and no GPU semantics. The
//! header/frame framing is transport-private and does NOT go through `protocol::codec`; the handshake body
//! IS protocol and is delegated to it.

pub mod abi;
pub mod config;
pub mod error;
pub mod frame;
pub mod handshake;
pub mod header;
pub mod readback;
