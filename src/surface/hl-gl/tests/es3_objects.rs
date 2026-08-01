//! Unit tests for ES3 client-side object families.
//!
//! These families lower to no GPU IR, so a plain [`GlContext`] exercises their observable object state
//! and GL errors without a socket, GPU, or guest library.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{es3, intro, query, record};

fn ctx() -> GlContext {
    let mut context = GlContext::new();
    context.set_surface(GlSurface {
        have: true,
        width: 256,
        height: 128,
    });
    context
}

#[path = "es3_objects/buffer.rs"]
mod buffer;
#[path = "es3_objects/feedback.rs"]
mod feedback;
#[path = "es3_objects/framebuffer.rs"]
mod framebuffer;
#[path = "es3_objects/link.rs"]
mod link;
#[path = "es3_objects/main_body.rs"]
mod main_body;
#[path = "es3_objects/pipeline.rs"]
mod pipeline;
#[path = "es3_objects/query.rs"]
mod query_object;
#[path = "es3_objects/sampler.rs"]
mod sampler;
#[path = "es3_objects/texture.rs"]
mod texture;
