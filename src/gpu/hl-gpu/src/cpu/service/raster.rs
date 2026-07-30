//! Render-pass execution: attachment clears, `ClearRect`, and the software draw path (triangle
//! rasterization with premultiplied linear-light source-over blending). Ported from the render arms of
//! `SoftwareBackend::submit`, `clear_rect`, `raster_draw`, `exec_draw`, `exec_draw_indexed`,
//! `raster_state`, and the draw-vertex helpers in `hl-gpu/src/software.rs`.

use crate::cpu::format::{load_texel_linear, store_texel_linear};
use crate::cpu::model::pipeline::Pipeline;
use crate::cpu::model::{pipeline, texture, texture_mut};
use crate::protocol::model::descriptor::{BlendState, DepthState, StencilFaceState};
use crate::protocol::model::enums::{compare, stencil_op, IndexFormat, TextureFormat, Topology};
use crate::protocol::model::error::{GpuError, Result};
use crate::runtime::model::resources::SessionResources;

mod clear;
mod draw;
mod fragment;

pub(crate) use clear::{clear_depth_stencil_target, clear_rect, clear_target};
pub(crate) use draw::{exec_draw, exec_draw_indexed};

/// Hard ceiling on a draw's `instance_count` — the raster analogue of
/// [`MAX_DISPATCH_BLOCKS`](crate::cpu::service::compute::MAX_DISPATCH_BLOCKS).
///
/// The instance loop re-rasterizes the whole primitive set once per instance, and `instance_count` is
/// bounds-checked only when some vertex layout is per-instance; otherwise a maximal count means ~4 billion
/// full-framebuffer rasterizations. That is a pure CPU-time denial of service — it grows no allocation, so
/// no residency ceiling notices it.
///
/// It is a RUNTIME cap only — no wire bytes change — set far above any legitimate draw: the largest
/// instance count anywhere in this workspace is 40, and browser-class instanced rendering runs to thousands,
/// so `1 << 20` (1,048,576) leaves at least three orders of magnitude of headroom. Rejected in the
/// pre-execution validation pass so an over-cap draw leaves no partial raster behind.
pub(crate) const MAX_DRAW_INSTANCES: u32 = 1 << 20;
use fragment::{barycentric, edge, ndc_to_fb, read_vertex, tri_bbox, write_fragment, DrawVertex};

/// Rasterize one draw's assembled triangles into every bound color attachment, compositing with
/// premultiplied source-over performed in LINEAR light (sRGB targets decode/encode around the blend; a
/// target whose blend is `None` gets an opaque replace).
#[allow(clippy::too_many_arguments)]
fn raster_draw(
    res: &mut SessionResources,
    targets: &[(u32, TextureFormat)],
    blends: &[Option<BlendState>],
    write_masks: &[u32],
    topology: Topology,
    verts: &[DrawVertex],
    depth: Option<(u32, DepthState)>,
    stencil_ref: u32,
    cull: u32,
    front_face: u32,
) -> Result<()> {
    let tris: Vec<[usize; 3]> = match topology {
        Topology::TriangleList => (0..verts.len() / 3)
            .map(|t| [3 * t, 3 * t + 1, 3 * t + 2])
            .collect(),
        Topology::TriangleStrip => (0..verts.len().saturating_sub(2))
            .map(|i| {
                if i % 2 == 0 {
                    [i, i + 1, i + 2]
                } else {
                    [i + 1, i, i + 2]
                }
            })
            .collect(),
        _ => return Ok(()),
    };
    if tris.is_empty() {
        return Ok(());
    }

    match depth {
        Some((depth_tex, state)) => raster_draw_depth(
            res,
            targets,
            blends,
            write_masks,
            &tris,
            verts,
            depth_tex,
            state,
            stencil_ref,
            cull,
            front_face,
        ),
        None => raster_draw_no_depth(
            res,
            targets,
            blends,
            write_masks,
            &tris,
            verts,
            cull,
            front_face,
        ),
    }
}

