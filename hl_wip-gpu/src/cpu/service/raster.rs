//! Render-pass execution: attachment clears, `ClearRect`, and the software draw path (triangle
//! rasterization with premultiplied linear-light source-over blending). Ported from the render arms of
//! `SoftwareBackend::submit`, `clear_rect`, `raster_draw`, `exec_draw`, `exec_draw_indexed`,
//! `raster_state`, and the draw-vertex helpers in `hl-gpu/src/software.rs`.

use crate::cpu::format::{
    clear_texel, is_srgb, load_texel_linear, rgba_channel_order, srgb_to_linear, store_texel_linear,
    texel_bytes,
};
use crate::cpu::model::pipeline::Pipeline;
use crate::cpu::model::{pipeline, texture, texture_mut};
use crate::protocol::model::descriptor::{BlendState, DepthState, StencilFaceState};
use crate::protocol::model::enums::{compare, stencil_op, IndexFormat, TextureFormat, Topology};
use crate::protocol::model::error::{GpuError, Result};
use crate::runtime::model::resources::SessionResources;

/// A `BeginRenderPass` `LoadOp::Clear` on one color attachment: fill every level-0 texel with the packed
/// clear color.
pub(crate) fn clear_target(res: &mut SessionResources, texture_id: u32, color: [f32; 4]) -> Result<()> {
    let (fmt, w, h) = {
        let t = texture(res, texture_id)?;
        (t.desc.format, t.desc.width, t.desc.height)
    };
    let texel = clear_texel(fmt, color)?;
    let t = texture_mut(res, texture_id)?;
    let n = (w * h) as usize;
    t.pixels.clear();
    t.pixels.reserve(n * texel.len());
    for _ in 0..n {
        t.pixels.extend_from_slice(&texel);
    }
    Ok(())
}

/// `ClearRect`: fill only the covered sub-rectangle of a texture with the packed clear color.
pub(crate) fn clear_rect(
    res: &mut SessionResources,
    texture_id: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: [f32; 4],
) -> Result<()> {
    let (fmt, tw, th) = {
        let t = texture(res, texture_id)?;
        (t.desc.format, t.desc.width, t.desc.height)
    };
    let texel = clear_texel(fmt, color)?;
    let bpt = texel.len();
    let x0 = x.min(tw) as usize;
    let y0 = y.min(th) as usize;
    let x1 = x.saturating_add(w).min(tw) as usize;
    let y1 = y.saturating_add(h).min(th) as usize;
    let tw = tw as usize;
    let t = texture_mut(res, texture_id)?;
    for yy in y0..y1 {
        for xx in x0..x1 {
            let off = (yy * tw + xx) * bpt;
            t.pixels[off..off + bpt].copy_from_slice(&texel);
        }
    }
    Ok(())
}

/// A `BeginRenderPass` `LoadOp::Clear` on a depth (or combined depth+stencil) attachment. A `Depth32Float`
/// plane is filled with the packed clear depth (little-endian f32, one per texel); a `Depth24PlusStencil8`
/// plane is filled per texel with `[depth: f32 le | stencil: clear_stencil as u8 @ byte 4 | 0,0,0]` — the
/// oracle-internal 8-byte layout the depth/stencil rasterizer reads (`clear_stencil` is truncated to the
/// 8-bit stencil buffer). Formats without a depth aspect are left unchanged.
pub(crate) fn clear_depth_stencil_target(
    res: &mut SessionResources,
    texture_id: u32,
    clear_depth: f32,
    clear_stencil: u32,
) -> Result<()> {
    let (fmt, w, h) = {
        let t = texture(res, texture_id)?;
        (t.desc.format, t.desc.width as usize, t.desc.height as usize)
    };
    let n = w * h;
    let t = texture_mut(res, texture_id)?;
    match fmt {
        TextureFormat::Depth32Float => {
            let bytes = clear_depth.to_le_bytes();
            t.pixels.clear();
            t.pixels.reserve(n * 4);
            for _ in 0..n {
                t.pixels.extend_from_slice(&bytes);
            }
        }
        TextureFormat::Depth24PlusStencil8 => {
            let d = clear_depth.to_le_bytes();
            let s = clear_stencil as u8;
            let texel = [d[0], d[1], d[2], d[3], s, 0, 0, 0];
            t.pixels.clear();
            t.pixels.reserve(n * 8);
            for _ in 0..n {
                t.pixels.extend_from_slice(&texel);
            }
        }
        _ => {}
    }
    Ok(())
}

