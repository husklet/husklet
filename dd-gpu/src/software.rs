//! A real (if minimal) CPU executor — the standing correctness fallback the architecture mandates
//! (llvmpipe/lavapipe fills this role on a real host; here it's a hand-rolled analog).
//!
//! It materializes buffers and textures in plain host memory and actually *executes* the parts an ML /
//! headless smoke test needs end-to-end with no GPU: buffer write/readback (CUDA H2D/D2H), render-pass
//! **clear** into a color target, and buffer↔texture / buffer↔buffer copies. Draw/Dispatch are recorded
//! but not rasterized/run (that needs a SPIR-V interpreter — out of scope). Enough to prove the whole
//! IR→wire→replay→execute→readback chain works headless on this Linux host.

use crate::backend::{Capabilities, GpuBackend, PresentKind, PresentToken};
use crate::id::*;
use crate::ir::*;
use crate::ptx::{self, KernelDescriptor, KernelProgram};
use crate::{GpuError, Result};

struct Buffer {
    data: Vec<u8>,
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

/// A created pipeline. Compute pipelines remember their kernel shader so a `Dispatch` can run it.
enum Pipeline {
    Render,
    Compute { shader: u32 },
}

pub struct SoftwareBackend {
    buffers: ResourceTable<Buffer>,
    textures: ResourceTable<Texture>,
    shaders: ResourceTable<ShaderModule>,
    pipelines: ResourceTable<Pipeline>,
    bind_groups: ResourceTable<BindGroupDesc>,
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
            Pipeline::Render => return Err(GpuError::Unsupported("dispatch on a render pipeline")),
        };
        // Clone the program out so the shader-table borrow is released before we touch buffers.
        let prog = match self.shaders.get(shader_id)? {
            ShaderModule::Kernel(p) => (**p).clone(),
            ShaderModule::Spirv(_) => return Ok(()), // software oracle cannot run SPIR-V
        };
        let bg = self.bind_groups.get(bgid)?.clone();
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
                let off = offset as usize;
                let len = if size == 0 {
                    buf.data.len().saturating_sub(off)
                } else {
                    size as usize
                };
                if off + len > buf.data.len() {
                    return Err(GpuError::OutOfBounds);
                }
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
                buf.data[off..off + regions[r].len()].copy_from_slice(&regions[r]);
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
        self.buffers.insert(id.0, Buffer { data: vec![0u8; desc.size as usize] })
    }
    fn destroy_buffer(&mut self, id: BufferId) -> Result<()> {
        self.buffers.remove(id.0).map(|_| ())
    }
    fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> Result<()> {
        let b = self.buffers.get_mut(id.0)?;
        let off = offset as usize;
        if off + data.len() > b.data.len() {
            return Err(GpuError::OutOfBounds);
        }
        b.data[off..off + data.len()].copy_from_slice(data);
        Ok(())
    }
    fn read_buffer(&mut self, id: BufferId, offset: u64, out: &mut [u8]) -> Result<()> {
        let b = self.buffers.get(id.0)?;
        let off = offset as usize;
        if off + out.len() > b.data.len() {
            return Err(GpuError::OutOfBounds);
        }
        out.copy_from_slice(&b.data[off..off + out.len()]);
        Ok(())
    }

    fn create_texture(&mut self, id: TextureId, desc: &TextureDesc) -> Result<()> {
        let bpt = Self::texel_bytes(desc.format)?;
        let n = bpt * desc.width as usize * desc.height as usize;
        self.textures.insert(
            id.0,
            Texture { desc: desc.clone(), pixels: vec![0u8; n] },
        )
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
        self.pipelines.insert(id.0, Pipeline::Render)
    }
    fn create_compute_pipeline(&mut self, id: PipelineId, desc: &ComputePipelineDesc) -> Result<()> {
        self.shaders.get(desc.compute.module)?;
        self.pipelines.insert(id.0, Pipeline::Compute { shader: desc.compute.module })
    }
    fn destroy_pipeline(&mut self, id: PipelineId) -> Result<()> {
        self.pipelines.remove(id.0).map(|_| ())
    }

    fn create_bind_group(&mut self, id: BindGroupId, desc: &BindGroupDesc) -> Result<()> {
        for e in &desc.entries {
            match &e.resource {
                BindResource::Buffer { id, .. } => {
                    self.buffers.get(*id)?;
                }
                BindResource::Texture { id } => {
                    self.textures.get(*id)?;
                }
                BindResource::Sampler { id } => {
                    self.samplers.get(*id)?;
                }
            }
        }
        self.bind_groups.insert(id.0, desc.clone())
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
        // Synchronous executor: work already completed at submit; just record the reached value.
        let v = self.fences.get_mut(id.0)?;
        *v = (*v).max(value);
        Ok(())
    }

    fn submit(&mut self, cb: &CommandBuffer) -> Result<()> {
        // Walk the encoder; execute clears + copies + compute dispatches, record draws.
        let mut cur_targets: Vec<ColorAttachment> = Vec::new();
        let mut cur_pipeline: Option<u32> = None;
        let mut cur_bind_group: Option<u32> = None;
        for op in &cb.encoder {
            match op {
                Enc::BeginRenderPass { color, .. } => {
                    // execute Clear load ops immediately
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
                        } else {
                            self.textures.get(c.texture)?; // validate
                        }
                    }
                    cur_targets = color.clone();
                }
                Enc::EndRenderPass => cur_targets.clear(),
                Enc::SetPipeline(p) => {
                    self.pipelines.get(*p)?;
                    cur_pipeline = Some(*p);
                }
                Enc::SetBindGroup { group, .. } => {
                    self.bind_groups.get(*group)?;
                    cur_bind_group = Some(*group);
                }
                Enc::Draw { .. } | Enc::DrawIndexed { .. } => {
                    let _ = &cur_targets;
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
                        if so + sz > s.data.len() {
                            return Err(GpuError::OutOfBounds);
                        }
                        s.data[so..so + sz].to_vec()
                    };
                    let d = self.buffers.get_mut(*dst)?;
                    let d_off = *dst_offset as usize;
                    if d_off + chunk.len() > d.data.len() {
                        return Err(GpuError::OutOfBounds);
                    }
                    d.data[d_off..d_off + chunk.len()].copy_from_slice(&chunk);
                }
                Enc::CopyBufferToTexture { src, src_offset, dst, width, height, .. } => {
                    let (fmt,) = {
                        let t = self.textures.get(*dst)?;
                        (t.desc.format,)
                    };
                    let bpt = Self::texel_bytes(fmt)?;
                    let need = bpt * (*width as usize) * (*height as usize);
                    let chunk = {
                        let s = self.buffers.get(*src)?;
                        let so = *src_offset as usize;
                        if so + need > s.data.len() {
                            return Err(GpuError::OutOfBounds);
                        }
                        s.data[so..so + need].to_vec()
                    };
                    let t = self.textures.get_mut(*dst)?;
                    if need > t.pixels.len() {
                        return Err(GpuError::OutOfBounds);
                    }
                    t.pixels[..need].copy_from_slice(&chunk);
                }
                Enc::CopyTextureToBuffer { src, width, height, dst, dst_offset, .. } => {
                    let (fmt,) = {
                        let t = self.textures.get(*src)?;
                        (t.desc.format,)
                    };
                    let bpt = Self::texel_bytes(fmt)?;
                    let need = bpt * (*width as usize) * (*height as usize);
                    let chunk = {
                        let t = self.textures.get(*src)?;
                        if need > t.pixels.len() {
                            return Err(GpuError::OutOfBounds);
                        }
                        t.pixels[..need].to_vec()
                    };
                    let d = self.buffers.get_mut(*dst)?;
                    let d_off = *dst_offset as usize;
                    if d_off + chunk.len() > d.data.len() {
                        return Err(GpuError::OutOfBounds);
                    }
                    d.data[d_off..d_off + chunk.len()].copy_from_slice(&chunk);
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
