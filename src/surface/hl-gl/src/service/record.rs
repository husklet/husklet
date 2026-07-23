//! The `gl*` recording ops — the deferred-lowering front half of the driver.
//!
//! Every function here mutates [`GlContext`] and submits NOTHING: a `gl*` call records into per-context
//! state (a created object, a binding, or an appended [`DrawCall`]) exactly as `gl_shim.c` does, and the
//! IR is emitted later, at swap, by [`crate::service::frame`]. Ported from `hl-shim-gl/src/gles.rs`
//! (the state-recording bodies) — the semantics (bindings, the draw-time state snapshot) are preserved.

use crate::model::context::GlContext;
use crate::model::glconst::*;
use crate::model::program::DrawCall;
use hl_gpu::protocol::model::enums::TextureFormat;

// ---- buffers -------------------------------------------------------------------------------------

/// `glBindBuffer(target, name)`. `GL_ARRAY_BUFFER`/`GL_ELEMENT_ARRAY_BUFFER` use their dedicated bindings;
/// every other ES3 target (UBO/SSBO/PBO/dispatch-indirect/…) records into the general binding map so
/// `glMapBufferRange`/`glDispatchComputeIndirect` can resolve it.
mod buffers;
mod draw;
mod framebuffers;
mod programs;
mod state;
mod textures;

pub use buffers::*;
pub use draw::*;
pub use framebuffers::*;
pub use programs::*;
pub use state::*;
pub use textures::*;
