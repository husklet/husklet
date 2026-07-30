//! `transport` — the mechanism that MOVES encoded protocol batches across a process boundary.
//!
//! It executes nothing and knows no GPU semantics: it encodes via [`crate::protocol::codec`], frames the
//! bytes with a transport-private 16-byte submit header + 1-byte ack ([`model`]), and moves them over a
//! Unix socket ([`adapter::unix`]). It provides the two ends of the boundary:
//!
//! - [`client::RemoteCommandSink`] — a [`crate::protocol::port::sink::CommandSink`] that encodes each batch
//!   and writes it as a framed submit; it owns connect/reconnect, residency replay, and handshake driving.
//! - [`server::serve`] / [`server::serve_connection`] — the host-side loop that accepts a connection,
//!   advertises capabilities, reads frames, decodes them, hands the batch to a handler, and writes acks.
//!
//! The framing (header + ack) is byte-identical to the shipped guest/host, so an existing guest/host
//! interoperates; only the handshake body flows through the protocol codec.

pub mod adapter;
pub mod client;
pub mod model;
pub mod server;

// Facade re-exports so downstream reads `hl_gpu::transport::{RemoteCommandSink, serve, Surface, …}`.
pub use client::RemoteCommandSink;
pub use model::abi::{
    GpuAlloc, Surface, DEFAULT_EXEC_SOCK, DRM_FMT_XRGB8888, HL_DMABUF_MOD_MAGIC,
    HL_IOCTL_GPU_ALLOC, RENDER_NODE,
};
pub use model::config::{TransportConfig, TransportConfigError};
pub use model::error::{TransportError, TransportPhase};
pub use model::frame::Frame;
pub use model::header::{SubmitHeader, ACK_FAIL, ACK_OK};
pub use model::readback::{ReadbackRequest, READBACK_MAGIC, READBACK_VERSION};
pub use server::{
    serve, serve_connection, serve_connection_with_handler, ConnectionHandler, Verdict,
};
