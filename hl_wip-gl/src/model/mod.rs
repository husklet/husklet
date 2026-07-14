//! The GL object model + its invariants (mirrors hl-cuda's `model/`).
//!
//! Pure owned values: no `Cmd` is built here and nothing is submitted. A [`context::GlContext`]
//! aggregates the per-context state (surface descriptor, buffer/texture/program tables, the bound GL
//! state, the recorded draw-list, and the IR id counters); the [`super::service`] layer drives it and
//! emits the IR at swap.
//!
//! [`glconst`] carries the canonical Khronos numeric constants (the GLES/EGL `#define`s) the recording
//! ops and the frame builder reference by name rather than magic hex.

pub mod buffer;
pub mod context;
pub mod glconst;
pub mod program;
pub mod texture;
