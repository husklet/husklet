//! GL shader and program objects plus immutable draw-time state.
//!
//! Programs retain attached shaders, linked shader IR, uniform reflection, and sampler bindings. Draws
//! snapshot mutable GL state when recorded so deferred frame lowering observes the original resources.

mod draw;
mod feedback;
mod link;
mod object;
mod reflection;
mod shader;
mod table;

pub use draw::{
    Attr, BufferSnapshot, ClientArray, DepthStencilSnapshot, DrawBufferState, DrawCall,
    TargetSnapshot, TextureSnapshot, TransformFeedbackCapture, MAX_DRAW_BUFFERS,
};
pub use feedback::{
    CaptureScalar, CaptureScalarKind, TransformFeedbackLayout, TransformFeedbackVarying,
};
pub use object::Program;
pub use reflection::UniformLocation;
pub use shader::Shader;
pub use table::Programs;

/// The vertex-attribute upper bound GL exposes (matches `hl-shim-gl` `MAXATTR`).
pub const MAX_ATTR: usize = 16;
