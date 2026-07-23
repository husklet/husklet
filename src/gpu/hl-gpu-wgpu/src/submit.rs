//! Command-buffer replay: turn a validated [`CommandBuffer`]'s encoder ops into real wgpu work.
//!
//! Render and compute passes are executed as forward-scanned `Begin..End` units (a wgpu pass borrows its
//! encoder for its whole lifetime, so it can't straddle the outer op loop); everything else — sub-rect
//! clears, buffer/texture copies, fills — is CPU-mediated through the byte-addressable buffer/texture
//! helpers so wgpu's copy-alignment rules never leak to the protocol boundary. Each unit submits and, on
//! the readback paths, `poll(Wait)`s, giving the strict sequential semantics the CPU oracle guarantees.
//!
//! The clears, copies, dispatches, and draws here are the executed analogues of the CPU oracle's
//! `submit` (`hl-gpu/src/cpu/executor.rs`) — same ops, same order, now on the GPU.

use hl_gpu::protocol::model::command::{CommandBuffer, Enc};
use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, DepthAttachment, Extent3d, Origin3d, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{Filter, IndexFormat, LoadOp, TextureAspect, TextureFormat};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};
use hl_log::tag;

use crate::convert::Format;
use crate::pipeline::PipelineNative;
use crate::{buffer, fence, texture, WgpuExecutor};

/// Intersect a GL-style viewport rect `(x, y, w, h)` with the render target `[0, tw] × [0, th]` so it
/// satisfies wgpu's strict `RenderPass::set_viewport` bounds (`x,y >= 0`, `x+w <= tw`, `y+h <= th`, `w,h > 0`
/// — see `wgpu_core::command::render::set_viewport`). Returns the in-bounds sub-rect, or `None` when the
/// intersection is empty (the whole viewport lies outside the target — nothing should rasterize).
///
/// WHY this exists: GL's `glViewport` is only the NDC→window transform and permits a rect that starts
/// negative or overhangs the framebuffer; GL simply lets the framebuffer clip the fragments. wgpu forbids
/// such a rect outright, so forwarding Chrome's legitimate scrolled-layer viewport (`y=-386, h=642` into a
/// 256-tall target) verbatim NACKs the frame and orphans its resources. Intersecting makes the frame VALID
/// and keeps the whole in-bounds path (every non-scrolled draw) pixel-exact.
///
/// FIDELITY NOTE: wgpu ties the NDC→window transform to the (necessarily in-bounds) rect, so for a viewport
/// that is genuinely larger than / offset outside the target the transform cannot be reproduced exactly
/// without baking a compensating scale+bias into every vertex shader's `gl_Position` (ANGLE's driver-uniform
/// technique) — a follow-up. This intersection is the standard "make it valid" clamp: it stops the
/// validation NACK + downstream `UnknownId` orphan cascade and confines rasterization to the visible region.
fn clamp_viewport(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    tw: u32,
    th: u32,
) -> Option<(f32, f32, f32, f32)> {
    let (tw, th) = (tw as f32, th as f32);
    let x0 = x.max(0.0);
    let y0 = y.max(0.0);
    let x1 = (x + w).min(tw);
    let y1 = (y + h).min(th);
    let cw = x1 - x0;
    let ch = y1 - y0;
    if cw.is_finite() && ch.is_finite() && cw > 0.0 && ch > 0.0 {
        Some((x0, y0, cw, ch))
    } else {
        None
    }
}