/// Face-cull decision for a triangle whose framebuffer-space signed area is `area` (`edge(fb0,fb1,fb2)`,
/// computed after [`ndc_to_fb`] — the SAME y-down viewport transform the wgpu executor rasterizes through).
/// `front_face` (0 = CCW, 1 = CW) classifies the winding into front/back, and `cull` (1 = front, 2 = back)
/// drops that facing. `cull == 0` (None) — the neutral wire default — and a neutral `front_face` keep every
/// triangle, byte-for-byte the pre-cull behavior. Degenerate (`area == 0`) triangles are already dropped by
/// the caller before this is consulted.
///
/// Sign convention: a triangle whose vertices are counter-clockwise in NDC (positive NDC signed area) is
/// FRONT-facing under `front_face == 0` (CCW). Because `ndc_to_fb` flips Y, that CCW-in-NDC winding has a
/// NEGATIVE framebuffer-space `area` here — so CCW-front ⇔ `area < 0`. This matches the wgpu executor,
/// which (like GL/WebGPU) determines facing from the NDC winding via its y-flipped viewport, NOT from the
/// raw y-down framebuffer sign. The convention is PINNED by the differential fuzzer
/// (`hl-gpu-wgpu/tests/differential.rs` `gen_cull_*`): those programs draw fullscreen triangles of KNOWN
/// winding under both `front_face` values and both cull faces and assert the oracle culls EXACTLY the same
/// triangles the executor does (a flipped sign inverts every cull decision — 40 divergences until matched).
fn culled(area: f32, cull: u32, front_face: u32) -> bool {
    let front_facing = if front_face == 1 {
        area > 0.0
    } else {
        area < 0.0
    };
    match cull {
        1 => front_facing,  // cull front faces
        2 => !front_facing, // cull back faces
        _ => false,         // cull none
    }
}

/// The depth-less fixed-function path (unchanged semantics): within one draw the first triangle to cover a
/// pixel wins (`covered` mask), and across draws a later draw overwrites an earlier one (painter's order).
#[allow(clippy::too_many_arguments)]
fn raster_draw_no_depth(
    res: &mut SessionResources,
    targets: &[(u32, TextureFormat)],
    blends: &[Option<BlendState>],
    write_masks: &[u32],
    tris: &[[usize; 3]],
    verts: &[DrawVertex],
    cull: u32,
    front_face: u32,
) -> Result<()> {
    for (ti, (tex_id, fmt)) in targets.iter().enumerate() {
        let order = fmt.rgba_channel_order().ok_or(GpuError::Unsupported(
            "software: draw into a non-4-channel color format",
        ))?;
        let srgb = fmt.is_srgb();
        let blend_enabled = blends.get(ti).map(|b| b.is_some()).unwrap_or(false);
        let write_mask = write_masks.get(ti).copied().unwrap_or(0xF);
        let (w, h, bpt) = {
            let t = texture(res, *tex_id)?;
            (
                t.desc.width as usize,
                t.desc.height as usize,
                t.desc.format.software_texel_bytes()?,
            )
        };
        if w == 0 || h == 0 {
            continue;
        }
        let mut covered = vec![false; w * h];
        let t = texture_mut(res, *tex_id)?;
        for tri in tris {
            let v = [verts[tri[0]], verts[tri[1]], verts[tri[2]]];
            let fb = [
                ndc_to_fb(v[0].pos, w, h),
                ndc_to_fb(v[1].pos, w, h),
                ndc_to_fb(v[2].pos, w, h),
            ];
            let area = edge(fb[0], fb[1], fb[2]);
            if area == 0.0 || culled(area, cull, front_face) {
                continue;
            }
            let (minx, miny, maxx, maxy) = tri_bbox(&fb, w, h);
            for py in miny..maxy {
                for px in minx..maxx {
                    let idx = py * w + px;
                    if covered[idx] {
                        continue;
                    }
                    let c = [px as f32 + 0.5, py as f32 + 0.5];
                    let bary = match barycentric(&fb, c, area) {
                        Some(b) => b,
                        None => continue,
                    };
                    let src = DrawVertex::interpolate_color(&v, bary);
                    let texel = &mut t.pixels[idx * bpt..idx * bpt + bpt];
                    write_fragment(texel, order, srgb, *fmt, blend_enabled, write_mask, src)?;
                    covered[idx] = true;
                }
            }
        }
    }
    Ok(())
}