/// The fixed-function raster state a draw is rasterized against, resolved from the bound render pipeline.
struct RasterState {
    /// Primitive assembly for the vertex stream.
    topology: Topology,
    /// Per-color-target blend (aligned with the render pass's color targets).
    blends: Vec<Option<BlendState>>,
    /// Per-color-target RGBA write mask (aligned with the color targets); `0xF` = write all channels.
    write_masks: Vec<u32>,
    /// Slot-0 vertex stride (bytes).
    stride: usize,
    /// Depth/stencil state, if the pipeline declares a depth attachment.
    depth: Option<DepthState>,
    /// Face culling: 0 = none, 1 = front, 2 = back.
    cull: u32,
    /// Front-face winding: 0 = CCW, 1 = CW.
    front_face: u32,
}

/// Fetch the pipeline's raster state (topology, per-target blend + write mask, slot-0 vertex stride, depth
/// state, cull + front_face) if it is a render pipeline whose first vertex layout can carry positions.
/// `None` => nothing to rasterize.
fn raster_state(res: &SessionResources, pipeline_id: Option<u32>) -> Result<Option<RasterState>> {
    let pid = match pipeline_id {
        Some(p) => p,
        None => return Ok(None),
    };
    match pipeline(res, pid)? {
        Pipeline::Render {
            vertex_layouts, topology, blends, write_masks, cull, front_face, depth, ..
        } => {
            let stride = match vertex_layouts.first() {
                Some(l) if l.stride as usize >= 8 => l.stride as usize,
                _ => return Ok(None),
            };
            Ok(Some(RasterState {
                topology: *topology,
                blends: blends.clone(),
                write_masks: write_masks.clone(),
                stride,
                depth: depth.clone(),
                cull: *cull,
                front_face: *front_face,
            }))
        }
        Pipeline::Compute { .. } => Ok(None),
    }
}

/// Resolve the active depth attachment for a draw: pair the render pass's depth-attachment texture with
/// the pipeline's depth state. Depth testing runs only when BOTH are present.
fn active_depth(depth_tex: Option<u32>, depth_state: Option<DepthState>) -> Option<(u32, DepthState)> {
    match (depth_tex, depth_state) {
        (Some(t), Some(s)) => Some((t, s)),
        _ => None,
    }
}

/// Execute a non-indexed `Draw`: fetch `[first_vertex, first_vertex+vertex_count)` from slot-0's vertex
/// buffer and rasterize into the bound color attachments.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_draw(
    res: &mut SessionResources,
    pipeline_id: Option<u32>,
    targets: &[(u32, TextureFormat)],
    depth_tex: Option<u32>,
    vertex_buffer: Option<(u32, u64)>,
    first_vertex: u32,
    vertex_count: u32,
    instance_count: u32,
    stencil_ref: u32,
) -> Result<()> {
    let RasterState { topology, blends, write_masks, stride, depth: depth_state, cull, front_face } =
        match raster_state(res, pipeline_id)? {
            Some(s) => s,
            None => return Ok(()),
        };
    let (vbuf, voff) = match vertex_buffer {
        Some(x) => x,
        None => return Ok(()),
    };
    let verts = {
        let b = crate::cpu::model::buffer(res, vbuf)?;
        let mut out = Vec::with_capacity(vertex_count as usize);
        for i in first_vertex..first_vertex.saturating_add(vertex_count) {
            let base = voff as usize + i as usize * stride;
            if base + 8 > b.data.len() {
                return Err(GpuError::OutOfBounds);
            }
            out.push(read_vertex(&b.data, base, stride));
        }
        out
    };
    let depth = active_depth(depth_tex, depth_state);
    for _ in 0..instance_count.max(1) {
        raster_draw(
            res, targets, &blends, &write_masks, topology, &verts, depth.clone(), stencil_ref, cull,
            front_face,
        )?;
    }
    Ok(())
}

