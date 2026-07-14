//! A real (if minimal) CPU executor — the standing correctness fallback the architecture mandates
//! (llvmpipe/lavapipe fills this role on a real host; here it's a hand-rolled analog).
//!
//! It materializes buffers and textures in plain host memory and actually *executes* the parts an ML /
//! headless smoke test needs end-to-end with no GPU: buffer write/readback (CUDA H2D/D2H), render-pass
//! **clear** into a color target, and buffer↔texture / buffer↔buffer copies. Draw/Dispatch are recorded
//! but not rasterized/run (that needs a SPIR-V interpreter — out of scope). Enough to prove the whole
//! IR→wire→replay→execute→readback chain works headless on this Linux host.
//!
//! Because it doubles as the headless *oracle* that other backends are checked against, it validates
//! the command stream the way a real driver would: it rejects malformed IR (bad ids, out-of-bounds or
//! wrapping ranges, missing usage bits, contradictory descriptors, stale/reused resources, illegal
//! pass sequencing) with a typed [`GpuError`] instead of panicking or silently doing the wrong thing.
//! `submit` validates the *entire* command buffer before mutating anything, so a stream that fails
//! validation leaves all resources untouched (no partial side effects).

use crate::backend::{Capabilities, GpuBackend, PresentKind, PresentToken};
use crate::id::*;
use crate::ir::*;
use crate::ptx::{self, KernelDescriptor, KernelProgram};
use crate::{GpuError, Result};
use std::collections::HashMap;

struct Buffer {
    data: Vec<u8>,
    usage: u32,
}

struct Texture {
    desc: TextureDesc,
    /// Tight-packed level-0 pixels (bytes_per_texel * w * h).
    pixels: Vec<u8>,
}

/// A registered shader module. The software oracle's shader ABI is a hl-GPU **kernel program**
/// (compiled from forwarded PTX); a Metal backend would instead carry SPIR-V for the same slot — the
/// per-backend seam described in `docs/ideas/CUDA_ON_METAL.md §5`.
enum ShaderModule {
    /// A compiled compute kernel this backend can actually execute on the CPU.
    Kernel(Box<KernelProgram>),
    /// Opaque SPIR-V — accepted but not run here (needs a Metal/Vulkan backend). The words are
    /// validated at create time and then discarded; this backend never reads them back.
    Spirv,
}

/// A created pipeline. Compute pipelines remember their kernel shader so a `Dispatch` can run it;
/// render pipelines remember the state a draw must be validated against (color-target formats and the
/// vertex buffer layouts).
enum Pipeline {
    Render {
        color_formats: Vec<TextureFormat>,
        vertex_layouts: Vec<VertexLayout>,
        /// Primitive assembly for a draw's vertex stream (triangle list/strip supported by the raster path).
        topology: Topology,
        /// Per-color-target blend: `Some(_)` selects premultiplied linear-light source-over; `None` is an
        /// opaque replace. Aligned with `color_formats`.
        blends: Vec<Option<BlendState>>,
    },
    Compute {
        shader: u32,
    },
}

/// A generation stamp captured for one resource a bind group references, so a later use can detect
/// that the id was destroyed and (possibly) reused for a different resource since binding.
#[derive(Clone, Copy)]
struct GenRef {
    id: u32,
    gen: u32,
}

/// A created bind group: the descriptor plus the generation of every resource it referenced at
/// creation time (used to reject stale references after destroy/reuse).
struct BindGroupState {
    desc: BindGroupDesc,
    buffers: Vec<GenRef>,
    textures: Vec<GenRef>,
    samplers: Vec<GenRef>,
}

pub struct SoftwareBackend {
    buffers: ResourceTable<Buffer>,
    textures: ResourceTable<Texture>,
    shaders: ResourceTable<ShaderModule>,
    pipelines: ResourceTable<Pipeline>,
    bind_groups: ResourceTable<BindGroupState>,
    surfaces: ResourceTable<SurfaceDesc>,
    fences: ResourceTable<u64>,
    samplers: ResourceTable<()>,
    /// Count of dispatches/draws seen — lets a test confirm compute work reached the executor.
    pub dispatches: u64,
    pub draws: u64,
    next_present_handle: u64,
}

impl SoftwareBackend {
    pub fn new() -> Self {
        Self {
            buffers: ResourceTable::new(BufferId::KIND),
            textures: ResourceTable::new(TextureId::KIND),
            shaders: ResourceTable::new(ShaderId::KIND),
            pipelines: ResourceTable::new(PipelineId::KIND),
            bind_groups: ResourceTable::new(BindGroupId::KIND),
            surfaces: ResourceTable::new(SurfaceId::KIND),
            fences: ResourceTable::new(FenceId::KIND),
            samplers: ResourceTable::new(SamplerId::KIND),
            dispatches: 0,
            draws: 0,
            next_present_handle: 1,
        }
    }

    /// Seed raw per-sample texels for resolve conformance tests and software-oracle callers. Data is
    /// texel-major, then sample-major, then channel-major.
    pub fn write_texture_samples(&mut self, id: TextureId, data: &[u8]) -> Result<()> {
        let texture = self.textures.get_mut(id.0)?;
        if texture.pixels.len() != data.len() {
            return Err(GpuError::OutOfBounds);
        }
        texture.pixels.copy_from_slice(data);
        Ok(())
    }

    fn texel_bytes(fmt: TextureFormat) -> Result<usize> {
        fmt.bytes_per_texel()
            .ok_or(GpuError::Unsupported("software: non-color texture format"))
    }

    /// Execute a compute `Dispatch`: resolve the bound compute pipeline → kernel program and the bound
    /// resources → the parameter blob + storage regions, run the kernel per-thread over the grid, and
    /// write the mutated regions back. A SPIR-V (non-kernel) module is recorded but not run here.
    fn run_dispatch(
        &mut self,
        pipeline: Option<u32>,
        bind_group: Option<u32>,
        grid: (u32, u32, u32),
    ) -> Result<()> {
        let (pid, bgid) = match (pipeline, bind_group) {
            (Some(p), Some(b)) => (p, b),
            // A dispatch with no pipeline/bind group bound is a malformed stream; nothing to run.
            _ => return Ok(()),
        };
        let shader_id = match self.pipelines.get(pid)? {
            Pipeline::Compute { shader } => *shader,
            Pipeline::Render { .. } => {
                return Err(GpuError::Unsupported("dispatch on a render pipeline"))
            }
        };
        // Clone the program out so the shader-table borrow is released before we touch buffers.
        let prog = match self.shaders.get(shader_id)? {
            ShaderModule::Kernel(p) => (**p).clone(),
            ShaderModule::Spirv => return Ok(()), // software oracle cannot run SPIR-V
        };
        let bg = self.bind_groups.get(bgid)?.desc.clone();
        self.run_kernel(&prog, &bg, grid)
    }

    fn run_kernel(&mut self, prog: &KernelProgram, bg: &BindGroupDesc, grid: (u32, u32, u32)) -> Result<()> {
        // Gather the parameter blob (binding 0) and each pointer region (binding r+1 → region r).
        let mut param_blob: Vec<u8> = Vec::new();
        let mut regions: Vec<Vec<u8>> = vec![Vec::new(); prog.num_regions as usize];
        let mut writeback: Vec<Option<(u32, u64)>> = vec![None; prog.num_regions as usize];
        for e in &bg.entries {
            if let BindResource::Buffer { id, offset, size } = e.resource {
                let buf = self.buffers.get(id)?;
                let (off, len) = buffer_slice_bounds(buf.data.len(), offset, size)?;
                let bytes = buf.data[off..off + len].to_vec();
                if e.binding == 0 {
                    param_blob = bytes;
                } else {
                    let r = (e.binding - 1) as usize;
                    if r < regions.len() {
                        regions[r] = bytes;
                        writeback[r] = Some((id, offset));
                    }
                }
            }
        }
        ptx::execute(prog, &param_blob, &mut regions, grid)?;
        for (r, wb) in writeback.iter().enumerate() {
            if let Some((id, offset)) = wb {
                let buf = self.buffers.get_mut(*id)?;
                let off = *offset as usize;
                let end = off
                    .checked_add(regions[r].len())
                    .filter(|e| *e <= buf.data.len())
                    .ok_or(GpuError::OutOfBounds)?;
                buf.data[off..end].copy_from_slice(&regions[r]);
            }
        }
        Ok(())
    }

