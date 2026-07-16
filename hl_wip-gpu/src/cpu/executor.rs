//! [`CpuExecutor`] — the reference [`GpuExecutor`], pure CPU with no platform deps. It is the semantic
//! **oracle**: it reproduces byte-for-byte the outputs the shipping `hl-gpu` `SoftwareBackend` produces,
//! so a real GPU executor (wgpu/Metal) is correct exactly when it matches this one on the conformance
//! suite.
//!
//! Ported from `SoftwareBackend` in `hl-gpu/src/software.rs`: its per-resource `create_*`/`destroy_*`,
//! `write_buffer`, `submit` (validate-then-execute), and `present`, collapsed onto the single batch
//! [`GpuExecutor::execute`] over the runtime-owned [`SessionResources`]. The one adaptation: the PTX text
//! front-end lives in the driver (`hl-cuda`), not here, so a `PtxKernel` shader consumes a **pre-compiled
//! [`KernelProgram`]** registered via [`CpuExecutor::define_kernel`] rather than parsing PTX at create time.
//!
//! [`SessionResources`]: crate::runtime::model::resources::SessionResources
//! [`KernelProgram`]: crate::protocol::model::kernel::KernelProgram

use std::collections::HashMap;

use hl_log::tag;

use crate::cpu::format::texel_bytes;
use crate::cpu::model::pipeline::Pipeline;
use crate::cpu::model::shader::ShaderModule;
use crate::cpu::model::{
    bind_group, buffer, buffer_mut, fence, fence_mut, surface, texture, BindGroupState, Buffer,
    GenRef, Sampler, Texture,
};
use crate::cpu::service::compute::run_dispatch;
use crate::cpu::service::copy;
use crate::cpu::service::raster;
use crate::protocol::model::capability::{
    command_bits, format_bits, shader_payload, Capabilities, PresentKind, ALL_COMMANDS,
    COLOR_FORMATS, DEPTH_FORMATS,
};
use crate::protocol::model::command::{Cmd, CommandBuffer, Enc, ShaderPayloadKind, WIRE_VERSION};
use crate::protocol::model::descriptor::{
    BindResource, ComputePipelineDesc, RenderPipelineDesc, TextureDesc,
};
use crate::protocol::model::enums::{
    buffer_usage, texture_usage, IndexFormat, LoadOp, TextureDim, TextureFormat,
};
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{BufferId, FenceId, SurfaceId, TextureId};
use crate::protocol::model::kernel::{KernelDescriptor, KernelProgram, SPIRV_MAGIC};
use crate::runtime::model::resources::SessionResources;
use crate::runtime::port::executor::{GpuExecutor, Presented};

/// The pure CPU reference executor. Holds no resources of its own (those live in the runtime-owned
/// [`SessionResources`]); it carries only the pre-compiled kernels a `PtxKernel` shader resolves to and a
/// couple of work counters a test can read.
#[derive(Default)]
pub struct CpuExecutor {
    /// Pre-compiled kernels keyed by the shader id a later `CreateShader { PtxKernel, .. }` uses. The PTX
    /// text parser is a driver concern (`hl-cuda`), so the compiled program can be injected here directly
    /// (the `define_kernel` convenience) for tests that hand-build a [`KernelProgram`].
    kernels: HashMap<u32, KernelProgram>,
    /// Optional kernel front-end: compiles a driver-forwarded [`KernelDescriptor`] (source text + entry +
    /// block dims, decoded from a `CreateShader` KERNEL payload) into a [`KernelProgram`]. Injected by the
    /// composition root so the PTX parser stays a driver concern (`hl-cuda`) and never links into this
    /// crate. When set, a real (non-empty) descriptor on the wire is compiled directly — no `define_kernel`.
    #[allow(clippy::type_complexity)]
    kernel_compiler: Option<Box<dyn Fn(&KernelDescriptor) -> Result<KernelProgram>>>,
    /// Count of dispatches/draws seen — lets a test confirm compute/draw work reached the executor.
    pub dispatches: u64,
    pub draws: u64,
}

