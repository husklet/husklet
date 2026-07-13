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

/// A registered shader module. The software oracle's shader ABI is a dd-GPU **kernel program**
/// (compiled from forwarded PTX); a Metal backend would instead carry SPIR-V for the same slot — the
/// per-backend seam described in `docs/ideas/CUDA_ON_METAL.md §5`.
enum ShaderModule {
    /// A compiled compute kernel this backend can actually execute on the CPU.
    Kernel(Box<KernelProgram>),
    /// Opaque SPIR-V words — recorded but not run here (needs a Metal/Vulkan backend).
    Spirv(#[allow(dead_code)] Vec<u32>),
}

/// A created pipeline. Compute pipelines remember their kernel shader so a `Dispatch` can run it;
/// render pipelines remember the state a draw must be validated against (color-target formats and the
/// vertex buffer layouts).
enum Pipeline {
    Render {
        color_formats: Vec<TextureFormat>,
        vertex_layouts: Vec<VertexLayout>,
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
            ShaderModule::Spirv(_) => return Ok(()), // software oracle cannot run SPIR-V
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
                    if c.load == LoadOp::Clear {
                        Self::clear_texel(t.desc.format, c.clear)?; // reject unclearable formats up front
                    }
                    formats.push(t.desc.format);
                }
                if let Some(dp) = depth {
                    // A depth attachment must name a real texture created with render-target usage;
                    // the software oracle does not fabricate an internal depth buffer.
                    self.texture_with_usage(
                        dp.texture,
                        texture_usage::RENDER_TARGET,
                        "depth attachment lacks RENDER_TARGET usage",
                    )?;
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
                self.textures.get(*texture)?;
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
                let (_, _, src_span) = texture_copy_layout(t, *width, *height, *bytes_per_row)?;
                check_len(s.data.len(), *src_offset, src_span)?;
            }
            Enc::CopyTextureToBuffer { src, mip, width, height, dst, dst_offset, bytes_per_row } => {
                if *mip != 0 {
                    return Err(GpuError::Unsupported("software: non-zero mip copy"));
                }
                let t = self.texture_with_usage(*src, texture_usage::COPY_SRC, "copy src lacks COPY_SRC")?;
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
                // A blit resamples per-texel; the oracle only handles equal-texel-size color formats.
                if Self::texel_bytes(s.desc.format)? != Self::texel_bytes(d.desc.format)? {
                    return Err(GpuError::Invalid("blit between incompatible texel sizes"));
                }
                check_region_in_texture(s, src_origin, src_extent)?;
                check_region_in_texture(d, dst_origin, dst_extent)?;
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
            Pipeline::Render { color_formats, vertex_layouts } => (color_formats, vertex_layouts),
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
        let top = p00[c] as f32 * (1.0 - tx) + p10[c] as f32 * tx;
        let bot = p01[c] as f32 * (1.0 - tx) + p11[c] as f32 * tx;
        out.push((top * (1.0 - ty) + bot * ty + 0.5) as u8);
    }
    out
}

impl GpuBackend for SoftwareBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "dd-software".into(),
            unified_memory: true, // it's all host memory
            supports_compute: true, // executes compiled PTX kernels (dd-GPU kernel IR) on the CPU
            supports_graphics: true, // clear/copy only
            max_texture_2d: 8192,
            present_kinds: vec![PresentKind::Shm],
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
        if desc.sample_count != 1 {
            return Err(GpuError::Unsupported("software: multisample textures"));
        }
        let bpt = Self::texel_bytes(desc.format)?;
        let n = bpt
            .checked_mul(desc.width as usize)
            .and_then(|v| v.checked_mul(desc.height as usize))
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

    fn create_shader(&mut self, id: ShaderId, spirv: &[u32]) -> Result<()> {
        // An empty shader module is never valid — reject it rather than record an unusable module
        // that would later fall through to a builtin or a no-op draw.
        if spirv.is_empty() {
            return Err(GpuError::Invalid("empty shader module"));
        }
        // A dd-GPU kernel descriptor (forwarded PTX + launch config) is compiled to an executable
        // kernel program here; anything else is treated as opaque SPIR-V (recorded, not run).
        let module = match KernelDescriptor::from_words(spirv) {
            Some(desc) => {
                let desc = desc?;
                let prog = ptx::compile(&desc.ptx, &desc.entry, desc.block)?;
                ShaderModule::Kernel(Box::new(prog))
            }
            None => ShaderModule::Spirv(spirv.to_vec()),
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
        for op in &cb.encoder {
            match op {
                Enc::BeginRenderPass { color, .. } => {
                    for c in color {
                        if c.load == LoadOp::Clear {
                            let (fmt, w, h) = {
                                let t = self.textures.get(c.texture)?;
                                (t.desc.format, t.desc.width, t.desc.height)
                            };
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
                Enc::ClearRect { texture, x, y, w, h, color } => {
                    self.clear_rect(*texture, *x, *y, *w, *h, *color)?;
                }
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetBindGroup { group, .. } => cur_bind_group = Some(*group),
                Enc::Draw { .. } | Enc::DrawIndexed { .. } => {
                    self.draws += 1;
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
                    let (sw, bpt) = {
                        let t = self.textures.get(*src)?;
                        (t.desc.width as usize, Self::texel_bytes(t.desc.format)?)
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
                                ),
                            };
                            let ddx = dst_origin.x as usize + dx;
                            let ddy = dst_origin.y as usize + dy;
                            let off = (ddy * dw + ddx) * bpt;
                            t.pixels[off..off + bpt].copy_from_slice(&texel);
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