    /// Convert a normalized clear color to packed bytes for the 8-bit color formats.
    fn clear_texel(fmt: TextureFormat, c: [f32; 4]) -> Result<Vec<u8>> {
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        Ok(match fmt {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Srgb => {
                vec![to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), to_u8(c[3])]
            }
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb => {
                vec![to_u8(c[2]), to_u8(c[1]), to_u8(c[0]), to_u8(c[3])]
            }
            TextureFormat::R8Unorm => vec![to_u8(c[0])],
            TextureFormat::Rg8Unorm => vec![to_u8(c[0]), to_u8(c[1])],
            _ => return Err(GpuError::Unsupported("software: clear for this format")),
        })
    }

    fn clear_rect(&mut self, texture: u32, x: u32, y: u32, w: u32, h: u32, color: [f32; 4]) -> Result<()> {
        let (fmt, tw, th) = {
            let t = self.textures.get(texture)?;
            (t.desc.format, t.desc.width, t.desc.height)
        };
        let texel = Self::clear_texel(fmt, color)?;
        let bpt = texel.len();
        let x0 = x.min(tw) as usize;
        let y0 = y.min(th) as usize;
        let x1 = x.saturating_add(w).min(tw) as usize;
        let y1 = y.saturating_add(h).min(th) as usize;
        let tw = tw as usize;
        let t = self.textures.get_mut(texture)?;
        for yy in y0..y1 {
            for xx in x0..x1 {
                let off = (yy * tw + xx) * bpt;
                t.pixels[off..off + bpt].copy_from_slice(&texel);
            }
        }
        Ok(())
    }

    /// Rasterize one draw's assembled triangles into every bound color attachment, compositing with
    /// premultiplied source-over performed in LINEAR light. For each covered pixel the source and
    /// destination colors are decoded through the target's transfer function (sRGB EOTF for an sRGB
    /// format, identity for Unorm), the source is premultiplied by its alpha and composited `over` the
    /// destination, and the result is re-encoded — so an sRGB target blends gamma-correctly rather than
    /// naively in sRGB space. A target whose blend is `None` gets an opaque replace (straight source).
    fn raster_draw(
        &mut self,
        targets: &[(u32, TextureFormat)],
        blends: &[Option<BlendState>],
        topology: Topology,
        verts: &[DrawVertex],
    ) -> Result<()> {
        // Assemble triangle index triples. Only triangle primitives rasterize; other topologies are
        // recorded by the draw counter but produce no pixels in the oracle.
        let tris: Vec<[usize; 3]> = match topology {
            Topology::TriangleList => (0..verts.len() / 3).map(|t| [3 * t, 3 * t + 1, 3 * t + 2]).collect(),
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
                let t = self.textures.get(*tex_id)?;
                (t.desc.width as usize, t.desc.height as usize, Self::texel_bytes(t.desc.format)?)
            };
            if w == 0 || h == 0 {
                continue;
            }
            // At most one composite per pixel per draw: a quad's two triangles share a diagonal, and a
            // pixel center that lands exactly on it must not be blended twice. First triangle wins.
            let mut covered = vec![false; w * h];
            let t = self.textures.get_mut(*tex_id)?;
            for tri in &tris {
                let v = [verts[tri[0]], verts[tri[1]], verts[tri[2]]];
                let fb = [ndc_to_fb(v[0].pos, w, h), ndc_to_fb(v[1].pos, w, h), ndc_to_fb(v[2].pos, w, h)];
                let area = edge(fb[0], fb[1], fb[2]);
                if area == 0.0 {
                    continue; // degenerate triangle covers nothing
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
                        // Barycentric interpolation of the straight source color (flat for a solid fill).
                        let (l0, l1, l2) = (e0 / area, e1 / area, e2 / area);
                        let mut src = [0f32; 4];
                        for k in 0..4 {
                            src[k] = l0 * v[0].color[k] + l1 * v[1].color[k] + l2 * v[2].color[k];
                        }
                        let texel = &mut t.pixels[idx * bpt..idx * bpt + bpt];
                        if blend_enabled {
                            let a = src[3].clamp(0.0, 1.0);
                            // Decode source color into linear light (sRGB values pass the EOTF).
                            let s_lin = |k: usize| if srgb { srgb_to_linear(src[k].clamp(0.0, 1.0)) } else { src[k].clamp(0.0, 1.0) };
                            let dst = load_texel_linear(texel, order, srgb);
                            let out = [
                                s_lin(0) * a + dst[0] * (1.0 - a),
                                s_lin(1) * a + dst[1] * (1.0 - a),
                                s_lin(2) * a + dst[2] * (1.0 - a),
                                a + dst[3] * (1.0 - a),
                            ];
                            store_texel_linear(texel, order, srgb, out);
                        } else {
                            // Opaque replace: the straight source is already in the target encoding.
                            let bytes = Self::clear_texel(*fmt, src)?;
                            texel.copy_from_slice(&bytes);
                        }
                        covered[idx] = true;
                    }
                }
            }
        }
        Ok(())
    }

    /// Fetch the pipeline's raster state (topology, per-target blend, slot-0 vertex stride) if it is a
    /// render pipeline whose first vertex layout can carry positions. `None` => nothing to rasterize.
    fn raster_state(&self, pipeline: Option<u32>) -> Result<Option<(Topology, Vec<Option<BlendState>>, usize)>> {
        let pid = match pipeline {
            Some(p) => p,
            None => return Ok(None),
        };
        match self.pipelines.get(pid)? {
            Pipeline::Render { vertex_layouts, topology, blends, .. } => {
                let stride = match vertex_layouts.first() {
                    Some(l) if l.stride as usize >= 8 => l.stride as usize,
                    _ => return Ok(None), // no position-bearing vertex layout
                };
                Ok(Some((*topology, blends.clone(), stride)))
            }
            Pipeline::Compute { .. } => Ok(None),
        }
    }

    /// Execute a non-indexed `Draw`: fetch `[first_vertex, first_vertex+vertex_count)` from slot-0's
    /// vertex buffer and rasterize into the bound color attachments.
    fn exec_draw(
        &mut self,
        pipeline: Option<u32>,
        targets: &[(u32, TextureFormat)],
        vertex_buffer: Option<(u32, u64)>,
        first_vertex: u32,
        vertex_count: u32,
        instance_count: u32,
    ) -> Result<()> {
        let (topology, blends, stride) = match self.raster_state(pipeline)? {
            Some(s) => s,
            None => return Ok(()),
        };
        let (vbuf, voff) = match vertex_buffer {
            Some(x) => x,
            None => return Ok(()),
        };
        let verts = {
            let b = self.buffers.get(vbuf)?;
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
        // The software oracle does not model per-instance vertex-attribute divisors, so each instance
        // replays the same geometry; N instances = N rasterization passes into the bound targets.
        for _ in 0..instance_count.max(1) {
            self.raster_draw(targets, &blends, topology, &verts)?;
        }
        Ok(())
    }

    /// Execute a `DrawIndexed`: read `index_count` indices from the bound index buffer, add `base_vertex`,
    /// gather the referenced slot-0 vertices, and rasterize.
    #[allow(clippy::too_many_arguments)]
    fn exec_draw_indexed(
        &mut self,
        pipeline: Option<u32>,
        targets: &[(u32, TextureFormat)],
        vertex_buffer: Option<(u32, u64)>,
        index_buffer: Option<(u32, u64, IndexFormat)>,
        first_index: u32,
        index_count: u32,
        base_vertex: i32,
        instance_count: u32,
    ) -> Result<()> {
        let (topology, blends, stride) = match self.raster_state(pipeline)? {
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
            let b = self.buffers.get(ibuf)?;
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
                    IndexFormat::U32 => u32::from_le_bytes([b.data[base], b.data[base + 1], b.data[base + 2], b.data[base + 3]]),
                };
                out.push(raw);
            }
            out
        };
        let verts = {
            let b = self.buffers.get(vbuf)?;
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
            self.raster_draw(targets, &blends, topology, &verts)?;
        }
        Ok(())
    }

    // --- validation helpers -----------------------------------------------------------------------

    fn buffer_with_usage(&self, id: u32, usage: u32, what: &'static str) -> Result<&Buffer> {
        let b = self.buffers.get(id)?;
        if b.usage & usage == 0 {
            return Err(GpuError::Invalid(what));
        }
        Ok(b)
    }

    fn texture_with_usage(&self, id: u32, usage: u32, what: &'static str) -> Result<&Texture> {
        let t = self.textures.get(id)?;
        if t.desc.usage & usage == 0 {
            return Err(GpuError::Invalid(what));
        }
        Ok(t)
    }

    /// Re-check that every resource a bind group referenced is still live *and* still the same
    /// allocation it was bound against (generation match), rejecting a stale reference into a reused
    /// id.
    fn check_bind_group_live(&self, bgid: u32) -> Result<&BindGroupState> {
        let bg = self.bind_groups.get(bgid)?;
        for r in &bg.buffers {
            if self.buffers.generation(r.id) != Some(r.gen) {
                return Err(GpuError::UnknownId { kind: BufferId::KIND, id: r.id });
            }
        }
        for r in &bg.textures {
            if self.textures.generation(r.id) != Some(r.gen) {
                return Err(GpuError::UnknownId { kind: TextureId::KIND, id: r.id });
            }
        }
        for r in &bg.samplers {
            if self.samplers.generation(r.id) != Some(r.gen) {
                return Err(GpuError::UnknownId { kind: SamplerId::KIND, id: r.id });
            }
        }
        Ok(bg)
    }

    /// Validate the whole command buffer against a simulated encoder state without mutating any
    /// resource. Returns `Ok` only if every op in the stream is legal, so the executor that follows
    /// cannot fail partway and leave partial side effects.
    fn validate_cb(&self, cb: &CommandBuffer) -> Result<()> {
        let mut st = EncoderState::default();
        for op in &cb.encoder {
            self.validate_op(op, &mut st)?;
        }
        if st.in_render_pass || st.in_compute_pass {
            return Err(GpuError::Invalid("command buffer ends inside an open pass"));
        }
        Ok(())
    }

    fn validate_op(&self, op: &Enc, st: &mut EncoderState) -> Result<()> {
        match op {
            Enc::BeginRenderPass { color, depth } => {
                if st.in_render_pass || st.in_compute_pass {
                    return Err(GpuError::Invalid("nested render pass"));
                }
                let mut formats = Vec::with_capacity(color.len());
                for c in color {
                    let t = self.texture_with_usage(
                        c.texture,
                        texture_usage::RENDER_TARGET,
                        "color attachment lacks RENDER_TARGET usage",
                    )?;
                    if t.desc.sample_count != 1 {
                        return Err(GpuError::Unsupported("software: multisample render attachment"));
                    }
                    if c.load == LoadOp::Clear {
                        Self::clear_texel(t.desc.format, c.clear)?; // reject unclearable formats up front
                    }
                    formats.push(t.desc.format);
                }
                if let Some(dp) = depth {
                    // A depth attachment must name a real texture created with render-target usage;
                    // the software oracle does not fabricate an internal depth buffer.
                    let t = self.texture_with_usage(
                        dp.texture,
                        texture_usage::RENDER_TARGET,
                        "depth attachment lacks RENDER_TARGET usage",
                    )?;
                    if t.desc.sample_count != 1 {
                        return Err(GpuError::Unsupported("software: multisample depth attachment"));
                    }
                }
                st.in_render_pass = true;
                st.color_targets = color.iter().map(|c| c.texture).collect();
                st.color_formats = formats;
            }
            Enc::EndRenderPass => {
                if !st.in_render_pass {
                    return Err(GpuError::Invalid("EndRenderPass outside a render pass"));
                }
                st.end_pass();
            }
            Enc::BeginComputePass => {
                if st.in_render_pass || st.in_compute_pass {
                    return Err(GpuError::Invalid("nested compute pass"));
                }
                st.in_compute_pass = true;
            }
            Enc::EndComputePass => {
                if !st.in_compute_pass {
                    return Err(GpuError::Invalid("EndComputePass outside a compute pass"));
                }
                st.end_pass();
            }
            Enc::SetPipeline(p) => {
                self.pipelines.get(*p)?;
                st.pipeline = Some(*p);
            }
            Enc::SetBindGroup { group, .. } => {
                self.bind_groups.get(*group)?;
                st.bind_group = Some(*group);
            }
            Enc::SetVertexBuffer { slot, buffer, offset } => {
                self.buffers.get(*buffer)?;
                st.vertex_buffers.insert(*slot, (*buffer, *offset));
            }
            Enc::SetIndexBuffer { buffer, offset, format } => {
                self.buffers.get(*buffer)?;
                st.index_buffer = Some((*buffer, *offset, *format));
            }
            Enc::SetViewport { min_depth, max_depth, .. } => {
                if !(0.0..=1.0).contains(min_depth)
                    || !(0.0..=1.0).contains(max_depth)
                    || min_depth > max_depth
                {
                    return Err(GpuError::Invalid("viewport depth range out of [0,1] or inverted"));
                }
            }
            Enc::SetScissor { .. } => {}
            Enc::ClearRect { texture, .. } => {
                let t = self.textures.get(*texture)?;
                if t.desc.sample_count != 1 {
                    return Err(GpuError::Unsupported("software: multisample clear"));
                }
            }
            Enc::Draw { vertex_count, instance_count, first_vertex, first_instance } => {
                self.validate_draw(st, |layout, slot| {
                    let (buffer, offset) = st
                        .vertex_buffers
                        .get(&slot)
                        .copied()
                        .ok_or(GpuError::Invalid("draw with no vertex buffer bound for a layout slot"))?;
                    let count = if layout.step_mode == 1 {
                        first_instance.checked_add(*instance_count)
                    } else {
                        first_vertex.checked_add(*vertex_count)
                    };
                    self.check_vertex_range(buffer, offset, layout.stride, count)
                })?;
            }
            Enc::DrawIndexed { index_count, first_index, .. } => {
                self.validate_draw(st, |layout, slot| {
                    // Indexed vertex fetch depends on index values we can't read here, so only require
                    // that a vertex buffer is bound for each layout slot.
                    st.vertex_buffers
                        .get(&slot)
                        .map(|_| ())
                        .ok_or(GpuError::Invalid("indexed draw with no vertex buffer bound for a layout slot"))?;
                    let _ = layout;
                    Ok(())
                })?;
                let (buffer, offset, format) = st
                    .index_buffer
                    .ok_or(GpuError::Invalid("indexed draw with no index buffer bound"))?;
                let isz = match format {
                    IndexFormat::U16 => 2usize,
                    IndexFormat::U32 => 4usize,
                };
                let last = first_index
                    .checked_add(*index_count)
                    .ok_or(GpuError::OutOfBounds)?;
                let need = (last as usize).checked_mul(isz).ok_or(GpuError::OutOfBounds)?;
                let b = self.buffers.get(buffer)?;
                let end = (offset as usize).checked_add(need).ok_or(GpuError::OutOfBounds)?;
                if end > b.data.len() {
                    return Err(GpuError::OutOfBounds);
                }
            }
            Enc::Dispatch { .. } => {
                if !st.in_compute_pass {
                    return Err(GpuError::Invalid("Dispatch outside a compute pass"));
                }
                match st.pipeline {
                    Some(p) => match self.pipelines.get(p)? {
                        Pipeline::Compute { .. } => {}
                        Pipeline::Render { .. } => {
                            return Err(GpuError::Unsupported("dispatch on a render pipeline"))
                        }
                    },
                    None => return Err(GpuError::Invalid("Dispatch with no pipeline bound")),
                }
                if let Some(bg) = st.bind_group {
                    self.check_bind_group_live(bg)?;
                }
            }
            Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size } => {
                let s = self.buffer_with_usage(*src, buffer_usage::COPY_SRC, "copy src lacks COPY_SRC")?;
                check_range(s.data.len(), *src_offset, *size)?;
                let d = self.buffer_with_usage(*dst, buffer_usage::COPY_DST, "copy dst lacks COPY_DST")?;
                check_range(d.data.len(), *dst_offset, *size)?;
            }
            Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, mip, width, height } => {
                if *mip != 0 {
                    return Err(GpuError::Unsupported("software: non-zero mip copy"));
                }
                let s = self.buffer_with_usage(*src, buffer_usage::COPY_SRC, "copy src lacks COPY_SRC")?;
                let t = self.texture_with_usage(*dst, texture_usage::COPY_DST, "copy dst lacks COPY_DST")?;
                if t.desc.sample_count != 1 {
                    return Err(GpuError::Unsupported("software: buffer copy to multisample texture"));
                }
                let (_, _, src_span) = texture_copy_layout(t, *width, *height, *bytes_per_row)?;
                check_len(s.data.len(), *src_offset, src_span)?;
            }
            Enc::CopyTextureToBuffer { src, mip, width, height, dst, dst_offset, bytes_per_row } => {
                if *mip != 0 {
                    return Err(GpuError::Unsupported("software: non-zero mip copy"));
                }
                let t = self.texture_with_usage(*src, texture_usage::COPY_SRC, "copy src lacks COPY_SRC")?;
                if t.desc.sample_count != 1 {
                    return Err(GpuError::Unsupported("software: multisample texture readback copy"));
                }
                let bpt = Self::texel_bytes(t.desc.format)?;
                if *dst_offset % bpt as u64 != 0 {
                    return Err(GpuError::Invalid("texture readback offset not texel-aligned"));
                }
                let (_, _, dst_span) = texture_copy_layout(t, *width, *height, *bytes_per_row)?;
                let d = self.buffer_with_usage(*dst, buffer_usage::COPY_DST, "copy dst lacks COPY_DST")?;
                check_len(d.data.len(), *dst_offset, dst_span)?;
            }
            Enc::CopyTextureToTexture { src, src_sub, src_origin, dst, dst_sub, dst_origin, extent } => {
                Self::check_copy_subresource(src_sub, src_origin, extent.depth)?;
                Self::check_copy_subresource(dst_sub, dst_origin, extent.depth)?;
                let s = self.texture_with_usage(*src, texture_usage::COPY_SRC, "copy src lacks COPY_SRC")?;
                let d = self.texture_with_usage(*dst, texture_usage::COPY_DST, "copy dst lacks COPY_DST")?;
                if s.desc.sample_count != 1 || d.desc.sample_count != 1 {
                    return Err(GpuError::Unsupported("software: multisample texture copy"));
                }
                // A texture→texture copy moves raw texels: the two formats must agree on texel size, else
                // the byte copy is meaningless (Vulkan requires size-compatible formats for vkCmdCopyImage).
                if Self::texel_bytes(s.desc.format)? != Self::texel_bytes(d.desc.format)? {
                    return Err(GpuError::Invalid("texture copy between incompatible texel sizes"));
                }
                check_region_in_texture(s, src_origin, extent)?;
                check_region_in_texture(d, dst_origin, extent)?;
            }
            Enc::BlitTexture {
                src,
                src_sub,
                src_origin,
                src_extent,
                dst,
                dst_sub,
                dst_origin,
                dst_extent,
                ..
            } => {
                Self::check_copy_subresource(src_sub, src_origin, src_extent.depth)?;
                Self::check_copy_subresource(dst_sub, dst_origin, dst_extent.depth)?;
                if src_extent.width == 0 || src_extent.height == 0 || dst_extent.width == 0 || dst_extent.height == 0 {
                    return Err(GpuError::Invalid("blit with a zero-sized region"));
                }
                let s = self.texture_with_usage(*src, texture_usage::COPY_SRC, "blit src lacks COPY_SRC")?;
                let d = self.texture_with_usage(*dst, texture_usage::COPY_DST, "blit dst lacks COPY_DST")?;
                if s.desc.sample_count != 1 || d.desc.sample_count != 1 {
                    return Err(GpuError::Unsupported("software: multisample blit"));
                }
                // A blit resamples per-texel; the oracle only handles equal-texel-size color formats.
                if Self::texel_bytes(s.desc.format)? != Self::texel_bytes(d.desc.format)? {
                    return Err(GpuError::Invalid("blit between incompatible texel sizes"));
                }
                check_region_in_texture(s, src_origin, src_extent)?;
                check_region_in_texture(d, dst_origin, dst_extent)?;
            }
            Enc::ResolveTexture { src, src_sub, src_origin, dst, dst_sub, dst_origin, extent } => {
                Self::check_copy_subresource(src_sub, src_origin, extent.depth)?;
                Self::check_copy_subresource(dst_sub, dst_origin, extent.depth)?;
                let s = self.texture_with_usage(*src, texture_usage::COPY_SRC, "resolve src lacks COPY_SRC")?;
                let d = self.texture_with_usage(*dst, texture_usage::COPY_DST, "resolve dst lacks COPY_DST")?;
                if s.desc.sample_count <= 1 || d.desc.sample_count != 1 {
                    return Err(GpuError::Invalid("resolve sample counts"));
                }
                if s.desc.format != d.desc.format {
                    return Err(GpuError::Invalid("resolve formats differ"));
                }
                check_region_in_texture(s, src_origin, extent)?;
                check_region_in_texture(d, dst_origin, extent)?;
            }
        }
        Ok(())
    }

    /// The software oracle materializes only 2D, single-layer, level-0 color textures, so a copy/blit
    /// subresource that names a non-zero mip/layer, a non-color aspect, or a 3D depth slice is rejected as
    /// `Unsupported` rather than silently aliasing level 0 (mirrors the `CopyBufferToTexture` mip guard).
    fn check_copy_subresource(sub: &TextureSubresource, origin: &Origin3d, depth: u32) -> Result<()> {
        if sub.mip != 0 {
            return Err(GpuError::Unsupported("software: non-zero mip texture copy"));
        }
        if sub.layer != 0 {
            return Err(GpuError::Unsupported("software: array-layer texture copy"));
        }
        if sub.aspect != TextureAspect::All {
            return Err(GpuError::Unsupported("software: non-color aspect texture copy"));
        }
        if origin.z != 0 || depth > 1 {
            return Err(GpuError::Unsupported("software: 3D/depth-slice texture copy"));
        }
        Ok(())
    }

    /// Shared draw validation: must be inside a render pass with a bound render pipeline whose color
    /// formats match the current attachments, and (via `per_layout`) each vertex layout satisfied.
    /// Also rejects the read/write hazard of sampling a texture that is a current color attachment.
    fn validate_draw<F>(&self, st: &EncoderState, mut per_layout: F) -> Result<()>
    where
        F: FnMut(&VertexLayout, u32) -> Result<()>,
    {
        if !st.in_render_pass {
            return Err(GpuError::Invalid("draw outside a render pass"));
        }
        let pid = st.pipeline.ok_or(GpuError::Invalid("draw with no pipeline bound"))?;
        let (color_formats, vertex_layouts) = match self.pipelines.get(pid)? {
            Pipeline::Render { color_formats, vertex_layouts, .. } => (color_formats, vertex_layouts),
            Pipeline::Compute { .. } => return Err(GpuError::Unsupported("draw on a compute pipeline")),
        };
        // Pipeline color-target formats must be compatible with the render pass attachments.
        if color_formats.len() != st.color_formats.len()
            || color_formats.iter().zip(&st.color_formats).any(|(a, b)| a != b)
        {
            return Err(GpuError::Invalid("pipeline color format mismatches render attachment"));
        }
        for (slot, layout) in vertex_layouts.iter().enumerate() {
            per_layout(layout, slot as u32)?;
        }
        if let Some(bg) = st.bind_group {
            let bg = self.check_bind_group_live(bg)?;
            for r in &bg.textures {
                if st.color_targets.contains(&r.id) {
                    return Err(GpuError::Invalid("texture sampled while bound as a color attachment"));
                }
            }
        }
        Ok(())
    }

    fn check_vertex_range(&self, buffer: u32, offset: u64, stride: u32, count: Option<u32>) -> Result<()> {
        let b = self.buffers.get(buffer)?;
        let count = count.ok_or(GpuError::OutOfBounds)?;
        let need = (count as u64)
            .checked_mul(stride as u64)
            .ok_or(GpuError::OutOfBounds)?;
        let end = offset.checked_add(need).ok_or(GpuError::OutOfBounds)?;
        if end > b.data.len() as u64 {
            return Err(GpuError::OutOfBounds);
        }
        Ok(())
    }
}