impl CpuExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pre-compiled [`KernelProgram`] under the shader id a subsequent
    /// `CreateShader { kind: PtxKernel, id, .. }` will carry. Stands in for the driver's PTX→kernel-IR
    /// front-end, which is not part of this crate.
    pub fn define_kernel(&mut self, shader_id: u32, program: KernelProgram) {
        self.kernels.insert(shader_id, program);
    }

    /// Inject the kernel front-end that compiles a driver-forwarded [`KernelDescriptor`] (decoded from a
    /// `CreateShader` KERNEL payload) into a [`KernelProgram`]. With this set, a `PtxKernel` shader whose
    /// payload carries a real descriptor is compiled on the fly — the real driver flow needs no
    /// `define_kernel`. Keeping the parser out here preserves this crate's freedom from any PTX/CUDA code.
    pub fn set_kernel_compiler<F>(&mut self, compiler: F)
    where
        F: Fn(&KernelDescriptor) -> Result<KernelProgram> + 'static,
    {
        self.kernel_compiler = Some(Box::new(compiler));
    }

    /// Read `out.len()` bytes back from buffer `id` at `offset` — the readback path a conformance test
    /// asserts on. Operates over the runtime-owned resources (the executor stores natives there).
    pub fn read_buffer(
        &self,
        resources: &SessionResources,
        id: BufferId,
        offset: u64,
        out: &mut [u8],
    ) -> Result<()> {
        let b = buffer(resources, id.0)?;
        let off = offset as usize;
        let end = offset
            .checked_add(out.len() as u64)
            .filter(|e| *e <= b.data.len() as u64)
            .ok_or(GpuError::OutOfBounds)? as usize;
        out.copy_from_slice(&b.data[off..end]);
        Ok(())
    }

    /// Read the whole tight-packed level-0 pixel plane of texture `id` (exactly `out.len()` bytes).
    pub fn read_texture(
        &self,
        resources: &SessionResources,
        id: TextureId,
        out: &mut [u8],
    ) -> Result<()> {
        let t = texture(resources, id.0)?;
        if out.len() != t.pixels.len() {
            return Err(GpuError::OutOfBounds);
        }
        out.copy_from_slice(&t.pixels);
        Ok(())
    }

    // --- per-command handlers (ported from SoftwareBackend's create_*/write/submit/present) ---------

    fn create_texture(&self, res: &mut SessionResources, id: u32, desc: &TextureDesc) -> Result<()> {
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
        // A `Depth32Float` attachment is materialized as a tight-packed f32 depth plane (4 bytes/texel) so
        // the rasterizer can run the per-fragment depth test/write against it; the color helpers still
        // reject it (it is not a plain-color format). A `Depth24PlusStencil8` attachment is materialized as
        // an 8-byte/texel plane — `[depth: f32 le | stencil: u8 @ byte 4, bytes 5..8 zero]` — so the
        // rasterizer can run BOTH the depth test/write and the stencil test/op against it (see
        // `raster::raster_draw_depth`). The stored byte layout is an oracle-internal detail: only the color
        // target is ever read back and compared, so it need not match any hardware depth/stencil packing.
        // Other depth/stencil formats stay unsupported.
        let bpt = match desc.format {
            TextureFormat::Depth32Float => 4,
            TextureFormat::Depth24PlusStencil8 => 8,
            _ => texel_bytes(desc.format)?,
        };
        let n = bpt
            .checked_mul(desc.width as usize)
            .and_then(|v| v.checked_mul(desc.height as usize))
            .and_then(|v| v.checked_mul(desc.sample_count as usize))
            .ok_or(GpuError::OutOfBounds)?;
        res.textures.insert(id, Box::new(Texture { desc: desc.clone(), pixels: vec![0u8; n] }))
    }

    fn create_shader(
        &self,
        res: &mut SessionResources,
        id: u32,
        kind: ShaderPayloadKind,
        spirv: &[u32],
    ) -> Result<()> {
        if spirv.is_empty() {
            return Err(GpuError::Invalid("empty shader module"));
        }
        let module = match kind {
            ShaderPayloadKind::PtxKernel => {
                // The real driver flow serializes a kernel descriptor (source text + entry + block dims)
                // INTO this payload (`KernelDescriptor::to_words`). Decode it here and compile it via the
                // injected front-end (the PTX parser is a driver concern kept out of this crate). A
                // non-kernel / empty placeholder payload falls back to a pre-registered program — the
                // `define_kernel` convenience hand-built kernels use.
                let prog = match KernelDescriptor::from_words(spirv) {
                    // A real driver-produced descriptor (non-empty source): compile it on the fly.
                    Some(Ok(desc)) if !desc.ptx.is_empty() => {
                        let compiler = self.kernel_compiler.as_ref().ok_or(GpuError::Unsupported(
                            "cpu: PtxKernel payload needs a kernel compiler (set_kernel_compiler)",
                        ))?;
                        compiler(&desc)?
                    }
                    // A non-kernel / empty-placeholder / undecodable payload → the pre-registered
                    // program a test injected via `define_kernel`.
                    _ => self.kernels.get(&id).cloned().ok_or(GpuError::Unsupported(
                        "cpu: no compiled kernel registered for PtxKernel shader id",
                    ))?,
                };
                ShaderModule::Kernel(Box::new(prog))
            }
            ShaderPayloadKind::SpirV => {
                if spirv.first() != Some(&SPIRV_MAGIC) {
                    return Err(GpuError::Invalid("malformed SPIR-V shader payload"));
                }
                ShaderModule::Spirv
            }
            // GLSL / legacy-MSL / demo graphics payloads are opaque to the fixed-function CPU oracle: it
            // rasterizes from the pipeline + vertex data, not the shader source, so any graphics module is
            // an accepted opaque handle here (the real GLSL compile happens on the wgpu executor).
            ShaderPayloadKind::Glsl
            | ShaderPayloadKind::LegacyMsl
            | ShaderPayloadKind::DemoBuiltin => ShaderModule::Spirv,
        };
        res.shaders.insert(id, Box::new(module))
    }

    fn create_render_pipeline(
        &self,
        res: &mut SessionResources,
        id: u32,
        desc: &RenderPipelineDesc,
    ) -> Result<()> {
        res.shaders.get(desc.vertex.module)?;
        if let Some(f) = &desc.fragment {
            res.shaders.get(f.module)?;
        }
        for vb in &desc.vertex_buffers {
            for a in &vb.attrs {
                if vb.stride == 0 || a.offset >= vb.stride {
                    return Err(GpuError::Invalid("vertex attribute offset outside stride"));
                }
            }
        }
        res.pipelines.insert(
            id,
            Box::new(Pipeline::Render {
                color_formats: desc.color_targets.iter().map(|c| c.format).collect(),
                vertex_layouts: desc.vertex_buffers.clone(),
                topology: desc.topology,
                blends: desc.color_targets.iter().map(|c| c.blend.clone()).collect(),
                write_masks: desc.color_targets.iter().map(|c| c.write_mask).collect(),
                cull: desc.cull,
                front_face: desc.front_face,
                depth: desc.depth.clone(),
            }),
        )
    }

    fn create_compute_pipeline(
        &self,
        res: &mut SessionResources,
        id: u32,
        desc: &ComputePipelineDesc,
    ) -> Result<()> {
        res.shaders.get(desc.compute.module)?;
        res.pipelines.insert(id, Box::new(Pipeline::Compute { shader: desc.compute.module }))
    }

    fn create_bind_group(
        &self,
        res: &mut SessionResources,
        id: u32,
        desc: &crate::protocol::model::descriptor::BindGroupDesc,
    ) -> Result<()> {
        let mut buffers = Vec::new();
        let mut textures = Vec::new();
        let mut samplers = Vec::new();
        for e in &desc.entries {
            match &e.resource {
                BindResource::Buffer { id, offset, size } => {
                    let b = buffer(res, *id)?;
                    copy::buffer_slice_bounds(b.data.len(), *offset, *size)?;
                    buffers.push(GenRef { id: *id, gen: res.buffers.generation(*id).unwrap() });
                }
                BindResource::Texture { id } => {
                    texture_with_usage(
                        res,
                        *id,
                        texture_usage::SAMPLED,
                        "texture bound without SAMPLED usage",
                    )?;
                    textures.push(GenRef { id: *id, gen: res.textures.generation(*id).unwrap() });
                }
                BindResource::Sampler { id } => {
                    res.samplers.get(*id)?;
                    samplers.push(GenRef { id: *id, gen: res.samplers.generation(*id).unwrap() });
                }
            }
        }
        res.bind_groups.insert(id, Box::new(BindGroupState { desc: desc.clone(), buffers, textures, samplers }))
    }

    fn write_buffer(&self, res: &mut SessionResources, id: u32, offset: u64, data: &[u8]) -> Result<()> {
        let b = buffer_mut(res, id)?;
        let off = offset as usize;
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|e| *e <= b.data.len() as u64)
            .ok_or(GpuError::OutOfBounds)? as usize;
        b.data[off..end].copy_from_slice(data);
        Ok(())
    }

    /// Execute a `FillBuffer`: write the repeating little-endian 4-byte pattern of `value` over
    /// `[offset, offset+size)`. A device-side memset — the pattern tiles from `offset` (byte `i` of the
    /// region takes pattern byte `i % 4`), so a `size` that is not a multiple of 4 fills a partial pattern
    /// at the tail. Bounds are re-checked here (submit-time validation already verified them).
    fn fill_buffer(&self, res: &mut SessionResources, id: u32, offset: u64, size: u64, value: u32) -> Result<()> {
        let b = buffer_mut(res, id)?;
        let start = offset as usize;
        let end = offset
            .checked_add(size)
            .filter(|e| *e <= b.data.len() as u64)
            .ok_or(GpuError::OutOfBounds)? as usize;
        let pat = value.to_le_bytes();
        for (i, byte) in b.data[start..end].iter_mut().enumerate() {
            *byte = pat[i % 4];
        }
        Ok(())
    }

    fn present(&self, res: &SessionResources, surface_id: u32, texture_id: u32) -> Result<Presented> {
        let sdesc = surface(res, surface_id)?.clone();
        let t = texture(res, texture_id)?;
        if t.desc.sample_count != 1 {
            return Err(GpuError::Unsupported("software: present multisample texture"));
        }
        if t.desc.width != sdesc.width || t.desc.height != sdesc.height {
            return Err(GpuError::Invalid("present texture size does not match surface"));
        }
        Ok(Presented { surface: SurfaceId(surface_id), texture: TextureId(texture_id) })
    }

    /// Validate the whole command buffer (failure atomicity), then execute its clears/copies/draws/
    /// dispatches. Ported from `SoftwareBackend::submit`.
    fn submit(&mut self, res: &mut SessionResources, cb: &CommandBuffer) -> Result<()> {
        let _span = hl_log::hl_span!(tag::CPU, "submit");
        hl_log::hl_count!(tag::CPU, "submits");
        validate_cb(res, cb)?;

        let mut cur_pipeline: Option<u32> = None;
        let mut cur_bind_group: Option<u32> = None;
        let mut cur_targets: Vec<(u32, TextureFormat)> = Vec::new();
        let mut cur_depth: Option<u32> = None;
        let mut cur_vertex: HashMap<u32, (u32, u64)> = HashMap::new();
        let mut cur_index: Option<(u32, u64, IndexFormat)> = None;
        // The dynamic stencil reference value (WebGPU `setStencilReference`). It resets to 0 per render pass
        // (mirroring the wgpu executor, whose reference is pass-scoped state) and the stream's
        // `SetStencilReference` ops update it for the draws that follow. The pipeline's `DepthState` carries
        // the static stencil compare/ops/masks; this is the one dynamic operand the compare tests against
        // and a `REPLACE` op writes.
        let mut cur_stencil_ref: u32 = 0;
        for op in &cb.encoder {
            match op {
                Enc::BeginRenderPass { color, depth } => {
                    cur_targets.clear();
                    cur_stencil_ref = 0;
                    for c in color {
                        let fmt = texture(res, c.texture)?.desc.format;
                        cur_targets.push((c.texture, fmt));
                        if c.load == LoadOp::Clear {
                            raster::clear_target(res, c.texture, c.clear)?;
                        }
                    }
                    cur_depth = None;
                    if let Some(dp) = depth {
                        cur_depth = Some(dp.texture);
                        if dp.load == LoadOp::Clear {
                            // Clears both the depth plane (to `clear_depth`) and, for a combined
                            // depth+stencil attachment, the stencil plane (to `clear_stencil`); a
                            // `LoadOp::Load` preserves both, which a two-pass mark-then-test IR relies on.
                            raster::clear_depth_stencil_target(
                                res,
                                dp.texture,
                                dp.clear_depth,
                                dp.clear_stencil,
                            )?;
                        }
                    }
                }
                Enc::EndRenderPass => {
                    cur_targets.clear();
                    cur_depth = None;
                }
                Enc::ClearRect { texture, x, y, w, h, color } => {
                    raster::clear_rect(res, *texture, *x, *y, *w, *h, *color)?;
                }
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetStencilReference { reference } => cur_stencil_ref = *reference,
                Enc::SetBindGroup { group, .. } => cur_bind_group = Some(*group),
                Enc::SetVertexBuffer { slot, buffer, offset } => {
                    cur_vertex.insert(*slot, (*buffer, *offset));
                }
                Enc::SetIndexBuffer { buffer, offset, format } => {
                    cur_index = Some((*buffer, *offset, *format));
                }
                Enc::Draw { vertex_count, first_vertex, instance_count, .. } => {
                    self.draws += 1;
                    hl_log::hl_count!(tag::CPU, "draws");
                    let vb = cur_vertex.get(&0).copied();
                    raster::exec_draw(
                        res, cur_pipeline, &cur_targets, cur_depth, vb, *first_vertex, *vertex_count,
                        *instance_count, cur_stencil_ref,
                    )?;
                }
                Enc::DrawIndexed { index_count, first_index, base_vertex, instance_count, .. } => {
                    self.draws += 1;
                    hl_log::hl_count!(tag::CPU, "draws");
                    let vb = cur_vertex.get(&0).copied();
                    raster::exec_draw_indexed(
                        res, cur_pipeline, &cur_targets, cur_depth, vb, cur_index, *first_index,
                        *index_count, *base_vertex, *instance_count, cur_stencil_ref,
                    )?;
                }
                Enc::Dispatch { x, y, z } => {
                    self.dispatches += 1;
                    hl_log::hl_count!(tag::CPU, "dispatches");
                    run_dispatch(res, cur_pipeline, cur_bind_group, (*x, *y, *z))?;
                }
                Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size } => {
                    copy::copy_buffer_to_buffer(res, *src, *src_offset, *dst, *dst_offset, *size)?;
                }
                Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, width, height, .. } => {
                    copy::copy_buffer_to_texture(
                        res, *src, *src_offset, *bytes_per_row, *dst, *width, *height,
                    )?;
                }
                Enc::CopyTextureToBuffer { src, width, height, dst, dst_offset, bytes_per_row, .. } => {
                    copy::copy_texture_to_buffer(
                        res, *src, *width, *height, *dst, *dst_offset, *bytes_per_row,
                    )?;
                }
                Enc::CopyTextureToTexture { src, src_origin, dst, dst_origin, extent, .. } => {
                    copy::copy_texture_to_texture(res, *src, src_origin, *dst, dst_origin, extent)?;
                }
                Enc::BlitTexture {
                    src, src_origin, src_extent, dst, dst_origin, dst_extent, filter, ..
                } => {
                    copy::blit_texture(
                        res, *src, src_origin, src_extent, *dst, dst_origin, dst_extent, *filter,
                    )?;
                }
                Enc::ResolveTexture { src, src_origin, dst, dst_origin, extent, .. } => {
                    copy::resolve_texture(res, *src, src_origin, *dst, dst_origin, extent)?;
                }
                Enc::FillBuffer { buffer, offset, size, value } => {
                    self.fill_buffer(res, *buffer, *offset, *size, *value)?;
                }
                _ => {}
            }
        }
        if let Some((f, v)) = cb.signal {
            let slot = fence_mut(res, f)?;
            *slot = (*slot).max(v);
        }
        Ok(())
    }
}