/// Execute a `DrawIndexed`: read `index_count` indices from the bound index buffer, add `base_vertex`,
/// gather the referenced slot-0 vertices, and rasterize.
#[allow(clippy::too_many_arguments)]
pub(crate) fn exec_draw_indexed(
    res: &mut SessionResources,
    pipeline_id: Option<u32>,
    targets: &[(u32, TextureFormat)],
    depth_tex: Option<u32>,
    vertex_buffer: Option<(u32, u64)>,
    index_buffer: Option<(u32, u64, IndexFormat)>,
    first_index: u32,
    index_count: u32,
    base_vertex: i32,
    instance_count: u32,
    stencil_ref: u32,
) -> Result<()> {
    let RasterState { topology, blends, write_masks, stride, depth: depth_state, cull, front_face } =
        match raster_state(res, pipeline_id)? {
            Some(s) => s,
            None => return Ok(()),
        };
    let (vbuf, voff) = match vertex_buffer {
        Some(x) => x,
        None => return Ok(()),
    };
    let (ibuf, ioff, ifmt) = match index_buffer {
        Some(x) => x,
        None => return Ok(()),
    };
    let indices: Vec<u32> = {
        let b = crate::cpu::model::buffer(res, ibuf)?;
        let isz = match ifmt {
            IndexFormat::U16 => 2usize,
            IndexFormat::U32 => 4usize,
        };
        let mut out = Vec::with_capacity(index_count as usize);
        for i in first_index..first_index.saturating_add(index_count) {
            let base = ioff as usize + i as usize * isz;
            if base + isz > b.data.len() {
                return Err(GpuError::OutOfBounds);
            }
            let raw = match ifmt {
                IndexFormat::U16 => u16::from_le_bytes([b.data[base], b.data[base + 1]]) as u32,
                IndexFormat::U32 => u32::from_le_bytes([
                    b.data[base],
                    b.data[base + 1],
                    b.data[base + 2],
                    b.data[base + 3],
                ]),
            };
            out.push(raw);
        }
        out
    };
    let verts = {
        let b = crate::cpu::model::buffer(res, vbuf)?;
        let mut out = Vec::with_capacity(indices.len());
        for raw in indices {
            let vidx = (raw as i64) + base_vertex as i64;
            if vidx < 0 {
                return Err(GpuError::OutOfBounds);
            }
            let base = voff as usize + vidx as usize * stride;
            if base + 8 > b.data.len() {
                return Err(GpuError::OutOfBounds);
            }
            out.push(read_vertex(&b.data, base, stride));
        }
        out
    };
    let depth = active_depth(depth_tex, depth_state);
    for _ in 0..instance_count.max(1) {
        raster_draw(
            res, targets, &blends, &write_masks, topology, &verts, depth.clone(), stencil_ref, cull,
            front_face,
        )?;
    }
    Ok(())
}

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
        Topology::TriangleList => {
            (0..verts.len() / 3).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect()
        }
        Topology::TriangleStrip => (0..verts.len().saturating_sub(2))
            .map(|i| if i % 2 == 0 { [i, i + 1, i + 2] } else { [i + 1, i, i + 2] })
            .collect(),
        _ => return Ok(()),
    };
    if tris.is_empty() {
        return Ok(());
    }

    match depth {
        Some((depth_tex, state)) => raster_draw_depth(
            res, targets, blends, write_masks, &tris, verts, depth_tex, state, stencil_ref, cull,
            front_face,
        ),
        None => raster_draw_no_depth(res, targets, blends, write_masks, &tris, verts, cull, front_face),
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
/// (`hl_wip-gpu-wgpu/tests/differential.rs` `gen_cull_*`): those programs draw fullscreen triangles of KNOWN
/// winding under both `front_face` values and both cull faces and assert the oracle culls EXACTLY the same
/// triangles the executor does (a flipped sign inverts every cull decision — 40 divergences until matched).
fn culled(area: f32, cull: u32, front_face: u32) -> bool {
    let front_facing = if front_face == 1 { area > 0.0 } else { area < 0.0 };
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
        let order = rgba_channel_order(*fmt)
            .ok_or(GpuError::Unsupported("software: draw into a non-4-channel color format"))?;
        let srgb = is_srgb(*fmt);
        let blend_enabled = blends.get(ti).map(|b| b.is_some()).unwrap_or(false);
        let write_mask = write_masks.get(ti).copied().unwrap_or(0xF);
        let (w, h, bpt) = {
            let t = texture(res, *tex_id)?;
            (t.desc.width as usize, t.desc.height as usize, texel_bytes(t.desc.format)?)
        };
        if w == 0 || h == 0 {
            continue;
        }
        let mut covered = vec![false; w * h];
        let t = texture_mut(res, *tex_id)?;
        for tri in tris {
            let v = [verts[tri[0]], verts[tri[1]], verts[tri[2]]];
            let fb =
                [ndc_to_fb(v[0].pos, w, h), ndc_to_fb(v[1].pos, w, h), ndc_to_fb(v[2].pos, w, h)];
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
                    let src = interp_color(&v, bary);
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
            let sv = if has_stencil && o + 5 <= px.len() { px[o + 4] } else { 0 };
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
        let fb = [ndc_to_fb(v[0].pos, w, h), ndc_to_fb(v[1].pos, w, h), ndc_to_fb(v[2].pos, w, h)];
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
                    win[idx] = Some(interp_color(&v, bary));
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
        let order = rgba_channel_order(*fmt)
            .ok_or(GpuError::Unsupported("software: draw into a non-4-channel color format"))?;
        let srgb = is_srgb(*fmt);
        let blend_enabled = blends.get(ti).map(|b| b.is_some()).unwrap_or(false);
        let write_mask = write_masks.get(ti).copied().unwrap_or(0xF);
        let bpt = texel_bytes(texture(res, *tex_id)?.desc.format)?;
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

/// Barycentric weights of pixel-center `c` inside framebuffer-space triangle `fb` (signed area `area`),
/// or `None` if the pixel is outside. Matches the two-sided inside test the oracle uses.
fn barycentric(fb: &[[f32; 2]; 3], c: [f32; 2], area: f32) -> Option<[f32; 3]> {
    let e0 = edge(fb[1], fb[2], c);
    let e1 = edge(fb[2], fb[0], c);
    let e2 = edge(fb[0], fb[1], c);
    let inside =
        (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0) || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0);
    if !inside {
        return None;
    }
    Some([e0 / area, e1 / area, e2 / area])
}

/// Barycentric-interpolate the straight-alpha vertex color at a fragment.
fn interp_color(v: &[DrawVertex; 3], bary: [f32; 3]) -> [f32; 4] {
    let mut src = [0f32; 4];
    for k in 0..4 {
        src[k] = bary[0] * v[0].color[k] + bary[1] * v[1].color[k] + bary[2] * v[2].color[k];
    }
    src
}

/// Composite one source fragment into a target texel: premultiplied linear-light source-over when the
/// target's blend is enabled, else an opaque replace. Extracted so the depth-tested and depth-less paths
/// share byte-identical color math.
///
/// The RGBA `write_mask` (low 4 bits `R<<0|G<<1|B<<2|A<<3`, matching the executor's `ColorWrites` mapping)
/// gates which logical channels the write reaches: a channel whose bit is CLEAR keeps its prior value. The
/// neutral `0xF` writes every channel and takes the fast path — byte-for-byte the pre-mask behavior; the
/// composite (blend or replace) is computed in full and then the masked-out channels are restored, so
/// masking composes correctly with both the blend and the sRGB encode.
fn write_fragment(
    texel: &mut [u8],
    order: [usize; 4],
    srgb: bool,
    fmt: TextureFormat,
    blend_enabled: bool,
    write_mask: u32,
    src: [f32; 4],
) -> Result<()> {
    // Snapshot the prior per-channel bytes so masked-out channels can be restored after the write. Only
    // taken when a channel is actually masked out (`write_mask != 0xF`), keeping the neutral path allocation-
    // and copy-free.
    let masked = write_mask & 0xF != 0xF;
    let saved: [u8; 4] = if masked {
        [texel[order[0]], texel[order[1]], texel[order[2]], texel[order[3]]]
    } else {
        [0; 4]
    };
    if blend_enabled {
        let a = src[3].clamp(0.0, 1.0);
        let s_lin = |k: usize| {
            if srgb {
                srgb_to_linear(src[k].clamp(0.0, 1.0))
            } else {
                src[k].clamp(0.0, 1.0)
            }
        };
        let dst = load_texel_linear(texel, order, srgb);
        let out = [
            s_lin(0) * a + dst[0] * (1.0 - a),
            s_lin(1) * a + dst[1] * (1.0 - a),
            s_lin(2) * a + dst[2] * (1.0 - a),
            a + dst[3] * (1.0 - a),
        ];
        store_texel_linear(texel, order, srgb, out);
    } else {
        let bytes = clear_texel(fmt, src)?;
        texel.copy_from_slice(&bytes);
    }
    // Restore the channels the mask leaves untouched (logical R,G,B,A at byte indices `order[0..4]`).
    if masked {
        for k in 0..4 {
            if (write_mask >> k) & 1 == 0 {
                texel[order[k]] = saved[k];
            }
        }
    }
    Ok(())
}

/// One vertex the software oracle's draw path consumes: NDC position `(x, y)` at byte 0, a per-fragment
/// depth `z`, and a straight-alpha color. The stride selects the layout (all offsets little-endian f32):
///
/// * `>= 28`: `x,y,z` at 0/4/8, `color` (rgba) at 12/16/20/24  — 3D position + color (depth-tested draws)
/// * `>= 24`: `x,y`   at 0/4,   `color` (rgba) at 8/12/16/20   — the historical 2D-pos+color layout (`z=0`)
/// * `>= 12`: `x,y,z` at 0/4/8, color defaults to opaque white — 3D position only
/// * else   : `x,y`   at 0/4,   color defaults to opaque white — 2D position only (`z=0`)
///
/// The `>= 24` arm is byte-for-byte the pre-depth behavior, so existing color draws are unchanged; the z
/// component is an additive extension gated on the larger strides.
#[derive(Clone, Copy)]
struct DrawVertex {
    pos: [f32; 2],
    z: f32,
    color: [f32; 4],
}

fn read_vertex(data: &[u8], base: usize, stride: usize) -> DrawVertex {
    // Bounds-tolerant read: an offset past the slice yields 0.0 (submit-time validation already ensures a
    // valid vertex fits, so this only guards against a defensive over-read, never a real payload).
    let f = |o: usize| {
        let i = base + o;
        if i + 4 <= data.len() {
            f32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]])
        } else {
            0.0
        }
    };
    let pos = [f(0), f(4)];
    let (z, color) = if stride >= 28 {
        (f(8), [f(12), f(16), f(20), f(24)])
    } else if stride >= 24 {
        (0.0, [f(8), f(12), f(16), f(20)])
    } else if stride >= 12 {
        (f(8), [1.0, 1.0, 1.0, 1.0])
    } else {
        (0.0, [1.0, 1.0, 1.0, 1.0])
    };
    DrawVertex { pos, z, color }
}

