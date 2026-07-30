//! `glReadPixels` — the GL device→host readback, the GL equivalent of cuda's `cuMemcpyDtoH`.
//!
//! GL is deferred-lowering, so unlike a real driver there is no already-rendered framebuffer to sample
//! when `glReadPixels` is called. This service therefore does what `glFinish`+read would: it lowers the
//! recorded draw-list into the frame's render-target texture ([`crate::service::frame::build_frame_ir`]),
//! submits it, then copies that render-target texture back to a host-readable buffer with a
//! `CopyTextureToBuffer` + [`CommandSink::read_buffer`] — the SAME device→host port cuda's DtoH uses, so
//! the readback works identically over an in-process sink or the socketed `RemoteCommandSink`.
//!
//! The accepted render submission is a completion boundary. Once the sink accepts it, residency and
//! recording state advance exactly once; a later swap cannot replay the same command stream.
//!
//! The copied plane is the target's native texel order (Bgra8 for the default surface, Rgba8 for an
//! offscreen FBO), top-left origin. This service converts it into the requested GL pixel `format`
//! (`GL_RGBA`/`GL_BGRA_EXT`/`GL_RGB`, `UNSIGNED_BYTE`) in GL's bottom-left row order, for the
//! `(x, y, w, h)` rectangle. Callers validate `format`/`type` (only `UNSIGNED_BYTE` is modeled) and
//! null-check the destination before calling in.

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
    // region whose packed size overflows `usize` or exceeds this generous cap is rejected as
    // GL_INVALID_VALUE (never allocated, never an OOB read). Mirrors the CUDA/VK sweep's pre-alloc bounds.
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

    // Copy the whole rendered target back into a host-readable buffer (the device→host port).
    let readback = ctx.alloc_buffer_ir()?;
    let row_bytes = tw as u64 * 4;
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
/// The render pass, device-to-host copy, and authoritative present share one command batch. This avoids
/// replaying the complete draw list once for `glReadPixels` and again for `eglSwapBuffers`. Unlike public
/// `glReadPixels`, this internal presentation path preserves the target's top-left row order and converts
/// directly to XRGB instead of converting to bottom-left RGBA and immediately back again.
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
    let readback = ctx.alloc_buffer_ir()?;
    let row_bytes = tw as u64 * 4;
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

fn xrgb_plane(
    mut raw: Vec<u8>,
    target_width: i32,
    target_height: i32,
    target_format: TextureFormat,
    width: i32,
    height: i32,
) -> Vec<u8> {
    let expected = width.max(0) as usize * height.max(0) as usize * 4;
    if target_width != width || target_height != height || raw.len() < expected {
        return vec![0; expected];
    }
    raw.truncate(expected);
    let rgba = matches!(
        target_format,
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Srgb
    );
    for pixel in raw.chunks_exact_mut(4) {
        if rgba {
            pixel.swap(0, 2);
        }
        pixel[3] = 0xff;
    }
    raw
}

/// Convert the native-format target plane `raw` (tight-packed `tw`×`th`, top-left origin) into the
/// requested GL `format` for the `(x, y, w, h)` rectangle, in GL's bottom-left row order. Texels outside
/// the target read back as zero.
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
    // Bgra targets store [B,G,R,A]; Rgba targets store [R,G,B,A].
    let bgra = matches!(fmt, TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb);
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
            let sp = (ty as usize * tw + tx as usize) * 4;
            if sp + 4 > raw.len() {
                continue;
            }
            let s = &raw[sp..sp + 4];
            let (r, g, b, a) = if bgra {
                (s[2], s[1], s[0], s[3])
            } else {
                (s[0], s[1], s[2], s[3])
            };
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