impl GpuExecutor for CpuExecutor {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "hl-cpu".into(),
            unified_memory: true,
            supports_compute: true,
            supports_graphics: true,
            max_texture_2d: 8192,
            present_kinds: vec![PresentKind::Shm],
            wire_version: WIRE_VERSION,
            command_bits: command_bits(ALL_COMMANDS),
            // Executes compiled kernels; it cannot run a graphics (SPIR-V/MSL) shader.
            shader_payloads: shader_payload::KERNEL,
            // Color formats plus the depth formats the oracle materializes for depth-tested rendering.
            texture_formats: format_bits(COLOR_FORMATS) | format_bits(DEPTH_FORMATS),
            // Browser-class per-frame wire-byte ceiling (256 MiB): a hostile-DoS guard, not a correctness
            // bound — the `GlobalLedger` is the true host-OOM guard. Raised from 64 MiB, which tripped
            // healthy browser frames. (Matches the wgpu executor; see its rationale.)
            max_frame_bytes: 256 << 20,
            max_buffer_bytes: 256 << 20,
            max_bind_groups: 4,
            // Synchronous; a fence only reaches a value a submit signalled.
            supports_timeline_fences: false,
        }
    }

    fn execute(&mut self, res: &mut SessionResources, batch: &[Cmd]) -> Result<Vec<Presented>> {
        let _span = hl_log::hl_span!(tag::CPU, "dispatch");
        hl_log::hl_debug!(tag::CPU, "execute cmds={}", batch.len());
        let mut presents = Vec::new();
        for cmd in batch {
            match cmd {
                Cmd::CreateBuffer(id, d) => res
                    .buffers
                    .insert(*id, Box::new(Buffer { data: vec![0u8; d.size as usize], usage: d.usage }))?,
                Cmd::DestroyBuffer(id) => {
                    res.buffers.remove(*id)?;
                }
                Cmd::WriteBuffer { id, offset, data } => self.write_buffer(res, *id, *offset, data)?,
                Cmd::CreateTexture(id, d) => self.create_texture(res, *id, d)?,
                Cmd::DestroyTexture(id) => {
                    res.textures.remove(*id)?;
                }
                Cmd::CreateSampler(id, _) => res.samplers.insert(*id, Box::new(Sampler))?,
                Cmd::DestroySampler(id) => {
                    res.samplers.remove(*id)?;
                }
                Cmd::CreateShader { id, kind, spirv } => self.create_shader(res, *id, *kind, spirv)?,
                Cmd::DestroyShader(id) => {
                    res.shaders.remove(*id)?;
                }
                Cmd::CreateRenderPipeline(id, d) => self.create_render_pipeline(res, *id, d)?,
                Cmd::CreateComputePipeline(id, d) => self.create_compute_pipeline(res, *id, d)?,
                Cmd::DestroyPipeline(id) => {
                    res.pipelines.remove(*id)?;
                }
                Cmd::CreateBindGroup(id, d) => self.create_bind_group(res, *id, d)?,
                Cmd::DestroyBindGroup(id) => {
                    res.bind_groups.remove(*id)?;
                }
                Cmd::CreateSurface(id, d) => res.surfaces.insert(*id, Box::new(d.clone()))?,
                Cmd::DestroySurface(id) => {
                    res.surfaces.remove(*id)?;
                }
                Cmd::CreateFence(id) => res.fences.insert(*id, Box::new(0u64))?,
                Cmd::DestroyFence(id) => {
                    res.fences.remove(*id)?;
                }
                Cmd::Submit(cb) => self.submit(res, cb)?,
                Cmd::WaitFence { id, value } => {
                    let v = *fence(res, *id)?;
                    if v < *value {
                        return Err(GpuError::Invalid("wait on a fence value that was never signalled"));
                    }
                }
                Cmd::Present { surface, texture } => {
                    presents.push(self.present(res, *surface, *texture)?);
                }
            }
        }
        Ok(presents)
    }

    fn wait(&mut self, resources: &mut SessionResources, fence_id: FenceId, value: u64) -> Result<()> {
        let v = *fence(resources, fence_id.0)?;
        if v < value {
            return Err(GpuError::Invalid("wait on a fence value that was never signalled"));
        }
        Ok(())
    }

    /// Serve the device→host readback port by allocating the output and delegating to the inherent
    /// [`read_buffer`](CpuExecutor::read_buffer) over the runtime-owned resources.
    fn read_buffer(
        &self,
        resources: &SessionResources,
        id: BufferId,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        CpuExecutor::read_buffer(self, resources, id, offset, &mut out)?;
        Ok(out)
    }
}