/// The depth/stencil-tested path: interpolate per-fragment `z` from the vertex positions, run the STENCIL
/// test (front-face state) and the DEPTH test against the render pass's depth/stencil buffer, apply the
/// pipeline's stencil op (masked by `stencilWriteMask`), and — when BOTH tests pass — write the color to
/// every target and, when `depth_write` is set, store the new `z`. Fragment ordering is governed by the
/// tests, not by draw/triangle order, so a nearer fragment wins an overlap regardless of draw order. The
/// depth (`Depth32Float`) or depth+stencil (`Depth24PlusStencil8`) buffer is read once, updated in place,
/// and written back.
///
/// STENCIL MODEL (matches `wgpu`/Vulkan): for a `Depth24PlusStencil8` attachment the oracle keeps an 8-bit
/// stencil value per texel (byte 4 of the 8-byte plane texel). Per covered fragment it evaluates
/// `(reference & readMask) <compare> (stored & readMask)`; the outcome selects the op —
/// `fail_op` on a stencil fail, `depth_fail_op` on a stencil-pass/depth-fail, `pass_op` on both passing —
/// whose result ([`stencil_op::apply`]) is written back through `stencilWriteMask`
/// (`stored = (stored & !mask) | (new & mask)`). Only the FRONT-face state is modeled: the differential
/// programs set front == back, and the full-screen/centred geometry here is front-facing under the wire
/// default winding, so front-face suffices. A `Depth32Float` attachment carries no stencil aspect and its
/// pipeline's front/back are the inert `DISABLED` (`ALWAYS`/`KEEP`) default, so the stencil test always
/// passes and never writes — the depth-only path stays byte-for-byte unchanged.
#[allow(clippy::too_many_arguments)]
fn raster_draw_depth(
    res: &mut SessionResources,
    targets: &[(u32, TextureFormat)],
    blends: &[Option<BlendState>],
    write_masks: &[u32],
    tris: &[[usize; 3]],
    verts: &[DrawVertex],
    depth_tex: u32,
    state: DepthState,
    stencil_ref: u32,
    cull: u32,
    front_face: u32,
) -> Result<()> {
    // Dimensions + format come from the depth attachment (color + depth attachments share extent).
    let (ds_fmt, w, h) = {
        let t = texture(res, depth_tex)?;
        (t.desc.format, t.desc.width as usize, t.desc.height as usize)
    };
    if w == 0 || h == 0 {
        return Ok(());
    }
    // A combined depth+stencil attachment stores 8 bytes/texel (`[depth f32 | stencil u8 | 0,0,0]`); a
    // depth-only attachment stores 4 (`[depth f32]`). The stencil plane exists only for the former.
    let has_stencil = ds_fmt == TextureFormat::Depth24PlusStencil8;
    let stride = if has_stencil { 8 } else { 4 };

    // Load the depth (f32) and — when present — stencil (u8) planes from the attachment.
    let (mut depth_buf, mut stencil_buf): (Vec<f32>, Vec<u8>) = {
        let px = &texture(res, depth_tex)?.pixels;
        let mut d = Vec::with_capacity(w * h);
        let mut s = Vec::with_capacity(w * h);
        for i in 0..w * h {
            let o = i * stride;
            let z = if o + 4 <= px.len() {
                f32::from_le_bytes([px[o], px[o + 1], px[o + 2], px[o + 3]])
            } else {
                1.0
            };
            let sv = if has_stencil && o + 5 <= px.len() {
                px[o + 4]
            } else {
                0
            };
            d.push(z);
            s.push(sv);
        }
        (d, s)
    };

    // Static front-face stencil state + masks (truncated to the 8-bit buffer) + the dynamic reference.
    let sf: StencilFaceState = state.stencil_front;
    let read_mask = (state.stencil_read_mask & 0xff) as u8;
    let write_mask = (state.stencil_write_mask & 0xff) as u8;
    let sref = (stencil_ref & 0xff) as u8;

    // Resolve, per pixel, the winning interpolated source color (the depth+stencil tests decide the winner).
    let mut win: Vec<Option<[f32; 4]>> = vec![None; w * h];
    for tri in tris {
        let v = [verts[tri[0]], verts[tri[1]], verts[tri[2]]];
        let fb = [
            ndc_to_fb(v[0].pos, w, h),
            ndc_to_fb(v[1].pos, w, h),
            ndc_to_fb(v[2].pos, w, h),
        ];
        let area = edge(fb[0], fb[1], fb[2]);
        // A culled triangle contributes NOTHING — no color, no depth write, and no stencil op — matching
        // the executor, where face culling precedes the per-fragment depth/stencil tests.
        if area == 0.0 || culled(area, cull, front_face) {
            continue;
        }
        let (minx, miny, maxx, maxy) = tri_bbox(&fb, w, h);
        for py in miny..maxy {
            for px in minx..maxx {
                let idx = py * w + px;
                let c = [px as f32 + 0.5, py as f32 + 0.5];
                let bary = match barycentric(&fb, c, area) {
                    Some(b) => b,
                    None => continue,
                };
                let z = bary[0] * v[0].z + bary[1] * v[1].z + bary[2] * v[2].z;

                // Stencil test: `(ref & readMask) <compare> (stored & readMask)` (integer values represent
                // exactly in f32, so the shared `compare::passes` gives the exact stencil comparison).
                let stored_s = stencil_buf[idx];
                let s_pass = compare::passes(
                    sf.compare,
                    (sref & read_mask) as f32,
                    (stored_s & read_mask) as f32,
                );
                let d_pass = compare::passes(state.depth_compare, z, depth_buf[idx]);

                // Apply the stencil op selected by the test outcome, writing only `stencilWriteMask` bits.
                if has_stencil {
                    let op = if !s_pass {
                        sf.fail_op
                    } else if !d_pass {
                        sf.depth_fail_op
                    } else {
                        sf.pass_op
                    };
                    let candidate = stencil_op::apply(op, stored_s, sref);
                    stencil_buf[idx] = (stored_s & !write_mask) | (candidate & write_mask);
                }

                // Color + depth update only when BOTH the stencil and depth tests pass.
                if s_pass && d_pass {
                    win[idx] = Some(DrawVertex::interpolate_color(&v, bary));
                    if state.depth_write {
                        depth_buf[idx] = z;
                    }
                }
            }
        }
    }

    // Write the updated depth (and stencil) plane back.
    {
        let t = texture_mut(res, depth_tex)?;
        for i in 0..w * h {
            let o = i * stride;
            if o + 4 <= t.pixels.len() {
                t.pixels[o..o + 4].copy_from_slice(&depth_buf[i].to_le_bytes());
            }
            if has_stencil && o + 5 <= t.pixels.len() {
                t.pixels[o + 4] = stencil_buf[i];
            }
        }
    }

    // Composite the winning fragments into every color attachment.
    for (ti, (tex_id, fmt)) in targets.iter().enumerate() {
        let order = fmt.rgba_channel_order().ok_or(GpuError::Unsupported(
            "software: draw into a non-4-channel color format",
        ))?;
        let srgb = fmt.is_srgb();
        let blend_enabled = blends.get(ti).map(|b| b.is_some()).unwrap_or(false);
        let write_mask = write_masks.get(ti).copied().unwrap_or(0xF);
        let bpt = texture(res, *tex_id)?.desc.format.software_texel_bytes()?;
        let t = texture_mut(res, *tex_id)?;
        for (idx, winner) in win.iter().enumerate() {
            if let Some(src) = *winner {
                let texel = &mut t.pixels[idx * bpt..idx * bpt + bpt];
                write_fragment(texel, order, srgb, *fmt, blend_enabled, write_mask, src)?;
            }
        }
    }
    Ok(())
}
