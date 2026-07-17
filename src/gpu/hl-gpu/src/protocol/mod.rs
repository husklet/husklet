//! The hl-GPU protocol: the versioned command language drivers speak and the port they submit through.
//!
//! - [`model`] — the owned values + invariants (ids, errors, enums, descriptors, commands,
//!   capabilities, neutral kernel-IR types). Pure data; no serialization, no platform types.
//! - [`codec`] — serialization: little-endian wire primitives and the encode/decode of every command.
//! - [`port`] — the [`port::sink::CommandSink`] boundary trait, referencing only protocol types.

pub mod codec;
pub mod model;
pub mod port;

pub use model::command::WIRE_VERSION;
pub use model::error::{GpuError, Result};