// ===================================================================================================
// submit-time command-buffer validation (ported from SoftwareBackend::validate_cb / validate_op)
// ===================================================================================================

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

fn buffer_with_usage<'a>(
    res: &'a SessionResources,
    id: u32,
    usage: u32,
    what: &'static str,
) -> Result<&'a Buffer> {
    let b = buffer(res, id)?;
    if b.usage & usage == 0 {
        return Err(GpuError::Invalid(what));
    }
    Ok(b)
}

fn texture_with_usage<'a>(
    res: &'a SessionResources,
    id: u32,
    usage: u32,
    what: &'static str,
) -> Result<&'a Texture> {
    let t = texture(res, id)?;
    if t.desc.usage & usage == 0 {
        return Err(GpuError::Invalid(what));
    }
    Ok(t)
}

/// Re-check that every resource a bind group referenced is still live *and* still the same allocation it
/// was bound against (generation match).
fn check_bind_group_live<'a>(res: &'a SessionResources, bgid: u32) -> Result<&'a BindGroupState> {
    let bg = bind_group(res, bgid)?;
    for r in &bg.buffers {
        if res.buffers.generation(r.id) != Some(r.gen) {
            return Err(GpuError::UnknownId { kind: BufferId::KIND, id: r.id });
        }
    }
    for r in &bg.textures {
        if res.textures.generation(r.id) != Some(r.gen) {
            return Err(GpuError::UnknownId { kind: TextureId::KIND, id: r.id });
        }
    }
    for r in &bg.samplers {
        if res.samplers.generation(r.id) != Some(r.gen) {
            return Err(GpuError::UnknownId { kind: crate::protocol::model::id::SamplerId::KIND, id: r.id });
        }
    }
    Ok(bg)
}

