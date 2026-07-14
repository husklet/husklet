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
use crate::protocol::model::descriptor::BlendState;
use crate::protocol::model::enums::{IndexFormat, TextureFormat, Topology};
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

/// Fetch the pipeline's raster state (topology, per-target blend, slot-0 vertex stride) if it is a render
/// pipeline whose first vertex layout can carry positions. `None` => nothing to rasterize.
fn raster_state(
    res: &SessionResources,
    pipeline_id: Option<u32>,
) -> Result<Option<(Topology, Vec<Option<BlendState>>, usize)>> {
    let pid = match pipeline_id {
        Some(p) => p,
        None => return Ok(None),
    };
    match pipeline(res, pid)? {
        Pipeline::Render { vertex_layouts, topology, blends, .. } => {
            let stride = match vertex_layouts.first() {
                Some(l) if l.stride as usize >= 8 => l.stride as usize,
                _ => return Ok(None),
            };
            Ok(Some((*topology, blends.clone(), stride)))
        }
        Pipeline::Compute { .. } => Ok(None),
    }
}

/// Execute a non-indexed `Draw`: fetch `[first_vertex, first_vertex+vertex_count)` from slot-0's vertex
/// buffer and rasterize into the bound color attachments.
pub(crate) fn exec_draw(
    res: &mut SessionResources,
    pipeline_id: Option<u32>,
    targets: &[(u32, TextureFormat)],
    vertex_buffer: Option<(u32, u64)>,
    first_vertex: u32,
    vertex_count: u32,
    instance_count: u32,
) -> Result<()> {
    let (topology, blends, stride) = match raster_state(res, pipeline_id)? {
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
    for _ in 0..instance_count.max(1) {
        raster_draw(res, targets, &blends, topology, &verts)?;
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
    vertex_buffer: Option<(u32, u64)>,
    index_buffer: Option<(u32, u64, IndexFormat)>,
    first_index: u32,
    index_count: u32,
    base_vertex: i32,
    instance_count: u32,
) -> Result<()> {
    let (topology, blends, stride) = match raster_state(res, pipeline_id)? {
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
    for _ in 0..instance_count.max(1) {
        raster_draw(res, targets, &blends, topology, &verts)?;
    }
    Ok(())
}

/// Rasterize one draw's assembled triangles into every bound color attachment, compositing with
/// premultiplied source-over performed in LINEAR light (sRGB targets decode/encode around the blend; a
/// target whose blend is `None` gets an opaque replace).
fn raster_draw(
    res: &mut SessionResources,
    targets: &[(u32, TextureFormat)],
    blends: &[Option<BlendState>],
    topology: Topology,
    verts: &[DrawVertex],
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

    for (ti, (tex_id, fmt)) in targets.iter().enumerate() {
        let order = rgba_channel_order(*fmt)
            .ok_or(GpuError::Unsupported("software: draw into a non-4-channel color format"))?;
        let srgb = is_srgb(*fmt);
        let blend_enabled = blends.get(ti).map(|b| b.is_some()).unwrap_or(false);
        let (w, h, bpt) = {
            let t = texture(res, *tex_id)?;
            (t.desc.width as usize, t.desc.height as usize, texel_bytes(t.desc.format)?)
        };
        if w == 0 || h == 0 {
            continue;
        }
        let mut covered = vec![false; w * h];
        let t = texture_mut(res, *tex_id)?;
        for tri in &tris {
            let v = [verts[tri[0]], verts[tri[1]], verts[tri[2]]];
            let fb =
                [ndc_to_fb(v[0].pos, w, h), ndc_to_fb(v[1].pos, w, h), ndc_to_fb(v[2].pos, w, h)];
            let area = edge(fb[0], fb[1], fb[2]);
            if area == 0.0 {
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
                    let e0 = edge(fb[1], fb[2], c);
                    let e1 = edge(fb[2], fb[0], c);
                    let e2 = edge(fb[0], fb[1], c);
                    let inside = (e0 >= 0.0 && e1 >= 0.0 && e2 >= 0.0)
                        || (e0 <= 0.0 && e1 <= 0.0 && e2 <= 0.0);
                    if !inside {
                        continue;
                    }
                    let (l0, l1, l2) = (e0 / area, e1 / area, e2 / area);
                    let mut src = [0f32; 4];
                    for k in 0..4 {
                        src[k] = l0 * v[0].color[k] + l1 * v[1].color[k] + l2 * v[2].color[k];
                    }
                    let texel = &mut t.pixels[idx * bpt..idx * bpt + bpt];
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
                        let bytes = clear_texel(*fmt, src)?;
                        texel.copy_from_slice(&bytes);
                    }
                    covered[idx] = true;
                }
            }
        }
    }
    Ok(())
}

/// One vertex the software oracle's draw path consumes: position (NDC) at byte 0, straight-alpha color at
/// byte 8. A vertex stride < 24 carries position only and color defaults to opaque white.
#[derive(Clone, Copy)]
struct DrawVertex {
    pos: [f32; 2],
    color: [f32; 4],
}

fn read_vertex(data: &[u8], base: usize, stride: usize) -> DrawVertex {
    let f = |o: usize| {
        f32::from_le_bytes([data[base + o], data[base + o + 1], data[base + o + 2], data[base + o + 3]])
    };
    let pos = [f(0), f(4)];
    let color = if stride >= 24 { [f(8), f(12), f(16), f(20)] } else { [1.0, 1.0, 1.0, 1.0] };
    DrawVertex { pos, color }
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