impl WgpuExecutor {
    pub(crate) fn submit_cb(
        &mut self,
        res: &mut SessionResources,
        cb: &CommandBuffer,
    ) -> Result<()> {
        let ops = &cb.encoder;
        let mut i = 0;
        while i < ops.len() {
            match &ops[i] {
                Enc::BeginRenderPass { color, depth } => {
                    let end = find_end(ops, i, Enc::EndRenderPass)?;
                    self.run_render_pass(res, color, depth.as_ref(), &ops[i + 1..end])?;
                    i = end + 1;
                }
                Enc::BeginComputePass => {
                    let end = find_end(ops, i, Enc::EndComputePass)?;
                    self.run_compute_pass(res, &ops[i + 1..end])?;
                    i = end + 1;
                }
                Enc::ClearRect {
                    texture,
                    x,
                    y,
                    w,
                    h,
                    color,
                } => {
                    let (fmt, tw, th) = {
                        let t = texture::WgpuTexture::get(res, *texture)?;
                        (t.format, t.width, t.height)
                    };
                    // Clamp the rect to the texture, exactly as the CPU oracle's `clear_rect` does: a rect
                    // that runs past the texture edge fills ONLY the covered sub-rectangle. Without this the
                    // raw `x,y,w,h` would be handed to `Queue::write_texture`, whose bounds validation
                    // rejects an over-hang (a hard wgpu error) — where the oracle silently clamps. The
                    // protocol/runtime `validate` does not bounds-check `ClearRect`, so an over-hanging rect
                    // is a legal command; the two backends must handle it identically. An empty clamped rect
                    // is a no-op.
                    let x0 = (*x).min(tw);
                    let y0 = (*y).min(th);
                    let cw = x.saturating_add(*w).min(tw).saturating_sub(x0);
                    let ch = y.saturating_add(*h).min(th).saturating_sub(y0);
                    if cw != 0 && ch != 0 {
                        let texel = Format::from(fmt).clear_texel(*color)?;
                        let mut data = Vec::with_capacity(texel.len() * cw as usize * ch as usize);
                        for _ in 0..(cw as usize * ch as usize) {
                            data.extend_from_slice(&texel);
                        }
                        self.write_region(res, *texture, x0, y0, 0, cw, ch, 1, 0, &data)?;
                    }
                    i += 1;
                }
                Enc::CopyBufferToBuffer {
                    src,
                    src_offset,
                    dst,
                    dst_offset,
                    size,
                } => {
                    let bytes = self.read_bytes(res, *src, *src_offset, *size as usize)?;
                    self.write_bytes(res, *dst, *dst_offset, &bytes)?;
                    i += 1;
                }
                Enc::CopyTextureToBuffer {
                    src,
                    mip,
                    width,
                    height,
                    dst,
                    dst_offset,
                    bytes_per_row,
                } => {
                    let (bpt, tw, th, mips) = {
                        let t = texture::WgpuTexture::get(res, *src)?;
                        (
                            Format::from(t.format).texel_bytes()? as u32,
                            t.width,
                            t.height,
                            t.mip_levels,
                        )
                    };
                    // Honor the `mip` field: the readback below reads THAT level (not silently the base).
                    // An out-of-range mip is a typed `OutOfBounds` (the runtime does not range-check this op).
                    if *mip >= mips {
                        return Err(GpuError::OutOfBounds);
                    }
                    // The copy region must lie inside the SOURCE MIP LEVEL, whose dimensions are the base
                    // extent halved per level (floored at 1). A `width`/`height` past the level edge would
                    // slice past the tight readback plane below (a Rust panic), so guard it into `OutOfBounds`.
                    let lw = (tw >> *mip).max(1);
                    let lh = (th >> *mip).max(1);
                    if *width > lw || *height > lh {
                        return Err(GpuError::OutOfBounds);
                    }
                    // The tight readback plane is packed at the MIP LEVEL's width, not the copy region's
                    // width — the source row stride is `mip_width*bpt`, so a sub-region copy (width < level
                    // width) advances by the full plane stride, exactly as the CPU oracle does at level 0.
                    let src_stride = (lw * bpt) as usize;
                    let plane = self.read_texture_tight_mip(res, *src, *mip)?;
                    let row = (*width * bpt) as usize;
                    // `bytes_per_row == 0` means "tightly packed" on the destination (the protocol/oracle
                    // convention); a non-zero value is the explicit row stride.
                    let dst_stride = if *bytes_per_row == 0 {
                        row
                    } else {
                        *bytes_per_row as usize
                    };
                    for r in 0..*height as usize {
                        let s = r * src_stride;
                        let d = *dst_offset + (r * dst_stride) as u64;
                        self.write_bytes(res, *dst, d, &plane[s..s + row])?;
                    }
                    i += 1;
                }
                Enc::CopyBufferToTexture {
                    src,
                    src_offset,
                    bytes_per_row,
                    dst,
                    mip,
                    width,
                    height,
                } => {
                    let (bpt, dst_depth) = {
                        let t = texture::WgpuTexture::get(res, *dst)?;
                        // The destination region (`mip`, `width`, `height`) must fit the texture: an
                        // out-of-range mip or a `width`/`height` overhanging the mip level would be handed
                        // to `queue.write_texture`, whose bounds validation is a HARD wgpu error (its
                        // uncaptured-error handler panics). The runtime does not range-check this op, so
                        // guard it into a typed error here. Mip-level extent is the base extent halved per
                        // level, floored at 1 (the WebGPU mip pyramid).
                        if *mip >= t.mip_levels {
                            return Err(GpuError::OutOfBounds);
                        }
                        let lw = (t.width >> *mip).max(1);
                        let lh = (t.height >> *mip).max(1);
                        if *width > lw || *height > lh {
                            return Err(GpuError::OutOfBounds);
                        }
                        (Format::from(t.format).texel_bytes()? as u32, t.depth)
                    };
                    let row = (*width * bpt) as usize;
                    // `bytes_per_row == 0` means the source rows are tightly packed (the oracle convention).
                    let src_stride = if *bytes_per_row == 0 {
                        row
                    } else {
                        *bytes_per_row as usize
                    };
                    // A 3D destination has no z/depth field on this op, so the copy fills the WHOLE volume:
                    // the source holds `width*height*depth` tightly-stacked slices (rows advance `height`
                    // per slice). A plain 2D texture keeps `depth == 1`, i.e. the original single-plane copy.
                    let rows = *height as usize * dst_depth as usize;
                    let mut tight = Vec::with_capacity(row * rows);
                    for r in 0..rows {
                        let off = *src_offset + (r * src_stride) as u64;
                        tight.extend_from_slice(&self.read_bytes(res, *src, off, row)?);
                    }
                    self.write_region(
                        res, *dst, 0, 0, 0, *width, *height, dst_depth, *mip, &tight,
                    )?;
                    i += 1;
                }
                Enc::CopyTextureToTexture {
                    src,
                    src_sub,
                    src_origin,
                    dst,
                    dst_sub,
                    dst_origin,
                    extent,
                } => {
                    self.copy_texture_to_texture(
                        res, *src, src_sub, src_origin, *dst, dst_sub, dst_origin, extent,
                    )?;
                    i += 1;
                }
                Enc::FillBuffer {
                    buffer,
                    offset,
                    size,
                    value,
                } => {
                    self.fill_buffer(res, *buffer, *offset, *size, *value)?;
                    i += 1;
                }
                // Scaled/filtered blit: wgpu has no native image blit, so it is resampled by a
                // textured-triangle draw into the destination rect (see `blit.rs`). This is the executed
                // analogue of the CPU oracle's `blit_texture`.
                Enc::BlitTexture {
                    src,
                    src_sub,
                    src_origin,
                    src_extent,
                    dst,
                    dst_sub,
                    dst_origin,
                    dst_extent,
                    filter,
                } => {
                    self.blit_texture(
                        res, *src, src_sub, src_origin, src_extent, *dst, dst_sub, dst_origin,
                        dst_extent, *filter,
                    )?;
                    i += 1;
                }
                // Multisample resolve: average the multisampled `src`'s samples into single-sample `dst`.
                // wgpu has no standalone resolve command, so it is realized as a zero-draw render pass that
                // LOADs the multisampled color attachment and hands `dst` as its `resolve_target` — the
                // resolve happens at pass end (see `resolve_texture`). This is the executed analogue of the
                // CPU oracle's sample averaging.
                Enc::ResolveTexture {
                    src,
                    src_sub,
                    src_origin,
                    dst,
                    dst_sub,
                    dst_origin,
                    extent,
                } => {
                    self.resolve_texture(
                        res, *src, src_sub, src_origin, *dst, dst_sub, dst_origin, extent,
                    )?;
                    i += 1;
                }
                // Stray state-setters outside a pass cannot occur in a validated command buffer.
                _ => i += 1,
            }
        }
        if let Some((f, v)) = cb.signal {
            fence::Fence::signal(res, f, v)?;
        }
        Ok(())
    }