fn check_vertex_range(
    res: &SessionResources,
    buffer_id: u32,
    offset: u64,
    stride: u32,
    count: Option<u32>,
) -> Result<()> {
    let b = buffer(res, buffer_id)?;
    let count = count.ok_or(GpuError::OutOfBounds)?;
    let need = (count as u64).checked_mul(stride as u64).ok_or(GpuError::OutOfBounds)?;
    let end = offset.checked_add(need).ok_or(GpuError::OutOfBounds)?;
    if end > b.data.len() as u64 {
        return Err(GpuError::OutOfBounds);
    }
    Ok(())
}

fn validate_draw<F>(res: &SessionResources, st: &EncoderState, mut per_layout: F) -> Result<()>
where
    F: FnMut(&crate::protocol::model::descriptor::VertexLayout, u32) -> Result<()>,
{
    if !st.in_render_pass {
        return Err(GpuError::Invalid("draw outside a render pass"));
    }
    let pid = st.pipeline.ok_or(GpuError::Invalid("draw with no pipeline bound"))?;
    let (color_formats, vertex_layouts) = match crate::cpu::model::pipeline(res, pid)? {
        Pipeline::Render { color_formats, vertex_layouts, .. } => {
            (color_formats.clone(), vertex_layouts.clone())
        }
        Pipeline::Compute { .. } => return Err(GpuError::Unsupported("draw on a compute pipeline")),
    };
    if color_formats.len() != st.color_formats.len()
        || color_formats.iter().zip(&st.color_formats).any(|(a, b)| a != b)
    {
        return Err(GpuError::Invalid("pipeline color format mismatches render attachment"));
    }
    for (slot, layout) in vertex_layouts.iter().enumerate() {
        per_layout(layout, slot as u32)?;
    }
    if let Some(bg) = st.bind_group {
        let bg = check_bind_group_live(res, bg)?;
        for r in &bg.textures {
            if st.color_targets.contains(&r.id) {
                return Err(GpuError::Invalid("texture sampled while bound as a color attachment"));
            }
        }
    }
    Ok(())
}

