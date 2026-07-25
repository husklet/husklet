//! Wire tag constants, surfaced under `codec` for callers that reason about the serialization directly.
//!
//! The source of truth lives in [`crate::protocol::model::command`] (both the command types and the
//! capability descriptor reference the tags, so they belong in `model` to avoid a `model → codec`
//! dependency inversion). This module re-exports them for the `codec` role.

pub use crate::protocol::model::command::{etag, tag, WIRE_VERSION};