/// Simulated encoder state used by the validation pass.
#[derive(Default)]
struct EncoderState {
    in_render_pass: bool,
    in_compute_pass: bool,
    pipeline: Option<u32>,
    bind_group: Option<u32>,
    vertex_buffers: HashMap<u32, (u32, u64)>,
    index_buffer: Option<(u32, u64, IndexFormat)>,
    color_targets: Vec<u32>,
    color_formats: Vec<TextureFormat>,
}

impl EncoderState {
    fn end_pass(&mut self) {
        self.in_render_pass = false;
        self.in_compute_pass = false;
        self.color_targets.clear();
        self.color_formats.clear();
    }
}

/// Resolve a bind-group buffer slice's `(offset, len)`, treating `size == 0` as "to the end of the
/// buffer" and rejecting any offset/size that would run past the buffer (with wrapping-safe math).
fn buffer_slice_bounds(buf_len: usize, offset: u64, size: u64) -> Result<(usize, usize)> {
    if offset > buf_len as u64 {
        return Err(GpuError::OutOfBounds);
    }
    let off = offset as usize;
    let len = if size == 0 {
        buf_len - off
    } else {
        let end = offset.checked_add(size).ok_or(GpuError::OutOfBounds)?;
        if end > buf_len as u64 {
            return Err(GpuError::OutOfBounds);
        }
        size as usize
    };
    Ok((off, len))
}

