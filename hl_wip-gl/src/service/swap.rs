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

/// `glFlush`/`glFinish` incremental OFFSCREEN flush — the per-context bound on frame growth.
///
/// A deferred single-context GL model accumulates ALL recorded draws into one draw-list and lowers it only
/// at `eglSwapBuffers`. That is fine for a normal single-window app (one context, one swap per frame), but a
/// multi-context app like Chrome renders MANY offscreen FBO passes (gpu-raster worker contexts rasterize
/// compositor tiles into FBOs) that call `glFlush`/`glFinish` but NEVER `eglSwapBuffers`. Their draws would
/// otherwise pile monotonically into the single shared draw-list until the eventual swap frame is enormous
/// (observed cmds 729 → 34228, ~27 MiB) and the host executor NACKs the oversized frame.
///
/// So on `glFlush`/`glFinish` we lower + submit the recorded draw-list as a BOUNDED frame and reset it —
/// but ONLY when the whole pending list is OFFSCREEN (no draw targets the default framebuffer `0`). A
/// pending default-framebuffer (window) draw means a normal frame is mid-flight: it is RETAINED for the swap
/// (so an app that flushes right before `eglSwapBuffers` keeps its window content, and a same-frame offscreen
/// atlas that the window pass samples stays grouped with it for the cross-pass lowering). The submitted
/// frame carries NO `Present` — the offscreen passes render into their persistent FBO render-target textures
/// (stable IR ids kept on the context), exactly what a later pass samples. Returns `Ok(true)` if a frame was
/// flushed, `Ok(false)` if there was nothing to flush (empty, or a window draw is pending → retained).
pub fn flush_offscreen(ctx: &mut GlContext, sink: &mut dyn CommandSink) -> Result<bool> {
    if ctx.draws.is_empty() || !ctx.draws.iter().any(|d| d.fbo != 0) {
        // Nothing, or nothing OFFSCREEN, to drain — leave the (window-only) draws for the swap.
        return Ok(false);
    }
    // PARTITION the recorded draw-list: offscreen (`fbo != 0`) passes are EXECUTED now (submitted, no
    // Present) so they render into their persistent FBO render-target textures; the default-framebuffer
    // (`fbo == 0`, window) draws are RETAINED for `eglSwapBuffers`. This keeps the eventual swap frame
    // BOUNDED — only the window draws, not the thousands of accumulated offscreen tile passes — while the
    // offscreen tile content lands in stable IR textures a later window pass samples (see the persistent
    // `fbo_targets` cross-pass path in `crate::service::frame::lower_draw_n`). Relative order is preserved
    // within each partition; Chrome's compositor renders tiles (offscreen) BEFORE compositing them (window),
    // so flushing all offscreen work first is order-consistent. The recorded blits are offscreen copy ops,
    // so they ride with this flush.
    let n_before = ctx.draws.len();
    let (offscreen, window): (Vec<_>, Vec<_>) = ctx.draws.iter().cloned().partition(|d| d.fbo != 0);
    ctx.draws = offscreen;
    let built = frame::build_frame_ir(ctx);
    // Restore the retained window draws for the swap; the blits were consumed with the offscreen flush.
    ctx.draws = window;
    ctx.blits.clear();
    let Some(f) = built else {
        return Ok(false);
    };
    // NO Present: offscreen passes render into their FBO render-target textures (persistent IR ids); a later
    // window pass samples those by their stable ids. Submit is transactional (mirrors swap): on a sink error
    // the window draws are already restored, and the error propagates so the caller registers it.
    let cmds = f.cmds.len();
    sink.submit(&f.cmds)?;
    hl_log::hl_debug!(
        hl_log::tag::GL,
        "flush_offscreen submitted cmds={} offscreen_of={} retained_window={}",
        cmds,
        n_before,
        ctx.draws.len()
    );
    hl_log::hl_count!(hl_log::tag::GL, "offscreen_flushes");
    Ok(true)
}
