//! `glReadPixels` — the GL device→host readback, the GL equivalent of cuda's `cuMemcpyDtoH`.
//!
//! GL is deferred-lowering, so unlike a real driver there is no already-rendered framebuffer to sample when
//! `glReadPixels` is called. This service therefore does what `glFinish`+read would: it lowers the recorded
//! draw-list into the frame's render-target texture ([`crate::service::frame::build_frame_ir`]), submits it,
//! then copies that render-target texture back to a host-readable buffer with a
//! `CopyTextureToBuffer` + [`CommandSink::read_buffer`] — the SAME device→host port cuda's DtoH uses, so it
//! works identically over an in-process sink or the socketed `RemoteCommandSink`.
//!
//! The accepted render submission is a completion boundary: once the sink accepts it, residency and
//! recording state advance exactly once, and a later swap cannot replay the same command stream.
//! `glReadPixels` is NOT, however, a FRAME boundary — `eglSwapBuffers` still has to post the default
//! framebuffer's contents. So when this path renders a window's default framebuffer it marks the frame
//! deferred-present ([`GlContext::defer_default_present`]) and the swap presents the resident target
//! instead of replaying the draw-list.
//!
//! The copied plane is the target's native texel order (Bgra8 for the default surface, Rgba8 for an
//! offscreen FBO), with rows top-down — the contract on `RenderPasses::stores_bottom_up_rows`. So exactly
//! ONE row flip belongs on the `glReadPixels` path ([`pack_region`], which packs GL's bottom-left rows in
//! the requested `GL_RGBA`/`GL_BGRA_EXT`/`GL_RGB` `UNSIGNED_BYTE` format) and NONE on the `wl_shm` present
//! path ([`xrgb_plane`], whose buffer is top-down too). Callers validate `format`/`type` and the pointer.

use crate::model::context::GlContext;
use crate::model::glconst::GL_INVALID_FRAMEBUFFER_OPERATION;
use crate::service::frame;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::{buffer_usage, TextureFormat};
use hl_gpu::{BufferId, Cmd, CommandBuffer, CommandSink, Result};

// GL pixel formats this readback packs into (the caller has already validated `format`/`type`). `GL_RGBA`
// is the packing default (the `_` arm in `pack_region`), so only the two that need special handling are named.
const GL_RGB: u32 = 0x1907;
const GL_BGRA_EXT: u32 = 0x80E1;

/// Bytes per packed pixel for a supported readback `format` (`GL_RGB` = 3, else 4).
struct PixelFormat(u32);

impl PixelFormat {
    fn bytes_per_pixel(self) -> usize {
        if self.0 == GL_RGB {
            3
        } else {
            4
        }
    }
}

/// One texel of the RENDER TARGET being read back, and how to read it as the RGBA bytes `glReadPixels`
/// packs.
///
/// This is a DIFFERENT plane from the texture model's CPU shadow and does not share its rules. The shadow
/// is a four-channel eight-bit image for every narrow format by construction, so its texel size takes a
/// floor of four; a render target's plane is whatever the executor allocated for the format, which is the
/// format's true texel with no floor at all (`Format::copy_layout` derives the copy row from exactly
/// that). Applying the shadow's floor here is not conservative, it is wrong in both directions: it
/// over-states the row for a one-byte target, so the readback walks into inter-row padding, and it
/// under-states the row for a half-float target, so the copy is refused as out of bounds.
#[derive(Clone, Copy, Debug)]
struct TargetTexel(TextureFormat);

impl TargetTexel {
    /// The plane's true bytes per texel, or `None` for a format that has no plain-colour texel — the
    /// depth/stencil and block-compressed formats. A readback cannot compute a row for those, and
    /// answering four would make "could not describe this format" indistinguishable from a real width.
    fn bytes(self) -> Option<usize> {
        self.0.bytes_per_texel()
    }