/// Bounds-check `[offset, offset+size)` against `len` with wrapping-safe arithmetic.
fn check_range(len: usize, offset: u64, size: u64) -> Result<()> {
    let end = offset.checked_add(size).ok_or(GpuError::OutOfBounds)?;
    if end > len as u64 {
        return Err(GpuError::OutOfBounds);
    }
    Ok(())
}

/// Bounds-check a `usize` span starting at `offset` against `len`.
fn check_len(len: usize, offset: u64, span: usize) -> Result<()> {
    check_range(len, offset, span as u64)
}

/// Compute `(row_bytes, tight_bytes, buffer_span)` for a width×height texture copy, honoring the row
/// stride and validating the extent against the texture dimensions. `bytes_per_row == 0` means tight.
fn texture_copy_layout(t: &Texture, width: u32, height: u32, bytes_per_row: u32) -> Result<(usize, usize, usize)> {
    if width > t.desc.width || height > t.desc.height {
        return Err(GpuError::OutOfBounds);
    }
    let bpt = t
        .desc
        .format
        .bytes_per_texel()
        .ok_or(GpuError::Unsupported("software: non-color texture format"))?;
    let row_bytes = bpt.checked_mul(width as usize).ok_or(GpuError::OutOfBounds)?;
    let rows = height as usize;
    let stride = if bytes_per_row == 0 { row_bytes } else { bytes_per_row as usize };
    if stride < row_bytes {
        return Err(GpuError::OutOfBounds);
    }
    let tight = row_bytes.checked_mul(rows).ok_or(GpuError::OutOfBounds)?;
    let span = if rows == 0 {
        0
    } else {
        (rows - 1)
            .checked_mul(stride)
            .and_then(|v| v.checked_add(row_bytes))
            .ok_or(GpuError::OutOfBounds)?
    };
    Ok((row_bytes, tight, span))
}

/// Bounds-check that `[origin, origin+extent)` lies fully within a texture's level-0 plane (wrapping-safe).
fn check_region_in_texture(t: &Texture, origin: &Origin3d, extent: &Extent3d) -> Result<()> {
    let x_end = origin.x.checked_add(extent.width).ok_or(GpuError::OutOfBounds)?;
    let y_end = origin.y.checked_add(extent.height).ok_or(GpuError::OutOfBounds)?;
    if x_end > t.desc.width || y_end > t.desc.height {
        return Err(GpuError::OutOfBounds);
    }
    Ok(())
}

/// Fetch one texel (`bpt` bytes) from a tight-packed level-0 plane at `(x, y)`.
fn texel_at(pixels: &[u8], tex_w: usize, x: usize, y: usize, bpt: usize) -> &[u8] {
    let off = (y * tex_w + x) * bpt;
    &pixels[off..off + bpt]
}

/// The IEC 61966-2-1 sRGB EOTF on a normalized [0,1] value (electrical → linear-light optical).
fn srgb_to_linear(x: f32) -> f32 {
    if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
}
fn srgb_decode(v: u8) -> f32 {
    srgb_to_linear(v as f32 / 255.0)
}
fn srgb_encode(v: f32) -> u8 {
    let x = v.clamp(0.0, 1.0);
    let y = if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
    (y * 255.0 + 0.5) as u8
}
fn is_srgb(f: TextureFormat) -> bool {
    matches!(f, TextureFormat::Rgba8Srgb | TextureFormat::Bgra8Srgb)
}

/// Logical-RGBA → byte-offset permutation for the oracle's 4-channel color formats. `Bgra*` stores
/// blue and red swapped; alpha is always the last byte. Returns `None` for a non-4-channel format.
fn rgba_channel_order(f: TextureFormat) -> Option<[usize; 4]> {
    match f {
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Srgb => Some([0, 1, 2, 3]),
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb => Some([2, 1, 0, 3]),
        _ => None,
    }
}

/// Decode a stored 4-byte texel into straight (non-premultiplied) linear-light RGBA in [0,1]. Color
/// channels pass through the sRGB EOTF for an sRGB format; alpha is always linear (a coverage value, so
/// it is never gamma-encoded — matching the filtering path).
fn load_texel_linear(bytes: &[u8], order: [usize; 4], srgb: bool) -> [f32; 4] {
    let dec = |b: u8| if srgb { srgb_decode(b) } else { b as f32 / 255.0 };
    [dec(bytes[order[0]]), dec(bytes[order[1]]), dec(bytes[order[2]]), bytes[order[3]] as f32 / 255.0]
}

