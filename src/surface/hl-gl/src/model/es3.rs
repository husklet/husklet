//! ES3 client-side object families: sampler objects, occlusion/transform-feedback QUERY objects,
//! transform-feedback objects, and separate-shader PROGRAM PIPELINE objects.
//!
//! Ported from `hl-shim-gl/src/state.rs` (`SamplerObj`, `QueryObj`, `TransformFeedbackObj`) + the
//! `gl_shim.c` name allocators. None of these families lower to GPU IR — a real driver emits no command
//! for a `glSamplerParameteri`/`glBeginQuery`/`glBindTransformFeedback`; they carry observable object
//! STATE the app polls back through `glGetSamplerParameter*` / `glGetQueryObjectuiv` /
//! `glGetTransformFeedbackVarying`. So the tables live here as pure model state (the [`crate::service`]
//! layer drives them, submits nothing), and the deferred frame IR is unaffected.

use super::glconst::*;
use hl_gpu::protocol::model::enums::{AddressMode, Filter};
use std::collections::{HashMap, HashSet};

// ==================================================================================================

mod pipeline;
mod query;
mod sampler;
mod transform;

pub use pipeline::*;
pub use query::*;
pub use sampler::*;
pub use transform::*;