    /// Convert any wgpu VALIDATION error raised while running `body` into a typed [`GpuError`], instead of
    /// letting it reach wgpu 24's default uncaptured-error handler (which PANICS). A hostile IR op that wgpu
    /// itself rejects — a draw whose vertex/index count overruns the bound buffer, a depth-tested pipeline
    /// drawn in a pass with NO depth attachment, a stencil op on a non-stencil target, an over-large compute
    /// dispatch — would otherwise abort the whole executor. wgpu validates a render/compute pass when the
    /// pass is ENDED (dropped) and again at submit, so the scope must wrap the ENTIRE pass, not just the
    /// submit. This pushes a validation scope, runs `body`, then ALWAYS pops it (balanced even on an early
    /// return from `body`, so the scope stack never leaks): a captured wgpu error becomes `Err` and the
    /// device is NOT lost (a following valid program still runs); `body`'s own typed error takes precedence.
    /// A well-formed program raises no error, so this is a transparent pass-through.
    fn with_validation_scope(&mut self, body: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        self.gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let result = body(self);
        let captured = pollster::block_on(self.gpu.device.pop_error_scope());
        match (result, captured) {
            (Err(e), _) => Err(e),
            (Ok(()), Some(e)) => {
                // wgpu 24's `Display` for a validation error is the bare "Validation Error" — the ACTUAL
                // rule violated (which attachment/pipeline/format) lives in the Debug form and the
                // `std::error::Error::source()` chain, so surface both: `{e:?}` (Debug) plus every cause
                // walked off `source()`. This is what pins the offending pass without a wgpu_core log build.
                let mut chain = String::new();
                let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
                while let Some(s) = src {
                    chain.push_str("\n  caused by: ");
                    chain.push_str(&s.to_string());
                    src = s.source();
                }
                hl_log::hl_error!(
                    tag::EXEC,
                    "wgpu rejected a pass at validation: {e}\n  debug: {e:?}{chain}"
                );
                Err(GpuError::Invalid("wgpu: pass failed device validation"))
            }
            (Ok(()), None) => Ok(()),
        }
    }
}

/// Find the encoder index of the pass-closing op matching the `Begin` at `start`, rejecting an unclosed
/// or nested pass (a validated command buffer never nests, but stay defensive).
fn find_end(ops: &[Enc], start: usize, close: Enc) -> Result<usize> {
    for (k, op) in ops.iter().enumerate().skip(start + 1) {
        if std::mem::discriminant(op) == std::mem::discriminant(&close) {
            return Ok(k);
        }
    }
    Err(GpuError::Invalid("command buffer ends inside an open pass"))
}

mod compute;
mod render;
mod transfer;

#[cfg(test)]
mod tests;