/// Encode straight linear-light RGBA back into a stored 4-byte texel (inverse of [`load_texel_linear`]).
fn store_texel_linear(bytes: &mut [u8], order: [usize; 4], srgb: bool, rgba: [f32; 4]) {
    let enc = |v: f32| if srgb { srgb_encode(v) } else { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 };
    bytes[order[0]] = enc(rgba[0]);
    bytes[order[1]] = enc(rgba[1]);
    bytes[order[2]] = enc(rgba[2]);
    bytes[order[3]] = (rgba[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
}

/// One vertex the software oracle's draw path consumes. The oracle's fixed draw ABI is
/// `[x, y, r, g, b, a]` little-endian `f32` per vertex: position at byte offset 0, straight-alpha color
/// at byte offset 8. `pos` is NDC clip space (x right, y up, both in [-1,1]); `color` is straight
/// (non-premultiplied) RGBA in [0,1] expressed in the target attachment's OWN encoding (i.e. sRGB
/// electrical values for an sRGB target). A vertex stride < 24 carries position only and color defaults
/// to opaque white.
#[derive(Clone, Copy)]
struct DrawVertex {
    pos: [f32; 2],
    color: [f32; 4],
}

/// Read one [`DrawVertex`] out of a tight/strided vertex buffer at byte `base` (caller guarantees the
/// position bytes are in-bounds; color is read only when the stride carries it).
fn read_vertex(data: &[u8], base: usize, stride: usize) -> DrawVertex {
    let f = |o: usize| f32::from_le_bytes([data[base + o], data[base + o + 1], data[base + o + 2], data[base + o + 3]]);
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

/// Integer pixel bounding box `[minx,maxx) × [miny,maxy)` of a framebuffer-space triangle, clamped to
/// the target dimensions.
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

/// Bilinearly sample a tight-packed color plane at fractional `(fx, fy)` (in absolute texel space),
/// clamping neighbors to `[lo, hi]` in each axis — the oracle's `Filter::Linear` blit path.
fn sample_bilinear(
    pixels: &[u8],
    tex_w: usize,
    bpt: usize,
    fx: f32,
    fy: f32,
    x_lo: usize,
    x_hi: usize,
    y_lo: usize,
    y_hi: usize,
    format: TextureFormat,
) -> Vec<u8> {
    // Clamp the sample point to the texel-center range so a coordinate left of the first center (or right
    // of the last) resolves to the edge texel with zero fractional weight (clamp-to-edge, like a real GPU).
    let gx = (fx - 0.5).clamp(x_lo as f32, x_hi as f32);
    let gy = (fy - 0.5).clamp(y_lo as f32, y_hi as f32);
    let x0 = gx.floor() as usize;
    let y0 = gy.floor() as usize;
    let x1 = (x0 + 1).min(x_hi);
    let y1 = (y0 + 1).min(y_hi);
    let tx = gx - x0 as f32;
    let ty = gy - y0 as f32;
    let p00 = texel_at(pixels, tex_w, x0, y0, bpt);
    let p10 = texel_at(pixels, tex_w, x1, y0, bpt);
    let p01 = texel_at(pixels, tex_w, x0, y1, bpt);
    let p11 = texel_at(pixels, tex_w, x1, y1, bpt);
    let mut out = Vec::with_capacity(bpt);
    for c in 0..bpt {
        let cv = |v: u8| if c < 3 && is_srgb(format) { srgb_decode(v) } else { v as f32 / 255.0 };
        let top = cv(p00[c]) * (1.0 - tx) + cv(p10[c]) * tx;
        let bot = cv(p01[c]) * (1.0 - tx) + cv(p11[c]) * tx;
        let v = top * (1.0 - ty) + bot * ty;
        out.push(if c < 3 && is_srgb(format) { srgb_encode(v) } else { (v * 255.0 + 0.5) as u8 });
    }
    out
}

impl GpuBackend for SoftwareBackend {
    fn capabilities(&self) -> Capabilities {
        use crate::backend::{command_bits, format_bits, shader_payload, ALL_COMMANDS, COLOR_FORMATS};
        Capabilities {
            name: "hl-software".into(),
            unified_memory: true, // it's all host memory
            supports_compute: true, // executes compiled PTX kernels (hl-GPU kernel IR) on the CPU
            supports_graphics: true, // clear/copy only
            max_texture_2d: 8192,
            present_kinds: vec![PresentKind::Shm],
            wire_version: crate::ir::WIRE_VERSION,
            // Validates/replays every encoder op (clears + copies + dispatch; draws are recorded).
            command_bits: command_bits(ALL_COMMANDS),
            // Executes compiled PTX kernels; it cannot run a graphics (SPIR-V/MSL) shader, so PTX is the
            // only payload it truthfully executes.
            shader_payloads: shader_payload::PTX,
            // Color formats only — the CPU oracle does not materialize depth/stencil.
            texture_formats: format_bits(COLOR_FORMATS),
            max_frame_bytes: 64 << 20,
            max_buffer_bytes: 256 << 20,
            max_bind_groups: 4,
            supports_timeline_fences: false, // synchronous; a fence only reaches a value a submit signalled
        }
    }

    fn create_buffer(&mut self, id: BufferId, desc: &BufferDesc) -> Result<()> {
        self.buffers.insert(id.0, Buffer { data: vec![0u8; desc.size as usize], usage: desc.usage })
    }
    fn destroy_buffer(&mut self, id: BufferId) -> Result<()> {
        self.buffers.remove(id.0).map(|_| ())
    }
    fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> Result<()> {
        let b = self.buffers.get_mut(id.0)?;
        let off = offset as usize;
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|e| *e <= b.data.len() as u64)
            .ok_or(GpuError::OutOfBounds)? as usize;
        b.data[off..end].copy_from_slice(data);
        Ok(())
    }
    fn read_buffer(&mut self, id: BufferId, offset: u64, out: &mut [u8]) -> Result<()> {
        let b = self.buffers.get(id.0)?;
        let off = offset as usize;
        let end = offset
            .checked_add(out.len() as u64)
            .filter(|e| *e <= b.data.len() as u64)
            .ok_or(GpuError::OutOfBounds)? as usize;
        out.copy_from_slice(&b.data[off..end]);
        Ok(())
    }

    fn create_texture(&mut self, id: TextureId, desc: &TextureDesc) -> Result<()> {
        // Reject descriptors whose shape the software oracle cannot faithfully materialize, rather
        // than silently flattening/downleveling them (which would diverge from a real backend).
        if desc.width == 0 || desc.height == 0 {
            return Err(GpuError::Invalid("zero-sized texture"));
        }
        if desc.dim != TextureDim::D2 || desc.depth != 1 {
            return Err(GpuError::Unsupported("software: only 2D single-layer textures"));
        }
        if desc.mip_levels == 0 {
            return Err(GpuError::Invalid("texture mip_levels must be >= 1"));
        }
        if !matches!(desc.sample_count, 1 | 2 | 4 | 8) {
            return Err(GpuError::Unsupported("software: unsupported sample count"));
        }
        let bpt = Self::texel_bytes(desc.format)?;
        let n = bpt
            .checked_mul(desc.width as usize)
            .and_then(|v| v.checked_mul(desc.height as usize))
            .and_then(|v| v.checked_mul(desc.sample_count as usize))
            .ok_or(GpuError::OutOfBounds)?;
        self.textures.insert(id.0, Texture { desc: desc.clone(), pixels: vec![0u8; n] })
    }
    fn destroy_texture(&mut self, id: TextureId) -> Result<()> {
        self.textures.remove(id.0).map(|_| ())
    }
    fn read_texture(&mut self, id: TextureId, out: &mut [u8]) -> Result<()> {
        let t = self.textures.get(id.0)?;
        if out.len() != t.pixels.len() {
            return Err(GpuError::OutOfBounds);
        }
        out.copy_from_slice(&t.pixels);
        Ok(())
    }

    fn create_sampler(&mut self, id: SamplerId, _desc: &SamplerDesc) -> Result<()> {
        self.samplers.insert(id.0, ())
    }
    fn destroy_sampler(&mut self, id: SamplerId) -> Result<()> {
        self.samplers.remove(id.0).map(|_| ())
    }

    fn create_shader(&mut self, id: ShaderId, kind: crate::ir::ShaderPayloadKind, spirv: &[u32]) -> Result<()> {
        // An empty shader module is never valid — reject it rather than record an unusable module
        // that would later fall through to a builtin or a no-op draw.
        if spirv.is_empty() {
            return Err(GpuError::Invalid("empty shader module"));
        }
        // A hl-GPU kernel descriptor (forwarded PTX + launch config) is compiled to an executable
        // kernel program here; anything else is treated as opaque SPIR-V (recorded, not run).
        let module = match kind {
            crate::ir::ShaderPayloadKind::PtxKernel => {
                let desc = KernelDescriptor::from_words(spirv)
                    .ok_or(GpuError::Invalid("malformed PTX kernel shader payload"))?;
                let desc = desc?;
                let prog = ptx::compile(&desc.ptx, &desc.entry, desc.block)?;
                ShaderModule::Kernel(Box::new(prog))
            }
            crate::ir::ShaderPayloadKind::SpirV => {
                if spirv.first() != Some(&0x0723_0203) {
                    return Err(GpuError::Invalid("malformed SPIR-V shader payload"));
                }
                ShaderModule::Spirv
            }
            crate::ir::ShaderPayloadKind::LegacyMsl | crate::ir::ShaderPayloadKind::DemoBuiltin => {
                ShaderModule::Spirv
            }
        };
        self.shaders.insert(id.0, module)
    }
    fn destroy_shader(&mut self, id: ShaderId) -> Result<()> {
        self.shaders.remove(id.0).map(|_| ())
    }

    fn create_render_pipeline(&mut self, id: PipelineId, desc: &RenderPipelineDesc) -> Result<()> {
        self.shaders.get(desc.vertex.module)?;
        if let Some(f) = &desc.fragment {
            self.shaders.get(f.module)?;
        }
        // Reject impossible vertex layouts: an attribute must start within the vertex stride.
        for vb in &desc.vertex_buffers {
            for a in &vb.attrs {
                if vb.stride == 0 || a.offset >= vb.stride {
                    return Err(GpuError::Invalid("vertex attribute offset outside stride"));
                }
            }
        }
        self.pipelines.insert(
            id.0,
            Pipeline::Render {
                color_formats: desc.color_targets.iter().map(|c| c.format).collect(),
                vertex_layouts: desc.vertex_buffers.clone(),
                topology: desc.topology,
                blends: desc.color_targets.iter().map(|c| c.blend.clone()).collect(),
            },
        )
    }
    fn create_compute_pipeline(&mut self, id: PipelineId, desc: &ComputePipelineDesc) -> Result<()> {
        self.shaders.get(desc.compute.module)?;
        self.pipelines.insert(id.0, Pipeline::Compute { shader: desc.compute.module })
    }
    fn destroy_pipeline(&mut self, id: PipelineId) -> Result<()> {
        self.pipelines.remove(id.0).map(|_| ())
    }

    fn create_bind_group(&mut self, id: BindGroupId, desc: &BindGroupDesc) -> Result<()> {
        let mut buffers = Vec::new();
        let mut textures = Vec::new();
        let mut samplers = Vec::new();
        for e in &desc.entries {
            match &e.resource {
                BindResource::Buffer { id, offset, size } => {
                    let b = self.buffers.get(*id)?;
                    // Reject a slice that runs past the buffer end (wrapping-safe).
                    buffer_slice_bounds(b.data.len(), *offset, *size)?;
                    buffers.push(GenRef { id: *id, gen: self.buffers.generation(*id).unwrap() });
                }
                BindResource::Texture { id } => {
                    // A texture bound as a (sampled) resource must actually be usable as one.
                    self.texture_with_usage(*id, texture_usage::SAMPLED, "texture bound without SAMPLED usage")?;
                    textures.push(GenRef { id: *id, gen: self.textures.generation(*id).unwrap() });
                }
                BindResource::Sampler { id } => {
                    self.samplers.get(*id)?;
                    samplers.push(GenRef { id: *id, gen: self.samplers.generation(*id).unwrap() });
                }
            }
        }
        self.bind_groups.insert(id.0, BindGroupState { desc: desc.clone(), buffers, textures, samplers })
    }
    fn destroy_bind_group(&mut self, id: BindGroupId) -> Result<()> {
        self.bind_groups.remove(id.0).map(|_| ())
    }

    fn create_surface(&mut self, id: SurfaceId, desc: &SurfaceDesc) -> Result<()> {
        self.surfaces.insert(id.0, desc.clone())
    }
    fn destroy_surface(&mut self, id: SurfaceId) -> Result<()> {
        self.surfaces.remove(id.0).map(|_| ())
    }

    fn create_fence(&mut self, id: FenceId) -> Result<()> {
        self.fences.insert(id.0, 0)
    }
    fn destroy_fence(&mut self, id: FenceId) -> Result<()> {
        self.fences.remove(id.0).map(|_| ())
    }
    fn wait_fence(&mut self, id: FenceId, value: u64) -> Result<()> {
        // A wait must *observe* completion, not fabricate it. The executor runs submits synchronously,
        // so a fence only reaches `value` if a submit signalled it there; otherwise the wait is on an
        // unreached value and is a validation error, not a silent success.
        let v = *self.fences.get(id.0)?;
        if v < value {
            return Err(GpuError::Invalid("wait on a fence value that was never signalled"));
        }
        Ok(())
    }

    fn submit(&mut self, cb: &CommandBuffer) -> Result<()> {
        // Validate the entire stream first so a failure leaves all resources untouched (no partial
        // side effects), then execute the clears/copies/dispatches.
        self.validate_cb(cb)?;

        let mut cur_pipeline: Option<u32> = None;
        let mut cur_bind_group: Option<u32> = None;
        // Live raster state carried across the encoder (mirrors the fixed-function pipeline a draw reads).
        let mut cur_targets: Vec<(u32, TextureFormat)> = Vec::new();
        let mut cur_vertex: HashMap<u32, (u32, u64)> = HashMap::new();
        let mut cur_index: Option<(u32, u64, IndexFormat)> = None;
        for op in &cb.encoder {
            match op {
                Enc::BeginRenderPass { color, .. } => {
                    cur_targets.clear();
                    for c in color {
                        let (fmt, w, h) = {
                            let t = self.textures.get(c.texture)?;
                            (t.desc.format, t.desc.width, t.desc.height)
                        };
                        cur_targets.push((c.texture, fmt));
                        if c.load == LoadOp::Clear {
                            let texel = Self::clear_texel(fmt, c.clear)?;
                            let t = self.textures.get_mut(c.texture)?;
                            let n = (w * h) as usize;
                            t.pixels.clear();
                            t.pixels.reserve(n * texel.len());
                            for _ in 0..n {
                                t.pixels.extend_from_slice(&texel);
                            }
                        }
                    }
                }
                Enc::EndRenderPass => cur_targets.clear(),
                Enc::ClearRect { texture, x, y, w, h, color } => {
                    self.clear_rect(*texture, *x, *y, *w, *h, *color)?;
                }
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetBindGroup { group, .. } => cur_bind_group = Some(*group),
                Enc::SetVertexBuffer { slot, buffer, offset } => {
                    cur_vertex.insert(*slot, (*buffer, *offset));
                }
                Enc::SetIndexBuffer { buffer, offset, format } => {
                    cur_index = Some((*buffer, *offset, *format));
                }
                Enc::Draw { vertex_count, first_vertex, instance_count, .. } => {
                    self.draws += 1;
                    let vb = cur_vertex.get(&0).copied();
                    self.exec_draw(cur_pipeline, &cur_targets, vb, *first_vertex, *vertex_count, *instance_count)?;
                }
                Enc::DrawIndexed { index_count, first_index, base_vertex, instance_count, .. } => {
                    self.draws += 1;
                    let vb = cur_vertex.get(&0).copied();
                    self.exec_draw_indexed(
                        cur_pipeline, &cur_targets, vb, cur_index, *first_index, *index_count, *base_vertex, *instance_count,
                    )?;
                }
                Enc::Dispatch { x, y, z } => {
                    self.dispatches += 1;
                    self.run_dispatch(cur_pipeline, cur_bind_group, (*x, *y, *z))?;
                }
                Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size } => {
                    let chunk = {
                        let s = self.buffers.get(*src)?;
                        let so = *src_offset as usize;
                        let sz = *size as usize;
                        s.data[so..so + sz].to_vec()
                    };
                    let d = self.buffers.get_mut(*dst)?;
                    let d_off = *dst_offset as usize;
                    d.data[d_off..d_off + chunk.len()].copy_from_slice(&chunk);
                }
                Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, width, height, .. } => {
                    let (row_bytes, tight, _span) = {
                        let t = self.textures.get(*dst)?;
                        texture_copy_layout(t, *width, *height, *bytes_per_row)?
                    };
                    let rows = *height as usize;
                    let src_stride = if *bytes_per_row == 0 { row_bytes } else { *bytes_per_row as usize };
                    let so = *src_offset as usize;
                    let chunk = {
                        let s = self.buffers.get(*src)?;
                        let mut out = Vec::with_capacity(tight);
                        for row in 0..rows {
                            let start = so + row * src_stride;
                            out.extend_from_slice(&s.data[start..start + row_bytes]);
                        }
                        out
                    };
                    let t = self.textures.get_mut(*dst)?;
                    t.pixels[..tight].copy_from_slice(&chunk);
                }
                Enc::CopyTextureToBuffer { src, width, height, dst, dst_offset, bytes_per_row, .. } => {
                    // Read tight texel rows out of the texture, then write them into the destination
                    // buffer honoring the requested row stride (padding bytes between rows are left
                    // untouched, matching a real GPU readback).
                    let (row_bytes, _tight, _span) = {
                        let t = self.textures.get(*src)?;
                        texture_copy_layout(t, *width, *height, *bytes_per_row)?
                    };
                    let rows = *height as usize;
                    let dst_stride = if *bytes_per_row == 0 { row_bytes } else { *bytes_per_row as usize };
                    let (tw, bpt) = {
                        let t = self.textures.get(*src)?;
                        (t.desc.width as usize, Self::texel_bytes(t.desc.format)?)
                    };
                    let rows_data: Vec<u8> = {
                        let t = self.textures.get(*src)?;
                        let mut out = Vec::with_capacity(rows * row_bytes);
                        for row in 0..rows {
                            let start = row * tw * bpt;
                            out.extend_from_slice(&t.pixels[start..start + row_bytes]);
                        }
                        out
                    };
                    let d = self.buffers.get_mut(*dst)?;
                    let base = *dst_offset as usize;
                    for row in 0..rows {
                        let dstart = base + row * dst_stride;
                        d.data[dstart..dstart + row_bytes]
                            .copy_from_slice(&rows_data[row * row_bytes..row * row_bytes + row_bytes]);
                    }
                }
                Enc::CopyTextureToTexture { src, src_origin, dst, dst_origin, extent, .. } => {
                    // Move `extent` texels from src's level-0 plane into dst's, row by row. Validation
                    // already proved equal texel size and in-bounds regions.
                    let (sw, bpt) = {
                        let t = self.textures.get(*src)?;
                        (t.desc.width as usize, Self::texel_bytes(t.desc.format)?)
                    };
                    let ew = extent.width as usize;
                    let eh = extent.height as usize;
                    let row_bytes = ew * bpt;
                    let block: Vec<u8> = {
                        let t = self.textures.get(*src)?;
                        let mut out = Vec::with_capacity(row_bytes * eh);
                        for row in 0..eh {
                            let sy = src_origin.y as usize + row;
                            let sx = src_origin.x as usize;
                            let start = (sy * sw + sx) * bpt;
                            out.extend_from_slice(&t.pixels[start..start + row_bytes]);
                        }
                        out
                    };
                    let dw = self.textures.get(*dst)?.desc.width as usize;
                    let t = self.textures.get_mut(*dst)?;
                    for row in 0..eh {
                        let dy = dst_origin.y as usize + row;
                        let dx = dst_origin.x as usize;
                        let dstart = (dy * dw + dx) * bpt;
                        t.pixels[dstart..dstart + row_bytes]
                            .copy_from_slice(&block[row * row_bytes..(row + 1) * row_bytes]);
                    }
                }
                Enc::BlitTexture { src, src_origin, src_extent, dst, dst_origin, dst_extent, filter, .. } => {
                    // Resample src's [src_origin, src_extent) region into dst's [dst_origin, dst_extent)
                    // region — nearest or bilinear. Clone the source plane so a blit-to-self is well-defined.
                    let (sw, bpt, src_fmt) = {
                        let t = self.textures.get(*src)?;
                        (t.desc.width as usize, Self::texel_bytes(t.desc.format)?,t.desc.format)
                    };
                    let src_pixels = self.textures.get(*src)?.pixels.clone();
                    let (sox, soy) = (src_origin.x as usize, src_origin.y as usize);
                    let (sew, seh) = (src_extent.width as usize, src_extent.height as usize);
                    let (dew, deh) = (dst_extent.width as usize, dst_extent.height as usize);
                    let dw = self.textures.get(*dst)?.desc.width as usize;
                    let t = self.textures.get_mut(*dst)?;
                    for dy in 0..deh {
                        // Map the dst texel center back into the src region (absolute texel space).
                        let fy = soy as f32 + (dy as f32 + 0.5) * seh as f32 / deh as f32;
                        for dx in 0..dew {
                            let fx = sox as f32 + (dx as f32 + 0.5) * sew as f32 / dew as f32;
                            let texel = match filter {
                                Filter::Nearest => {
                                    let sx = (fx as usize).clamp(sox, sox + sew - 1);
                                    let sy = (fy as usize).clamp(soy, soy + seh - 1);
                                    texel_at(&src_pixels, sw, sx, sy, bpt).to_vec()
                                }
                                Filter::Linear => sample_bilinear(
                                    &src_pixels, sw, bpt, fx, fy,
                                    sox, sox + sew - 1, soy, soy + seh - 1,
                                    src_fmt,
                                ),
                            };
                            let ddx = dst_origin.x as usize + dx;
                            let ddy = dst_origin.y as usize + dy;
                            let off = (ddy * dw + ddx) * bpt;
                            t.pixels[off..off + bpt].copy_from_slice(&texel);
                        }
                    }
                }
                Enc::ResolveTexture { src, src_origin, dst, dst_origin, extent, .. } => {
                    let (sw, samples, bpt, source) = {
                        let t = self.textures.get(*src)?;
                        (t.desc.width as usize, t.desc.sample_count as usize,
                         Self::texel_bytes(t.desc.format)?, t.pixels.clone())
                    };
                    let dw = self.textures.get(*dst)?.desc.width as usize;
                    let t = self.textures.get_mut(*dst)?;
                    for y in 0..extent.height as usize {
                        for x in 0..extent.width as usize {
                            let sp = ((src_origin.y as usize + y) * sw + src_origin.x as usize + x)
                                * samples * bpt;
                            let dp = ((dst_origin.y as usize + y) * dw + dst_origin.x as usize + x) * bpt;
                            for channel in 0..bpt {
                                let sum: u32 = (0..samples)
                                    .map(|sample| source[sp + sample * bpt + channel] as u32)
                                    .sum();
                                t.pixels[dp + channel] = (sum / samples as u32) as u8;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some((f, v)) = cb.signal {
            let slot = self.fences.get_mut(f)?;
            *slot = (*slot).max(v);
        }
        Ok(())
    }

    fn present(&mut self, surface: SurfaceId, texture: TextureId) -> Result<PresentToken> {
        let sdesc = self.surfaces.get(surface.0)?.clone();
        let t = self.textures.get(texture.0)?;
        if t.desc.sample_count != 1 {
            return Err(GpuError::Unsupported("software: present multisample texture"));
        }
        // A present must hand over a frame that matches the surface geometry; a size mismatch is a
        // swapchain bug, not a valid present.
        if t.desc.width != sdesc.width || t.desc.height != sdesc.height {
            return Err(GpuError::Invalid("present texture size does not match surface"));
        }
        let format_ok = t.desc.format == sdesc.format;
        let handle = self.next_present_handle;
        self.next_present_handle += 1;
        Ok(PresentToken {
            surface: surface.0,
            kind: PresentKind::Shm,
            handle,
            width: t.desc.width,
            height: t.desc.height,
            format_ok,
        })
    }
}

#[cfg(test)]
mod srgb_tests {
    use super::*;
    #[test]
    fn bilinear_filtering_is_linear_light_and_alpha_is_linear() {
        let p = [0, 0, 0, 0, 255, 255, 255, 255];
        assert_eq!(sample_bilinear(&p, 2, 4, 1.0, 0.5, 0, 1, 0, 0, TextureFormat::Rgba8Srgb), [188, 188, 188, 128]);
    }
    #[test]
    fn unorm_math_and_raw_copies_remain_byte_domain() {
        let p = [0, 0, 0, 0, 255, 255, 255, 255];
        assert_eq!(sample_bilinear(&p, 2, 4, 1.0, 0.5, 0, 1, 0, 0, TextureFormat::Rgba8Unorm), [128, 128, 128, 128]);
    }

    // ---- software draw rasterization + linear-light premultiplied blending (golden pixels) ---------

    /// A straight-alpha source-over blend factor set (One, OneMinusSrcAlpha). The oracle keys off
    /// `blend.is_some()` — the exact factor enum values are not interpreted — but realistic values keep
    /// the fixture honest.
    fn over_blend() -> BlendState {
        BlendState { src_color: 1, dst_color: 6, op_color: 0, src_alpha: 1, dst_alpha: 6, op_alpha: 0 }
    }

    fn draw_pipeline(fmt: TextureFormat, blend: Option<BlendState>) -> RenderPipelineDesc {
        RenderPipelineDesc {
            vertex: ShaderRef { module: 1, entry: "vs".into() },
            fragment: Some(ShaderRef { module: 1, entry: "fs".into() }),
            // stride 24 = [pos.xy, color.rgba] as 6×f32 — the oracle's fixed software draw ABI.
            vertex_buffers: vec![VertexLayout {
                stride: 24,
                step_mode: 0,
                attrs: vec![
                    VertexAttr { location: 0, format: 0, offset: 0 },
                    VertexAttr { location: 1, format: 0, offset: 8 },
                ],
            }],
            color_targets: vec![ColorTargetState { format: fmt, blend, write_mask: 0xF }],
            depth: None,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            label: String::new(),
        }
    }

    /// Pack `[x, y, r, g, b, a]` vertices into little-endian f32 vertex-buffer bytes.
    fn vbytes(verts: &[[f32; 6]]) -> Vec<u8> {
        let mut out = Vec::new();
        for v in verts {
            for f in v {
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        out
    }

    /// Build a backend with a render pipeline (id 1), a `w`×`h` target (tex 1, `fmt`), and a vertex
    /// buffer (buf 1) holding `verts`. Ready to receive a draw into tex 1.
    fn draw_harness(fmt: TextureFormat, blend: Option<BlendState>, w: u32, h: u32, verts: &[[f32; 6]]) -> SoftwareBackend {
        let mut be = SoftwareBackend::new();
        be.create_shader(ShaderId(1), ShaderPayloadKind::DemoBuiltin, &[1, 2, 3]).unwrap();
        be.create_render_pipeline(PipelineId(1), &draw_pipeline(fmt, blend)).unwrap();
        be.create_texture(TextureId(1), &TextureDesc {
            width: w, height: h, depth: 1, mip_levels: 1, sample_count: 1, dim: TextureDim::D2,
            format: fmt, usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC, label: String::new(),
        }).unwrap();
        let data = vbytes(verts);
        be.create_buffer(BufferId(1), &BufferDesc {
            size: data.len() as u64, usage: buffer_usage::VERTEX | buffer_usage::COPY_DST, label: String::new(),
        }).unwrap();
        be.write_buffer(BufferId(1), 0, &data).unwrap();
        be
    }

    /// A single full-screen triangle (NDC) that covers every pixel of the target, carrying `color`.
    fn fullscreen_tri(color: [f32; 4]) -> [[f32; 6]; 3] {
        let c = color;
        [
            [-1.0, -1.0, c[0], c[1], c[2], c[3]],
            [3.0, -1.0, c[0], c[1], c[2], c[3]],
            [-1.0, 3.0, c[0], c[1], c[2], c[3]],
        ]
    }

    fn draw_and_read(be: &mut SoftwareBackend, clear: [f32; 4], count: u32) -> Vec<u8> {
        be.submit(&CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear, store: true }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                Enc::Draw { vertex_count: count, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
            ],
            signal: None,
        }).unwrap();
        let (w, h) = { let t = be.textures.get(1).unwrap(); (t.desc.width, t.desc.height) };
        let mut out = vec![0u8; (w * h * 4) as usize];
        be.read_texture(TextureId(1), &mut out).unwrap();
        out
    }

    #[test]
    fn software_draw_blends_semi_transparent_over_srgb_in_linear_light() {
        // Background sRGB(200,100,50,255); a 50%-alpha source sRGB(40,230,140) drawn over it. A correct
        // sRGB-aware compositor decodes BOTH to linear light, composites premultiplied source-over, and
        // re-encodes. That is byte-exactly (149,181,107,255) — and it is NOT the value a naive blend
        // done directly in sRGB space (120,165,95,255) would give.
        let src = [40.0 / 255.0, 230.0 / 255.0, 140.0 / 255.0, 0.5];
        let mut be = draw_harness(TextureFormat::Rgba8Srgb, Some(over_blend()), 2, 2, &fullscreen_tri(src));
        let px = draw_and_read(&mut be, [200.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0, 1.0], 3);
        for texel in px.chunks_exact(4) {
            assert_eq!(texel, [149, 181, 107, 255], "linear-light premultiplied source-over golden");
            assert_ne!(texel, [120, 165, 95, 255], "must NOT be the naive sRGB-space blend");
        }
    }

    #[test]
    fn software_draw_on_unorm_target_blends_in_the_byte_domain() {
        // The SAME numeric colors on a linear (Unorm) target: no transfer function is applied, so the
        // premultiplied source-over is done directly on the stored values → (120,165,95,255). This is
        // exactly the value the sRGB target must AVOID, proving the transfer functions are applied only
        // around the blend for sRGB formats.
        let src = [40.0 / 255.0, 230.0 / 255.0, 140.0 / 255.0, 0.5];
        let mut be = draw_harness(TextureFormat::Rgba8Unorm, Some(over_blend()), 2, 2, &fullscreen_tri(src));
        let px = draw_and_read(&mut be, [200.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0, 1.0], 3);
        for texel in px.chunks_exact(4) {
            assert_eq!(texel, [120, 165, 95, 255], "linear-target premultiplied over is byte-domain");
        }
    }

    #[test]
    fn software_draw_respects_bgra_channel_order() {
        // A Bgra8Srgb target stores blue and red swapped; the same linear-light blend result must land
        // in B,G,R,A byte order.
        let src = [40.0 / 255.0, 230.0 / 255.0, 140.0 / 255.0, 0.5];
        let mut be = draw_harness(TextureFormat::Bgra8Srgb, Some(over_blend()), 2, 2, &fullscreen_tri(src));
        let px = draw_and_read(&mut be, [200.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0, 1.0], 3);
        // Logical RGBA (149,181,107,255) stored BGRA → (107,181,149,255).
        for texel in px.chunks_exact(4) {
            assert_eq!(texel, [107, 181, 149, 255]);
        }
    }

    #[test]
    fn software_draw_opaque_replace_writes_the_straight_source() {
        // With blend disabled the draw is an opaque replace: the full straight source RGBA (including its
        // alpha, 0.5 → 128) is written directly, byte-for-byte, with no linear round-trip and no
        // compositing against the background.
        let src = [40.0 / 255.0, 230.0 / 255.0, 140.0 / 255.0, 0.5];
        let mut be = draw_harness(TextureFormat::Rgba8Srgb, None, 2, 2, &fullscreen_tri(src));
        let px = draw_and_read(&mut be, [200.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0, 1.0], 3);
        for texel in px.chunks_exact(4) {
            assert_eq!(texel, [40, 230, 140, 128], "opaque draw replaces with the straight source color");
        }
    }

    #[test]
    fn software_draw_rasterizes_only_covered_pixels() {
        // A triangle that maps to framebuffer vertices (0,0),(4,0),(0,4) covers the upper-left half of a
        // 4×4 target. The top-left pixel is blended; the bottom-right pixel keeps the cleared background —
        // proving the draw actually rasterizes a shape rather than filling the whole attachment.
        let src = [40.0 / 255.0, 230.0 / 255.0, 140.0 / 255.0, 0.5];
        let c = src;
        // NDC vertices that map to fb (0,0),(4,0),(0,4) on a 4×4 target.
        let verts = [
            [-1.0, 1.0, c[0], c[1], c[2], c[3]],
            [1.0, 1.0, c[0], c[1], c[2], c[3]],
            [-1.0, -1.0, c[0], c[1], c[2], c[3]],
        ];
        let mut be = draw_harness(TextureFormat::Rgba8Srgb, Some(over_blend()), 4, 4, &verts);
        let px = draw_and_read(&mut be, [200.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0, 1.0], 3);
        let at = |x: usize, y: usize| &px[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(at(0, 0), [149, 181, 107, 255], "covered pixel is blended in linear light");
        assert_eq!(at(3, 3), [200, 100, 50, 255], "uncovered pixel keeps the cleared background");
    }

    #[test]
    fn software_draw_quad_of_two_triangles_composites_each_pixel_once() {
        // A full quad drawn as TWO triangles sharing a diagonal must composite each pixel exactly once
        // (no double blend on the shared edge): the result equals the single-triangle full-screen blend.
        let c = [40.0 / 255.0, 230.0 / 255.0, 140.0 / 255.0, 0.5];
        // Two triangles covering the whole [-1,1]² quad, sharing the (-1,-1)-(1,1) diagonal.
        let verts = [
            [-1.0, -1.0, c[0], c[1], c[2], c[3]],
            [1.0, -1.0, c[0], c[1], c[2], c[3]],
            [1.0, 1.0, c[0], c[1], c[2], c[3]],
            [-1.0, -1.0, c[0], c[1], c[2], c[3]],
            [1.0, 1.0, c[0], c[1], c[2], c[3]],
            [-1.0, 1.0, c[0], c[1], c[2], c[3]],
        ];
        let mut be = draw_harness(TextureFormat::Rgba8Srgb, Some(over_blend()), 2, 2, &verts);
        let px = draw_and_read(&mut be, [200.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0, 1.0], 6);
        for texel in px.chunks_exact(4) {
            assert_eq!(texel, [149, 181, 107, 255], "each pixel blended exactly once (no diagonal double-blend)");
        }
    }

    // ---- instanced draws + base-vertex indexed draws (Enc::Draw/DrawIndexed instance/base fields) ----

    /// Submit a single non-indexed draw of `count` vertices with `instances` instances, then read the
    /// target. Isolates the `instance_count` path (glDrawArraysInstanced lowering).
    fn draw_instanced_and_read(be: &mut SoftwareBackend, clear: [f32; 4], count: u32, instances: u32) -> Vec<u8> {
        be.submit(&CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear, store: true }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                Enc::Draw { vertex_count: count, instance_count: instances, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
            ],
            signal: None,
        }).unwrap();
        let (w, h) = { let t = be.textures.get(1).unwrap(); (t.desc.width, t.desc.height) };
        let mut out = vec![0u8; (w * h * 4) as usize];
        be.read_texture(TextureId(1), &mut out).unwrap();
        out
    }

    #[test]
    fn software_instanced_draw_composites_once_per_instance() {
        // A 50%-alpha source-over triangle drawn with N instances must composite N times: each added
        // instance pulls the pixel further toward the pure source color. Collapsing instanced draws to a
        // single instance (the bug this closes) would make every instance count produce the identical pixel.
        let src = [40.0 / 255.0, 230.0 / 255.0, 140.0 / 255.0, 0.5];
        let clear = [200.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0, 1.0];
        let read = |instances: u32| {
            let mut be = draw_harness(TextureFormat::Rgba8Srgb, Some(over_blend()), 2, 2, &fullscreen_tri(src));
            draw_instanced_and_read(&mut be, clear, 3, instances)
        };
        let px1 = read(1);
        let px2 = read(2);
        let px3 = read(3);
        // Single instance is the established source-over golden.
        assert_eq!(&px1[0..4], [149, 181, 107, 255]);
        // Instancing is honored, not collapsed: more instances → strictly more source-saturated (red falls
        // toward 40, green rises toward 230).
        assert!(px2[0] < px1[0] && px3[0] < px2[0], "red falls toward source across instances: {} {} {}", px1[0], px2[0], px3[0]);
        assert!(px2[1] > px1[1] && px3[1] > px2[1], "green rises toward source across instances: {} {} {}", px1[1], px2[1], px3[1]);
        assert_ne!(&px3[0..4], &px1[0..4], "3 instances must not equal 1 instance (no collapse)");
    }

    #[test]
    fn software_base_vertex_offsets_indexed_vertex_fetch() {
        // Vertex buffer: verts 0..3 = a full-screen tri of color A, verts 3..6 = color B. An indexed draw
        // of indices [0,1,2] fetches color A at base_vertex 0 and color B at base_vertex 3 — proving
        // base_vertex is added to every fetched index (glDrawElementsBaseVertex) rather than dropped to 0.
        let a = [51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0, 1.0];
        let b = [204.0 / 255.0, 153.0 / 255.0, 102.0 / 255.0, 1.0];
        let mut verts: Vec<[f32; 6]> = Vec::new();
        verts.extend_from_slice(&fullscreen_tri(a));
        verts.extend_from_slice(&fullscreen_tri(b));
        // Opaque (no blend) so the fetched vertex color is written straight through.
        let mut be = draw_harness(TextureFormat::Rgba8Srgb, None, 2, 2, &verts);
        // Index buffer (buf 2): u16 [0, 1, 2].
        let idx: [u16; 3] = [0, 1, 2];
        let ibytes: Vec<u8> = idx.iter().flat_map(|i| i.to_le_bytes()).collect();
        be.create_buffer(BufferId(2), &BufferDesc {
            size: ibytes.len() as u64, usage: buffer_usage::INDEX | buffer_usage::COPY_DST, label: String::new(),
        }).unwrap();
        be.write_buffer(BufferId(2), 0, &ibytes).unwrap();

        let read_base = |be: &mut SoftwareBackend, base_vertex: i32| {
            be.submit(&CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetVertexBuffer { slot: 0, buffer: 1, offset: 0 },
                    Enc::SetIndexBuffer { buffer: 2, offset: 0, format: IndexFormat::U16 },
                    Enc::DrawIndexed { index_count: 3, instance_count: 1, first_index: 0, base_vertex, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }).unwrap();
            let mut out = vec![0u8; 2 * 2 * 4];
            be.read_texture(TextureId(1), &mut out).unwrap();
            out
        };

        let px0 = read_base(&mut be, 0);
        assert_eq!(&px0[0..4], [51, 102, 153, 255], "base_vertex 0 fetches vertices 0..3 (color A)");
        let px3 = read_base(&mut be, 3);
        assert_eq!(&px3[0..4], [204, 153, 102, 255], "base_vertex 3 fetches vertices 3..6 (color B)");
    }
}