/// Map an NDC position (x right, y up, in [-1,1]) to framebuffer pixel space (origin top-left, y down).
fn ndc_to_fb(p: [f32; 2], w: usize, h: usize) -> [f32; 2] {
    [(p[0] * 0.5 + 0.5) * w as f32, (0.5 - p[1] * 0.5) * h as f32]
}

/// Signed area (×2) of triangle `a,b,c` — the edge function used for barycentric coverage.
fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Integer pixel bounding box `[minx,maxx) × [miny,maxy)` of a framebuffer-space triangle, clamped to the
/// target dimensions.
fn tri_bbox(fb: &[[f32; 2]; 3], w: usize, h: usize) -> (usize, usize, usize, usize) {
    let minxf = fb[0][0].min(fb[1][0]).min(fb[2][0]);
    let maxxf = fb[0][0].max(fb[1][0]).max(fb[2][0]);
    let minyf = fb[0][1].min(fb[1][1]).min(fb[2][1]);
    let maxyf = fb[0][1].max(fb[1][1]).max(fb[2][1]);
    let minx = (minxf.floor().max(0.0) as i64).clamp(0, w as i64) as usize;
    let miny = (minyf.floor().max(0.0) as i64).clamp(0, h as i64) as usize;
    let maxx = (maxxf.ceil() as i64).clamp(0, w as i64) as usize;
    let maxy = (maxyf.ceil() as i64).clamp(0, h as i64) as usize;
    (minx, miny, maxx, maxy)
}
