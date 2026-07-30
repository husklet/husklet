//! Adversarial / hostile robustness sweep of the hl-gl shim (task #189, the fourth in the
//! driver-robustness quartet after the executor / Vulkan / CUDA sweeps).
//!
//! For EACH abuse below we drive a shim entrypoint with malformed input and assert it either sets the
//! correct GL error (`GL_INVALID_ENUM` / `GL_INVALID_VALUE` / `GL_INVALID_OPERATION` /
//! `GL_INVALID_FRAMEBUFFER_OPERATION`) or safely no-ops — but NEVER panics, arithmetic-overflows (debug),
//! or unbounded-allocates — and then a VALID call still works. This is the "shim survives every hostile
//! input and stays usable" gate, complementing the lifecycle-focused `robustness.rs`.
//!
//! Real bugs fixed by this pass (each has a dedicated test named `*_does_not_unbounded_alloc*`):
//!  * `glBufferSubData` with a huge/overflowing offset grew the buffer's `Vec` unbounded (debug-overflow
//!    panic on `offset + len`) — now bounded to the buffer size → `GL_INVALID_VALUE`.
//!  * `glMapBufferRange` with an out-of-range offset/length grew the buffer's `Vec` unbounded — now
//!    bounded to the buffer size → `GL_INVALID_VALUE`.
//!  * `glTexImage2D` with an over-max/empty extent (e.g. 40000×40000, NULL pixels) allocated a multi-GiB
//!    zeroed plane — now rejected beyond `GL_MAX_TEXTURE_SIZE` → `GL_INVALID_VALUE`.
//!  * `glReadPixels` with a huge `w`/`h` allocated a packed region proportional to `w*h*bpp` before any
//!    bounds check — now capped → `GL_INVALID_VALUE`.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{compute, es3, map, query, readpixels, record, sync};
use hl_gpu::RecordingSink;

fn ctx() -> GlContext {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: 320,
        height: 240,
    });
    c
}

const GL_RGBA: u32 = 0x1908;

#[path = "hostile_gl/allocation.rs"]
mod allocation;
#[path = "hostile_gl/enums.rs"]
mod enums;
#[path = "hostile_gl/framebuffer.rs"]
mod framebuffer;
#[path = "hostile_gl/index.rs"]
mod index;
#[path = "hostile_gl/object.rs"]
mod object;
#[path = "hostile_gl/readback.rs"]
mod readback;
#[path = "hostile_gl/service.rs"]
mod service;
