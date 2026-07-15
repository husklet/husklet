//! `glReadPixels` — the GL device→host readback, the GL equivalent of cuda's `cuMemcpyDtoH`.
//!
//! GL is deferred-lowering, so unlike a real driver there is no already-rendered framebuffer to sample
//! when `glReadPixels` is called. This service therefore does what `glFinish`+read would: it lowers the
//! recorded draw-list into the frame's render-target texture ([`crate::service::frame::build_frame_ir`]),
//! submits it, then copies that render-target texture back to a host-readable buffer with a
//! `CopyTextureToBuffer` + [`CommandSink::read_buffer`] — the SAME device→host port cuda's DtoH uses, so
//! the readback works identically over an in-process sink or the socketed `RemoteCommandSink`.
//!
//! Unlike `eglSwapBuffers`, reading pixels is NOT a frame boundary: the draw-list is left intact so a
//! later `eglSwapBuffers` still presents the same frame (`glReadPixels` observes, it does not consume).
//!
//! The copied plane is the target's native texel order (Bgra8 for the default surface, Rgba8 for an
//! offscreen FBO), top-left origin. This service converts it into the requested GL pixel `format`
//! (`GL_RGBA`/`GL_BGRA_EXT`/`GL_RGB`, `UNSIGNED_BYTE`) in GL's bottom-left row order, for the
//! `(x, y, w, h)` rectangle. Callers validate `format`/`type` (only `UNSIGNED_BYTE` is modeled) and
//! null-check the destination before calling in.

use crate::model::context::GlContext;
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
pub fn format_bpp(format: u32) -> usize {
    if format == GL_RGB {
        3
    } else {
        4
    }
}

/// `glReadPixels(x, y, w, h, format, GL_UNSIGNED_BYTE, dst)` — render the recorded frame and read the
/// `(x, y, w, h)` rectangle of the resulting render target back, tight-packed in `format`. Returns the
/// packed bytes (`w*h*bpp`); an empty region or a frame with nothing to render yields a zero-filled
/// buffer (matching a readback of an untouched default framebuffer).
pub fn read_pixels(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    format: u32,
) -> Result<Vec<u8>> {
    let _s = hl_log::hl_span!(hl_log::tag::PRESENT, "readpixels");
    let bpp = format_bpp(format);
    if w <= 0 || h <= 0 {
        return Ok(Vec::new());
    }
    let out_len = w as usize * h as usize * bpp;

    // Lower + render the recorded frame into its render-target texture. No draws → default-framebuffer
    // readback yields zeros (the model keeps no default-color plane), mirroring gl_shim.c.
    let Some(mut f) = frame::build_frame_ir(ctx) else {
        return Ok(vec![0u8; out_len]);
    };
    let (_surface, mut texture) = f.present;
    // Multiple-render-target frame: honor `glReadBuffer(GL_COLOR_ATTACHMENT{i})` — read the SELECTED
    // attachment's texture, not just `present` (attachment 0). `read_buffer_src` is a GL_COLOR_ATTACHMENT*
    // enum (else GL_BACK for the default framebuffer, which keeps `present`).
    if !f.color_attachments.is_empty() {
        const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
        let src = ctx.read_buffer_src;
        if (GL_COLOR_ATTACHMENT0..=GL_COLOR_ATTACHMENT0 + 15).contains(&src) {
            let idx = (src - GL_COLOR_ATTACHMENT0) as usize;
            if let Some(&t) = f.color_attachments.get(idx) {
                texture = t;
            }
        }
    }
    let (tw, th, fmt) = (f.target_width, f.target_height, f.target_format);
    if tw <= 0 || th <= 0 {
        return Ok(vec![0u8; out_len]);
    }

    // Copy the whole rendered target back into a host-readable buffer (the device→host port).
    let readback = ctx.alloc_buffer_ir();
    let row_bytes = tw as u64 * 4;
    let size = row_bytes * th as u64;
    f.cmds.push(Cmd::CreateBuffer(
        readback,
        BufferDesc { size, usage: buffer_usage::COPY_DST, label: String::new() },
    ));
    f.cmds.push(Cmd::Submit(CommandBuffer {
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

    sink.submit(&f.cmds)?;
    let raw = sink.read_buffer(BufferId(readback), 0, size as usize)?;
    hl_log::hl_add!(hl_log::tag::PRESENT, "readback_bytes", raw.len() as u64);

    Ok(pack_region(&raw, tw, th, fmt, x, y, w, h, format, bpp))
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
            let (r, g, b, a) = if bgra { (s[2], s[1], s[0], s[3]) } else { (s[0], s[1], s[2], s[3]) };
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