    /// One texel decoded to straight RGBA bytes.
    ///
    /// The narrowing of a float target to eight bits is REQUIRED here, unlike in the upload direction
    /// where it was the defect: ES 3.0 §4.3.1 says a `glReadPixels` into `GL_UNSIGNED_BYTE` clamps to
    /// `[0, 1]` and converts to unsigned normalized. Channels the target format does not carry read as
    /// zero, and an absent alpha reads as one, which is the same rule the sampler applies.
    fn rgba8(self, texel: &[u8]) -> [u8; 4] {
        let byte = |index: usize| texel.get(index).copied().unwrap_or(0);
        let unorm = |value: f32| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let float = |index: usize| {
            texel
                .get(index * 4..index * 4 + 4)
                .and_then(|bytes| bytes.try_into().ok())
                .map_or(0.0, f32::from_le_bytes)
        };
        let half = |index: usize| {
            texel
                .get(index * 2..index * 2 + 2)
                .and_then(|bytes| bytes.try_into().ok())
                .map_or(0.0, |bytes| crate::service::half::to_f32(u16::from_le_bytes(bytes)))
        };
        match self.0 {
            // Bgra targets store [B,G,R,A]; every other eight-bit target stores its channels in order.
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb => {
                [byte(2), byte(1), byte(0), byte(3)]
            }
            TextureFormat::R8Unorm | TextureFormat::R8Uint | TextureFormat::R8Sint => {
                [byte(0), 0, 0, 0xff]
            }
            TextureFormat::Rg8Unorm | TextureFormat::Rg8Uint | TextureFormat::Rg8Sint => {
                [byte(0), byte(1), 0, 0xff]
            }
            TextureFormat::R32Float => [unorm(float(0)), 0, 0, 0xff],
            TextureFormat::Rgba16Float => [
                unorm(half(0)),
                unorm(half(1)),
                unorm(half(2)),
                unorm(half(3)),
            ],
            TextureFormat::Rgba32Float => [
                unorm(float(0)),
                unorm(float(1)),
                unorm(float(2)),
                unorm(float(3)),
            ],
            _ => [byte(0), byte(1), byte(2), byte(3)],
        }
    }
}

/// `glReadPixels(x, y, w, h, format, GL_UNSIGNED_BYTE, dst)` — render the recorded frame and read the
/// `(x, y, w, h)` rectangle of the resulting render target back, tight-packed in `format`. Returns the
/// packed bytes (`w*h*bpp`); an empty region or a frame with nothing to render yields a zero-filled
/// buffer (matching a readback of an untouched default framebuffer).
pub struct PreparedPixels {
    bytes: Vec<u8>,
    packing: Option<Packing>,
}

struct Packing {
    target: (i32, i32, TextureFormat),
    region: (i32, i32, i32, i32),
    format: u32,
    bpp: usize,
}

impl PreparedPixels {
    fn empty(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            packing: None,
        }
    }

    pub fn complete(self, raw: Option<Vec<u8>>) -> Vec<u8> {
        let Some(packing) = self.packing else {
            return self.bytes;
        };
        let Some(raw) = raw else {
            return self.bytes;
        };
        let (tw, th, target_format) = packing.target;
        let (x, y, w, h) = packing.region;
        pack_region(
            &raw,
            tw,
            th,
            target_format,
            x,
            y,
            w,
            h,
            packing.format,
            packing.bpp,
        )
    }
}

pub fn read_pixels(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    format: u32,
) -> Result<Vec<u8>> {
    prepare_pixels(ctx, sink, x, y, w, h, format).map(|prepared| prepared.bytes)
}

