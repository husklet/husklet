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

impl RasterState {
    /// Resolve the bound pipeline's fixed-function raster state. `None` means there is nothing to draw.
    fn resolve(res: &SessionResources, pipeline_id: Option<u32>) -> Result<Option<Self>> {
        let pid = match pipeline_id {
            Some(p) => p,
            None => return Ok(None),
        };
        match pipeline(res, pid)? {
            Pipeline::Render {
                vertex_layouts,
                topology,
                blends,
                write_masks,
                cull,
                front_face,
                depth,
                ..
            } => {
                let stride = match vertex_layouts.first() {
                    Some(l) if l.stride as usize >= 8 => l.stride as usize,
                    _ => return Ok(None),
                };
                Ok(Some(Self {
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
}

/// Byte address of vertex `index` in a slot-0 vertex buffer bound at `offset` with `stride`. Every operand
/// is guest-controlled — the index of an indexed draw comes out of buffer CONTENT, which no validation can
/// bound — so the address is computed in checked arithmetic and an address no `usize` can hold is out of
/// bounds rather than a wrapped (and therefore bounds-check-passing) offset.
fn vertex_address(offset: u64, index: u64, stride: usize) -> Result<usize> {
    index
        .checked_mul(stride as u64)
        .and_then(|span| span.checked_add(offset))
        .and_then(|address| usize::try_from(address).ok())
        .ok_or(GpuError::OutOfBounds)
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
    rect: PassRect,
) -> Result<()> {
    let RasterState {
        topology,
        blends,
        write_masks,
        stride,
        depth: depth_state,
        cull,
        front_face,
    } = match RasterState::resolve(res, pipeline_id)? {
        Some(s) => s,
        None => return Ok(()),
    };
    let (vbuf, voff) = match vertex_buffer {
        Some(x) => x,
        None => return Ok(()),
    };
    let verts = {
        let b = crate::cpu::model::buffer(res, vbuf)?;
        // Reserve only for vertices the buffer could actually supply: `vertex_count` is guest-chosen and a
        // maximal one would otherwise reserve tens of gigabytes before the per-vertex bounds check below
        // rejects the first fetch.
        let mut out = Vec::with_capacity((vertex_count as usize).min(b.data.len() / stride + 1));
        for i in first_vertex..first_vertex.saturating_add(vertex_count) {
            let base = vertex_address(voff, i as u64, stride)?;
            if base.saturating_add(8) > b.data.len() {
                return Err(GpuError::OutOfBounds);
            }
            out.push(read_vertex(&b.data, base, stride));
        }
        out
    };
    let depth = depth_tex.zip(depth_state);
    for _ in 0..instance_count.max(1) {
        raster_draw(
            res,
            targets,
            &blends,
            &write_masks,
            topology,
            &verts,
            depth.clone(),
            stencil_ref,
            cull,
            front_face,
            rect,
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
    rect: PassRect,
) -> Result<()> {
    let RasterState {
        topology,
        blends,
        write_masks,
        stride,
        depth: depth_state,
        cull,
        front_face,
    } = match RasterState::resolve(res, pipeline_id)? {
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
            let base = vertex_address(voff, vidx as u64, stride)?;
            if base.saturating_add(8) > b.data.len() {
                return Err(GpuError::OutOfBounds);
            }
            out.push(read_vertex(&b.data, base, stride));
        }
        out
    };
    let depth = depth_tex.zip(depth_state);
    for _ in 0..instance_count.max(1) {
        raster_draw(
            res,
            targets,
            &blends,
            &write_masks,
            topology,
            &verts,
            depth.clone(),
            stencil_ref,
            cull,
            front_face,
            rect,
        )?;
    }
    Ok(())
}
use super::*;
