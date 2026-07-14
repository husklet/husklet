//! `eglSwapBuffers` — the frame boundary + the one sink-touching op in the GL driver.
//!
//! Ported from `hl-shim-gl/src/egl.rs` (`eglSwapBuffers`). It builds the frame's `Cmd` stream from the
//! recorded draw-list ([`crate::service::frame::build_frame_ir`]), submits it through the
//! [`hl_gpu::CommandSink`], appends the `Present` that scans the rendered default target out to its
//! surface, then resets the per-frame draw state. This is the tested lowering surface: a driver test
//! drives it against a [`hl_gpu::RecordingSink`] and asserts the exact recorded command sequence.
//!
//! The submit is TRANSACTIONAL, matching the C shim: on a sink error the recorded draws are RETAINED
//! (the frame is not reset) so the caller can surface an `EGL_CONTEXT_LOST` and the frame is not
//! silently lost.

use crate::model::context::GlContext;
use crate::service::frame;
use hl_gpu::{Cmd, CommandSink, Result};

/// `eglSwapBuffers()` — lower + submit + present the recorded frame, then reset per-frame state. Returns
/// `true` if a frame was presented, `false` if there was nothing (or nothing yet supported) to present
/// (a no-op swap — matching the shim's behaviour on an uncovered frame shape).
pub fn swap_buffers(ctx: &mut GlContext, sink: &mut dyn CommandSink) -> Result<bool> {
    let built = frame::build_frame_ir(ctx);
    let Some(mut f) = built else {
        // Nothing to present: still clear the (possibly empty / unsupported) draw-list so the next frame
        // starts clean, exactly as the C shim resets after a no-IR swap.
        ctx.reset_frame();
        return Ok(false);
    };

    // Append the Present of the rendered default target to its surface.
    let (surface, texture) = f.present;
    f.cmds.push(Cmd::Present { surface, texture });

    // TRANSACTIONAL: submit BEFORE resetting. On failure the draws are retained and the error propagates.
    sink.submit(&f.cmds)?;

    ctx.reset_frame();
    Ok(true)
}