pub fn prepare_pixels(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    format: u32,
) -> Result<PreparedPixels> {
    let _s = hl_log::hl_span!(hl_log::tag::PRESENT, "readpixels");
    let bpp = PixelFormat(format).bytes_per_pixel();
    if w <= 0 || h <= 0 {
        return Ok(PreparedPixels::empty(Vec::new()));
    }
    // Bound the packed-region allocation: a hostile (or overflowing) `w*h*bpp` must never trigger an
    // unbounded host allocation. The largest legitimate readback is the full render target (≤ 16384²), so a
    // region whose packed size overflows `usize` or exceeds this cap is rejected as GL_INVALID_VALUE (never
    // allocated, never an OOB read). Mirrors the CUDA/VK sweep's pre-alloc bounds.
    const MAX_READBACK_BYTES: usize = 1 << 30; // 1 GiB
    let out_len = match (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(bpp))
    {
        Some(n) if n <= MAX_READBACK_BYTES => n,
        _ => {
            ctx.set_gl_error(crate::model::glconst::GL_INVALID_VALUE);
            return Ok(PreparedPixels::empty(Vec::new()));
        }
    };

    // Lower + render pending work, or read the persistent target produced by an earlier glFlush/glFinish.
    // Chrome's accelerated Canvas path flushes its Skia FBO before getImageData; treating the then-empty
    // draw list as an untouched framebuffer returns transparent black despite the resident target holding
    // the rendered pixels.
    let frame_state = ctx.frame_state();
    let had_pending_work =
        !ctx.local.recording.draws.is_empty() || !ctx.local.recording.blits.is_empty();
    const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
    let attachment = ctx
        .local
        .read_buffer_src
        .checked_sub(GL_COLOR_ATTACHMENT0)
        .filter(|index| *index < 16)
        .unwrap_or(0);
    let requested = (ctx.local.read_fbo != 0).then(|| {
        let name = ctx
            .local
            .framebuffers
            .color_attachment_index(ctx.local.read_fbo, attachment);
        let target = ctx.textures.get(name)?;
        Some((name, target.gen, target.w, target.h, target.ir_format))
    });
    let requested = requested.flatten();
    if ctx.local.read_fbo != 0 && requested.is_none() {
        ctx.set_gl_error(GL_INVALID_FRAMEBUFFER_OPERATION);
        return Ok(PreparedPixels::empty(vec![0u8; out_len]));
    }
    let mut accepted_targets = Vec::new();
    let mut retained_shared = Vec::new();
    let mut submitted_frame = false;
    let mut defer_present = false;
    let (mut cmds, texture, tw, th, fmt) = if let Some(mut f) = frame::Frame::build(ctx) {
        f.cmds.extend_from_slice(ctx.pending_destroys());
        retained_shared = ctx.retain_shared_targets(&mut f);
        let selected = requested.and_then(|(name, generation, width, height, format)| {
            let current = f
                .targets
                .iter()
                .rev()
                .find(|target| target.name == name && target.generation == generation)
                .map(|target| target.texture);
            let resident = ctx.resident_fbo_target_tex(name, generation);
            let texture = current.or(resident)?;
            Some((texture, width, height, format))
        });
        let (texture, tw, th, fmt) = if ctx.local.read_fbo == 0 {
            if ctx.default_surfaces_match() {
                // GL: `glReadPixels` returns previously issued commands' results but is NOT a frame
                // boundary, and `eglSwapBuffers` posts the default framebuffer's contents. Rendering here
                // consumes the draw-list (the frame must execute exactly once), so the swap has to present
                // the target this render leaves resident. Only a window surface swaps.
                defer_present =
                    ctx.local.surface_kind == crate::model::context::SurfaceKind::Window;
                (
                    f.present.1,
                    f.target_width,
                    f.target_height,
                    f.target_format,
                )
            } else if let Some(read) = ctx.resident_default_read_target() {
                read
            } else {
                if let Err(error) = sink.submit(&f.cmds) {
                    ctx.restore_frame_state(frame_state);
                    return Err(error);
                }
                ctx.clear_pending_destroys();
                ctx.accept_targets(&f.targets);
                ctx.own_shared_targets(&retained_shared);
                ctx.reset_frame();
                ctx.prune_shared_textures();
                return Ok(PreparedPixels::empty(vec![0u8; out_len]));
            }
        } else if let Some(selected) = selected {
            selected
        } else {
            ctx.restore_frame_state(frame_state);
            return Ok(PreparedPixels::empty(vec![0u8; out_len]));
        };
        accepted_targets = f.targets;
        submitted_frame = true;
        (f.cmds, texture, tw, th, fmt)
    } else {
        ctx.restore_frame_state(frame_state.clone());
        if had_pending_work {
            return Ok(PreparedPixels::empty(vec![0u8; out_len]));
        }
        let Some((texture, tw, th, fmt)) =
            ctx.resident_fbo_read_target(ctx.local.read_fbo, attachment)
        else {
            return Ok(PreparedPixels::empty(vec![0u8; out_len]));
        };
        (Vec::new(), texture, tw, th, fmt)
    };
    if tw <= 0 || th <= 0 {
        ctx.restore_frame_state(frame_state);
        return Ok(PreparedPixels::empty(vec![0u8; out_len]));
    }

    // Copy the whole rendered target back into a host-readable buffer (the device→host port). The row is
    // the TARGET's own texel, not four bytes: the executor derives the copy's tight row from the texture's
    // format, so a row stated at four bytes for a one-byte target leaves the plane interleaved with
    // padding this path then reads as pixels, and a row stated at four bytes for a half-float target is
    // shorter than the tight row and the copy is refused.
    let Some(target_texel) = TargetTexel(fmt).bytes() else {
        // A depth/stencil or block-compressed colour attachment has no plain-colour texel to pack from.
        ctx.restore_frame_state(frame_state);
        ctx.set_gl_error(crate::model::glconst::GL_INVALID_OPERATION);
        return Ok(PreparedPixels::empty(vec![0u8; out_len]));
    };
    let readback = ctx.alloc_buffer_ir()?;
    let row_bytes = tw as u64 * target_texel as u64;
    let size = row_bytes * th as u64;
    cmds.push(Cmd::CreateBuffer(
        readback,
        BufferDesc {
            size,
            usage: buffer_usage::COPY_DST,
            label: String::new(),
        },
    ));
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyTextureToBuffer {
            src: texture,
            mip: 0,
            width: tw as u32,
            height: th as u32,
            dst: readback,
            dst_offset: 0,
            bytes_per_row: row_bytes as u32,
        }],
        signal: None,
    }));

    if let Err(error) = sink.submit(&cmds) {
        ctx.restore_frame_state(frame_state);
        return Err(error);
    }
    if submitted_frame {
        ctx.clear_pending_destroys();
        ctx.accept_targets(&accepted_targets);
        ctx.own_shared_targets(&retained_shared);
        ctx.reset_frame();
        ctx.prune_shared_textures();
        if defer_present {
            ctx.defer_default_present();
        }
    }
    hl_log::hl_debug!(
        hl_log::tag::PRESENT,
        "readback reason=gl_read_pixels targets=1 bytes={size}"
    );
    hl_log::hl_count!(hl_log::tag::PRESENT, "readback_gl_read_pixels");
    let read = sink.read_buffer(BufferId(readback), 0, size as usize);
    ctx.queue_buffer_destroy(readback);
    let cleanup = ctx.pending_destroys().to_vec();
    let cleanup_result = sink.submit(&cleanup);
    if cleanup_result.is_ok() {
        ctx.clear_pending_destroys();
    }
    let raw = match (read, cleanup_result) {
        (Ok(raw), Ok(())) => raw,
        (Err(error), _) => return Err(error),
        (Ok(_), Err(error)) => return Err(error),
    };
    hl_log::hl_add!(hl_log::tag::PRESENT, "readback_bytes", raw.len() as u64);

    Ok(PreparedPixels {
        bytes: pack_region(&raw, tw, th, fmt, x, y, w, h, format, bpp),
        packing: Some(Packing {
            target: (tw, th, fmt),
            region: (x, y, w, h),
            format,
            bpp,
        }),
    })
}

