//! Serialization of the protocol: the little-endian wire primitives ([`wire`]) and the encode/decode of
//! every command, descriptor, and the capability handshake ([`encode`] / [`decode`]).
//!
//! `codec` depends inward on [`super::model`] (values) — never the reverse. The command/descriptor
//! encode+decode logic is attached to the model types as inherent methods defined here, so downstream
//! reads naturally (`Cmd::encode`, `Cmd::decode`) while the model stays free of any serialization code.

pub mod decode;
pub mod encode;
pub mod tag;
pub mod wire;

pub use wire::{Decoder, Encoder};