fn validate_cb(res: &SessionResources, cb: &CommandBuffer) -> Result<()> {
    let mut st = EncoderState::default();
    for op in &cb.encoder {
        validate_op(res, op, &mut st)?;
    }
    if st.in_render_pass || st.in_compute_pass {
        return Err(GpuError::Invalid("command buffer ends inside an open pass"));
    }
    Ok(())
}

fn validate_op(res: &SessionResources, op: &Enc, st: &mut EncoderState) -> Result<()> {
    use crate::cpu::format::clear_texel;
    match op {
        Enc::BeginRenderPass { color, depth } => {
            if st.in_render_pass || st.in_compute_pass {
                return Err(GpuError::Invalid("nested render pass"));
            }
            let mut formats = Vec::with_capacity(color.len());
            for c in color {
                let t = texture_with_usage(
                    res,
                    c.texture,
                    texture_usage::RENDER_TARGET,
                    "color attachment lacks RENDER_TARGET usage",
                )?;
                if t.desc.sample_count != 1 {
                    return Err(GpuError::Unsupported("software: multisample render attachment"));
                }
                if c.load == LoadOp::Clear {
                    clear_texel(t.desc.format, c.clear)?;
                }
                formats.push(t.desc.format);
            }
            if let Some(dp) = depth {
                let t = texture_with_usage(
                    res,
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
            res.pipelines.get(*p)?;
            st.pipeline = Some(*p);
        }
        Enc::SetBindGroup { group, .. } => {
            res.bind_groups.get(*group)?;
            st.bind_group = Some(*group);
        }
        Enc::SetVertexBuffer { slot, buffer, offset } => {
            res.buffers.get(*buffer)?;
            st.vertex_buffers.insert(*slot, (*buffer, *offset));
        }
        Enc::SetIndexBuffer { buffer, offset, format } => {
            res.buffers.get(*buffer)?;
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
        // Dynamic stencil reference: pure dynamic pass state (the value the compare tests against and a
        // `REPLACE` op writes). Like `SetViewport`/`SetScissor` it carries no validation obligation — the
        // reference is any `u32`; the executor applies it to the draws that follow (see `submit`).
        Enc::SetStencilReference { .. } => {}
        Enc::ClearRect { texture, .. } => {
            let t = crate::cpu::model::texture(res, *texture)?;
            if t.desc.sample_count != 1 {
                return Err(GpuError::Unsupported("software: multisample clear"));
            }
        }
        Enc::Draw { vertex_count, instance_count, first_vertex, first_instance } => {
            validate_draw(res, st, |layout, slot| {
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
                check_vertex_range(res, buffer, offset, layout.stride, count)
            })?;
        }
        Enc::DrawIndexed { index_count, first_index, .. } => {
            validate_draw(res, st, |_layout, slot| {
                st.vertex_buffers.get(&slot).map(|_| ()).ok_or(GpuError::Invalid(
                    "indexed draw with no vertex buffer bound for a layout slot",
                ))
            })?;
            let (buffer, offset, format) =
                st.index_buffer.ok_or(GpuError::Invalid("indexed draw with no index buffer bound"))?;
            let isz = match format {
                IndexFormat::U16 => 2usize,
                IndexFormat::U32 => 4usize,
            };
            let last = first_index.checked_add(*index_count).ok_or(GpuError::OutOfBounds)?;
            let need = (last as usize).checked_mul(isz).ok_or(GpuError::OutOfBounds)?;
            let b = crate::cpu::model::buffer(res, buffer)?;
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
                Some(p) => match crate::cpu::model::pipeline(res, p)? {
                    Pipeline::Compute { .. } => {}
                    Pipeline::Render { .. } => {
                        return Err(GpuError::Unsupported("dispatch on a render pipeline"))
                    }
                },
                None => return Err(GpuError::Invalid("Dispatch with no pipeline bound")),
            }
            if let Some(bg) = st.bind_group {
                check_bind_group_live(res, bg)?;
            }
        }
        Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size } => {
            let s = buffer_with_usage(res, *src, buffer_usage::COPY_SRC, "copy src lacks COPY_SRC")?;
            copy::check_range(s.data.len(), *src_offset, *size)?;
            let d = buffer_with_usage(res, *dst, buffer_usage::COPY_DST, "copy dst lacks COPY_DST")?;
            copy::check_range(d.data.len(), *dst_offset, *size)?;
        }
        Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, mip, width, height } => {
            if *mip != 0 {
                return Err(GpuError::Unsupported("software: non-zero mip copy"));
            }
            let s_len = buffer_with_usage(res, *src, buffer_usage::COPY_SRC, "copy src lacks COPY_SRC")?
                .data
                .len();
            let t = texture_with_usage(res, *dst, texture_usage::COPY_DST, "copy dst lacks COPY_DST")?;
            if t.desc.sample_count != 1 {
                return Err(GpuError::Unsupported("software: buffer copy to multisample texture"));
            }
            let (_, _, src_span) = copy::texture_copy_layout(t, *width, *height, *bytes_per_row)?;
            copy::check_len(s_len, *src_offset, src_span)?;
        }
        Enc::CopyTextureToBuffer { src, mip, width, height, dst, dst_offset, bytes_per_row } => {
            if *mip != 0 {
                return Err(GpuError::Unsupported("software: non-zero mip copy"));
            }
            let t = texture_with_usage(res, *src, texture_usage::COPY_SRC, "copy src lacks COPY_SRC")?;
            if t.desc.sample_count != 1 {
                return Err(GpuError::Unsupported("software: multisample texture readback copy"));
            }
            let bpt = texel_bytes(t.desc.format)?;
            if *dst_offset % bpt as u64 != 0 {
                return Err(GpuError::Invalid("texture readback offset not texel-aligned"));
            }
            let (_, _, dst_span) = copy::texture_copy_layout(t, *width, *height, *bytes_per_row)?;
            let d_len =
                buffer_with_usage(res, *dst, buffer_usage::COPY_DST, "copy dst lacks COPY_DST")?
                    .data
                    .len();
            copy::check_len(d_len, *dst_offset, dst_span)?;
        }
        Enc::CopyTextureToTexture { src, src_sub, src_origin, dst, dst_sub, dst_origin, extent } => {
            copy::check_copy_subresource(src_sub, src_origin, extent.depth)?;
            copy::check_copy_subresource(dst_sub, dst_origin, extent.depth)?;
            let s = texture_with_usage(res, *src, texture_usage::COPY_SRC, "copy src lacks COPY_SRC")?;
            let s_fmt = s.desc.format;
            let s_samples = s.desc.sample_count;
            let d = texture_with_usage(res, *dst, texture_usage::COPY_DST, "copy dst lacks COPY_DST")?;
            if s_samples != 1 || d.desc.sample_count != 1 {
                return Err(GpuError::Unsupported("software: multisample texture copy"));
            }
            if texel_bytes(s_fmt)? != texel_bytes(d.desc.format)? {
                return Err(GpuError::Invalid("texture copy between incompatible texel sizes"));
            }
            let s = texture(res, *src)?;
            copy::check_region_in_texture(s, src_origin, extent)?;
            let d = texture(res, *dst)?;
            copy::check_region_in_texture(d, dst_origin, extent)?;
        }
        Enc::BlitTexture {
            src, src_sub, src_origin, src_extent, dst, dst_sub, dst_origin, dst_extent, ..
        } => {
            copy::check_copy_subresource(src_sub, src_origin, src_extent.depth)?;
            copy::check_copy_subresource(dst_sub, dst_origin, dst_extent.depth)?;
            if src_extent.width == 0
                || src_extent.height == 0
                || dst_extent.width == 0
                || dst_extent.height == 0
            {
                return Err(GpuError::Invalid("blit with a zero-sized region"));
            }
            let s = texture_with_usage(res, *src, texture_usage::COPY_SRC, "blit src lacks COPY_SRC")?;
            let s_fmt = s.desc.format;
            let s_samples = s.desc.sample_count;
            let d = texture_with_usage(res, *dst, texture_usage::COPY_DST, "blit dst lacks COPY_DST")?;
            if s_samples != 1 || d.desc.sample_count != 1 {
                return Err(GpuError::Unsupported("software: multisample blit"));
            }
            if texel_bytes(s_fmt)? != texel_bytes(d.desc.format)? {
                return Err(GpuError::Invalid("blit between incompatible texel sizes"));
            }
            let s = texture(res, *src)?;
            copy::check_region_in_texture(s, src_origin, src_extent)?;
            let d = texture(res, *dst)?;
            copy::check_region_in_texture(d, dst_origin, dst_extent)?;
        }
        Enc::ResolveTexture { src, src_sub, src_origin, dst, dst_sub, dst_origin, extent } => {
            copy::check_copy_subresource(src_sub, src_origin, extent.depth)?;
            copy::check_copy_subresource(dst_sub, dst_origin, extent.depth)?;
            let s = texture_with_usage(res, *src, texture_usage::COPY_SRC, "resolve src lacks COPY_SRC")?;
            let s_fmt = s.desc.format;
            let s_samples = s.desc.sample_count;
            let d = texture_with_usage(res, *dst, texture_usage::COPY_DST, "resolve dst lacks COPY_DST")?;
            if s_samples <= 1 || d.desc.sample_count != 1 {
                return Err(GpuError::Invalid("resolve sample counts"));
            }
            if s_fmt != d.desc.format {
                return Err(GpuError::Invalid("resolve formats differ"));
            }
            let s = texture(res, *src)?;
            copy::check_region_in_texture(s, src_origin, extent)?;
            let d = texture(res, *dst)?;
            copy::check_region_in_texture(d, dst_origin, extent)?;
        }
        Enc::FillBuffer { buffer, offset, size, .. } => {
            let b = buffer_with_usage(res, *buffer, buffer_usage::COPY_DST, "fill dst lacks COPY_DST")?;
            copy::check_range(b.data.len(), *offset, *size)?;
        }
    }
    Ok(())
}