/// Present a window frame and return the XRGB8888 plane needed by the `wl_shm` compatibility path.
///
/// The render pass, device-to-host copy, and authoritative present share one command batch, so the draw list
/// is not replayed once for `glReadPixels` and again for `eglSwapBuffers`. Unlike public `glReadPixels`, this
/// path keeps the target's top-down rows and converts straight to XRGB — no flip, and no flip back.
pub fn swap_xrgb(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    w: i32,
    h: i32,
) -> Result<Option<Vec<u8>>> {
    let prepared = prepare_swap_xrgb(ctx, sink, w, h)?;
    Ok(prepared.complete(None))
}

pub struct PreparedSwap {
    pixels: Option<Vec<u8>>,
    packing: Option<(i32, i32, TextureFormat, i32, i32)>,
}

impl PreparedSwap {
    pub fn complete(self, raw: Option<Vec<u8>>) -> Option<Vec<u8>> {
        let Some((tw, th, format, width, height)) = self.packing else {
            return self.pixels;
        };
        raw.map(|bytes| xrgb_plane(bytes, tw, th, format, width, height))
            .or(self.pixels)
    }
}

pub fn prepare_swap_xrgb(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    w: i32,
    h: i32,
) -> Result<PreparedSwap> {
    let frame_state = ctx.frame_state();
    let Some(mut frame) = frame::Frame::build(ctx) else {
        ctx.restore_frame_state(frame_state);
        if ctx.has_pending_destroys() {
            let destroys = ctx.pending_destroys().to_vec();
            sink.submit(&destroys)?;
            ctx.clear_pending_destroys();
        }
        ctx.reset_frame();
        return Ok(PreparedSwap {
            pixels: None,
            packing: None,
        });
    };

    let (surface, texture) = frame.present;
    let (tw, th, target_format) = (frame.target_width, frame.target_height, frame.target_format);
    // As in `prepare_pixels`: the copy row is the target's own texel. A window surface is Bgra8 today, so
    // this is four in practice — but the value has to come from the format, or this path acquires the
    // same latent defect the moment a surface is ever allocated in anything else.
    let Some(target_texel) = TargetTexel(target_format).bytes() else {
        ctx.restore_frame_state(frame_state);
        return Ok(PreparedSwap {
            pixels: None,
            packing: None,
        });
    };
    let readback = ctx.alloc_buffer_ir()?;
    let row_bytes = tw as u64 * target_texel as u64;
    let size = row_bytes * th as u64;
    frame.cmds.push(Cmd::CreateBuffer(
        readback,
        BufferDesc {
            size,
            usage: buffer_usage::COPY_DST,
            label: String::new(),
        },
    ));
    frame.cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyTextureToBuffer {
            src: texture,
            mip: 0,
            width: tw as u32,
            height: th as u32,
            dst: readback,
            dst_offset: 0,
            bytes_per_row: row_bytes as u32,
        }],
        signal: None,
    }));
    frame.cmds.extend_from_slice(ctx.pending_destroys());
    if ctx.local.present_token.is_some() && surface != 0 {
        frame.cmds.push(Cmd::Present {
            surface,
            texture,
            serial: ctx
                .local
                .present_serial
                .expect("native presentation carries a frame serial"),
        });
    }

    if let Err(error) = sink.submit(&frame.cmds) {
        ctx.restore_frame_state(frame_state);
        return Err(error);
    }
    ctx.clear_pending_destroys();
    ctx.reset_frame();

    hl_log::hl_debug!(
        hl_log::tag::PRESENT,
        "readback reason=shm_swap targets=1 bytes={size}"
    );
    hl_log::hl_count!(hl_log::tag::PRESENT, "readback_shm_swap");
    let read = sink.read_buffer(BufferId(readback), 0, size as usize);
    ctx.queue_buffer_destroy(readback);
    let cleanup = ctx.pending_destroys().to_vec();
    let cleanup_result = sink.submit(&cleanup);
    if cleanup_result.is_ok() {
        ctx.clear_pending_destroys();
    }
    let raw = match (read, cleanup_result) {
        (Ok(raw), Ok(())) => raw,
        (Err(error), _) => {
            hl_log::hl_warn!(
                hl_log::tag::PRESENT,
                "presented frame readback failed: {error}"
            );
            return Ok(PreparedSwap {
                pixels: None,
                packing: None,
            });
        }
        (Ok(_), Err(error)) => return Err(error),
    };
    hl_log::hl_add!(hl_log::tag::PRESENT, "readback_bytes", raw.len() as u64);
    Ok(PreparedSwap {
        pixels: Some(xrgb_plane(raw, tw, th, target_format, w, h)),
        packing: Some((tw, th, target_format, w, h)),
    })
}

/// Pack the target plane as `WL_SHM_FORMAT_XRGB8888` — `[B, G, R, X]` in memory, opaque.
///
/// The source stride is the target's own texel; the output is always four bytes a pixel because the
/// `wl_shm` format is. Those were the same number while every target was eight-bit four-channel, and
/// conflating them is what made this path read a one-byte plane as if it were four.
fn xrgb_plane(
    raw: Vec<u8>,
    target_width: i32,
    target_height: i32,
    target_format: TextureFormat,
    width: i32,
    height: i32,
) -> Vec<u8> {
    let pixels = width.max(0) as usize * height.max(0) as usize;
    let expected = pixels * 4;
    let target = TargetTexel(target_format);
    let Some(texel) = target.bytes() else {
        return vec![0; expected];
    };
    if target_width != width || target_height != height || raw.len() < pixels * texel {
        return vec![0; expected];
    }
    let mut out = vec![0u8; expected];
    for (index, source) in raw.chunks_exact(texel).take(pixels).enumerate() {
        let [r, g, b, _] = target.rgba8(source);
        out[index * 4..index * 4 + 4].copy_from_slice(&[b, g, r, 0xff]);
    }
    out
}

/// Convert the native-format target plane `raw` (tight-packed `tw`×`th`, rows top-down) into the requested
/// GL `format` for the `(x, y, w, h)` rectangle, in GL's bottom-left rows. Texels outside the target read
/// back as zero. This row flip is the ONE flip the GL readback path applies (see the module header).
#[allow(clippy::too_many_arguments)]
fn pack_region(
    raw: &[u8],
    tw: i32,
    th: i32,
    fmt: TextureFormat,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    format: u32,
    bpp: usize,
) -> Vec<u8> {
    let (tw, th) = (tw as usize, th as usize);
    let tight = w as usize * bpp;
    let mut out = vec![0u8; h as usize * tight];
    let target = TargetTexel(fmt);
    // The source stride is the target plane's own texel, and the channel order is the target format's.
    // Both used to be four bytes of [R,G,B,A] (or [B,G,R,A]), which is right for exactly the eight-bit
    // four-channel targets and wrong for every other one this driver can allocate.
    let Some(texel) = target.bytes() else {
        return out;
    };
    for row in 0..h as usize {
        // Output row `row` is GL scanline `y+row` (from the bottom); in a top-left texture that is
        // texture row `th-1-(y+row)`.
        let ty = th as isize - 1 - (y as isize + row as isize);
        if ty < 0 || ty >= th as isize {
            continue;
        }
        for col in 0..w as usize {
            let tx = x as isize + col as isize;
            if tx < 0 || tx >= tw as isize {
                continue;
            }
            let sp = (ty as usize * tw + tx as usize) * texel;
            if sp + texel > raw.len() {
                continue;
            }
            let [r, g, b, a] = target.rgba8(&raw[sp..sp + texel]);
            let dp = row * tight + col * bpp;
            match format {
                GL_BGRA_EXT => out[dp..dp + 4].copy_from_slice(&[b, g, r, a]),
                GL_RGB => out[dp..dp + 3].copy_from_slice(&[r, g, b]),
                _ => out[dp..dp + 4].copy_from_slice(&[r, g, b, a]), // GL_RGBA
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The readback's source stride is the target format's TRUE texel, with no floor at four.
    ///
    /// The texture model's CPU shadow takes `max(4, texel)` because narrow formats are shadowed as
    /// four-channel eight-bit images. That floor is correct there and wrong here, and it is wrong in BOTH
    /// directions, which is why it is pinned rather than left to a reader to re-derive: four for a
    /// one-byte target reads padding as pixels, and four for a half-float target under-states the tight
    /// row so the executor refuses the copy as out of bounds.
    #[test]
    fn the_target_texel_has_no_floor_and_no_invented_default() {
        assert_eq!(TargetTexel(TextureFormat::R8Unorm).bytes(), Some(1));
        assert_eq!(TargetTexel(TextureFormat::Rg8Unorm).bytes(), Some(2));
        assert_eq!(TargetTexel(TextureFormat::Rgba8Unorm).bytes(), Some(4));
        assert_eq!(TargetTexel(TextureFormat::Bgra8Unorm).bytes(), Some(4));
        assert_eq!(TargetTexel(TextureFormat::R32Float).bytes(), Some(4));
        assert_eq!(TargetTexel(TextureFormat::Rgba16Float).bytes(), Some(8));
        assert_eq!(TargetTexel(TextureFormat::Rgba32Float).bytes(), Some(16));
        // A format with no plain-colour texel answers None rather than four. Four would make "this path
        // cannot describe the format" indistinguishable from a real four-byte target, which is the shape
        // of failure that makes a wrong readback unattributable.
        assert_eq!(TargetTexel(TextureFormat::Depth32Float).bytes(), None);
        assert_eq!(TargetTexel(TextureFormat::Depth24PlusStencil8).bytes(), None);
        assert_eq!(TargetTexel(TextureFormat::Bc1RgbaUnorm).bytes(), None);
    }

    /// Each target format decodes to the RGBA bytes `glReadPixels` packs. The eight-bit rows are the
    /// control: they are what already worked and must be byte-identical.
    #[test]
    fn a_target_texel_decodes_to_the_channels_it_carries() {
        let half = |value: f32| crate::service::half::from_f32(value).to_le_bytes();

        assert_eq!(
            TargetTexel(TextureFormat::Rgba8Unorm).rgba8(&[1, 2, 3, 4]),
            [1, 2, 3, 4],
            "an RGBA8 target is passed through"
        );
        assert_eq!(
            TargetTexel(TextureFormat::Bgra8Unorm).rgba8(&[1, 2, 3, 4]),
            [3, 2, 1, 4],
            "a BGRA8 target is swizzled, exactly as before"
        );
        assert_eq!(
            TargetTexel(TextureFormat::R8Unorm).rgba8(&[200]),
            [200, 0, 0, 0xff],
            "a one-channel target reads green and blue as zero and alpha as one"
        );
        assert_eq!(
            TargetTexel(TextureFormat::Rg8Unorm).rgba8(&[200, 100]),
            [200, 100, 0, 0xff],
            "a two-channel target reads blue as zero and alpha as one"
        );

        // ES 3.0 §4.3.1: a readback into GL_UNSIGNED_BYTE clamps to [0,1] and converts to unsigned
        // normalized. Driven at the endpoints, because a mid-range value survives almost any arithmetic.
        let mut texel = Vec::new();
        for value in [1.0f32, 0.0, 4.0, -2.5] {
            texel.extend_from_slice(&half(value));
        }
        assert_eq!(
            TargetTexel(TextureFormat::Rgba16Float).rgba8(&texel),
            [255, 0, 255, 0],
            "a half-float target clamps out of range and scales in range"
        );

        let mut texel = Vec::new();
        for value in [0.5f32, 1.0, 0.0, 65504.0] {
            texel.extend_from_slice(&value.to_le_bytes());
        }
        assert_eq!(
            TargetTexel(TextureFormat::Rgba32Float).rgba8(&texel),
            [128, 255, 0, 255],
            "a float target rounds half up, as the clear-colour packing does"
        );
        assert_eq!(
            TargetTexel(TextureFormat::R32Float).rgba8(&1.0f32.to_le_bytes()),
            [255, 0, 0, 0xff],
            "a single-channel float target is one float, not four bytes of colour"
        );
    }

    /// `pack_region` walks the source at the target's texel and the destination at the packed one, and
    /// still applies exactly one row flip. A one-byte target is the case that used to read its
    /// neighbours' bytes; the RGBA8 target beside it is the control that must not move.
    #[test]
    fn pack_region_strides_the_source_by_the_target_texel() {
        // A 2x2 R8 target whose four texels are distinguishable, so a wrong stride cannot coincide.
        let raw = [10u8, 20, 30, 40];
        let packed = pack_region(&raw, 2, 2, TextureFormat::R8Unorm, 0, 0, 2, 2, 0x1908, 4);
        assert_eq!(
            packed,
            // GL row 0 is the BOTTOM, which is target row 1: texels 30 and 40.
            [30, 0, 0, 255, 40, 0, 0, 255, 10, 0, 0, 255, 20, 0, 0, 255],
            "each output pixel is one source byte, flipped into GL's bottom-left rows"
        );

        // The control: the same call shape over an RGBA8 target is unchanged.
        let raw: Vec<u8> = (0u8..16).collect();
        let packed = pack_region(&raw, 2, 2, TextureFormat::Rgba8Unorm, 0, 0, 2, 2, 0x1908, 4);
        assert_eq!(
            packed,
            [8, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5, 6, 7],
            "an RGBA8 target packs exactly as it always did"
        );

        // A format with no plain-colour texel yields the zero-filled rectangle rather than reading bytes
        // it cannot interpret.
        let packed = pack_region(&raw, 2, 2, TextureFormat::Depth32Float, 0, 0, 2, 2, 0x1908, 4);
        assert_eq!(packed, vec![0u8; 16], "an undescribable target packs zeros");
    }

    /// The `wl_shm` present path strides its source by the target texel too, and always emits four bytes
    /// a pixel because `WL_SHM_FORMAT_XRGB8888` is four bytes a pixel. Those were the same number while
    /// every target was eight-bit four-channel.
    #[test]
    fn xrgb_plane_strides_the_source_by_the_target_texel() {
        // Control: a BGRA8 target, the format a window surface actually uses, is unchanged.
        let raw = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(
            xrgb_plane(raw, 2, 1, TextureFormat::Bgra8Unorm, 2, 1),
            [1, 2, 3, 0xff, 5, 6, 7, 0xff],
            "a BGRA8 target is already [B,G,R,_] and only its alpha is forced opaque"
        );

        // An RGBA8 target is swizzled into XRGB order, as before.
        let raw = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(
            xrgb_plane(raw, 2, 1, TextureFormat::Rgba8Unorm, 2, 1),
            [3, 2, 1, 0xff, 7, 6, 5, 0xff],
            "an RGBA8 target is swizzled to [B,G,R,X]"
        );

        // A one-byte target: two source bytes become two four-byte pixels, where the old path read the
        // first four bytes of the plane as a single pixel and ran off the end of a short plane.
        assert_eq!(
            xrgb_plane(vec![200u8, 100], 2, 1, TextureFormat::R8Unorm, 2, 1),
            [0, 0, 200, 0xff, 0, 0, 100, 0xff],
            "a one-channel target contributes red only"
        );
    }
}
