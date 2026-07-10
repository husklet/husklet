//! A `GpuBackend` that records the replayed command sequence and enforces resource lifecycle via
//! [`ResourceTable`]s. The test oracle for "did the guest→wire→replay chain reach the host intact?"

use crate::backend::{Capabilities, GpuBackend, PresentKind, PresentToken};
use crate::id::*;
use crate::ir::*;
use crate::{GpuError, Result};

/// One recorded backend event. Tests assert on the exact `Vec<Rec>` a replay produced.
#[derive(Clone, PartialEq, Debug)]
pub enum Rec {
    CreateBuffer(u32, u64, u32),
    DestroyBuffer(u32),
    WriteBuffer(u32, u64, usize),
    CreateTexture(u32, u32, u32),
    DestroyTexture(u32),
    CreateShader(u32, usize),
    DestroyShader(u32),
    CreateRenderPipeline(u32),
    CreateComputePipeline(u32),
    CreateBindGroup(u32),
    CreateSurface(u32),
    CreateFence(u32),
    WaitFence(u32, u64),
    BeginRenderPass(usize),
    EndRenderPass,
    SetPipeline(u32),
    Draw(u32, u32),
    Dispatch(u32, u32, u32),
    Submit(usize),
    Present(u32, u32),
}

pub struct RecordingBackend {
    pub log: Vec<Rec>,
    buffers: ResourceTable<()>,
    textures: ResourceTable<()>,
    shaders: ResourceTable<()>,
    pipelines: ResourceTable<()>,
    bind_groups: ResourceTable<()>,
    surfaces: ResourceTable<()>,
    fences: ResourceTable<()>,
    samplers: ResourceTable<()>,
}

impl Default for RecordingBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingBackend {
    pub fn new() -> Self {
        Self {
            log: Vec::new(),
            buffers: ResourceTable::new(BufferId::KIND),
            textures: ResourceTable::new(TextureId::KIND),
            shaders: ResourceTable::new(ShaderId::KIND),
            pipelines: ResourceTable::new(PipelineId::KIND),
            bind_groups: ResourceTable::new(BindGroupId::KIND),
            surfaces: ResourceTable::new(SurfaceId::KIND),
            fences: ResourceTable::new(FenceId::KIND),
            samplers: ResourceTable::new(SamplerId::KIND),
        }
    }
}

impl GpuBackend for RecordingBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "dd-mock".into(),
            unified_memory: true,
            supports_compute: true,
            supports_graphics: true,
            max_texture_2d: 16384,
            present_kinds: vec![PresentKind::Shm],
        }
    }

    fn create_buffer(&mut self, id: BufferId, desc: &BufferDesc) -> Result<()> {
        self.buffers.insert(id.0, ())?;
        self.log.push(Rec::CreateBuffer(id.0, desc.size, desc.usage));
        Ok(())
    }
    fn destroy_buffer(&mut self, id: BufferId) -> Result<()> {
        self.buffers.remove(id.0)?;
        self.log.push(Rec::DestroyBuffer(id.0));
        Ok(())
    }
    fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> Result<()> {
        self.buffers.get(id.0)?; // use-after-free check
        self.log.push(Rec::WriteBuffer(id.0, offset, data.len()));
        Ok(())
    }

    fn create_texture(&mut self, id: TextureId, desc: &TextureDesc) -> Result<()> {
        self.textures.insert(id.0, ())?;
        self.log.push(Rec::CreateTexture(id.0, desc.width, desc.height));
        Ok(())
    }
    fn destroy_texture(&mut self, id: TextureId) -> Result<()> {
        self.textures.remove(id.0)?;
        self.log.push(Rec::DestroyTexture(id.0));
        Ok(())
    }

    fn create_sampler(&mut self, id: SamplerId, _desc: &SamplerDesc) -> Result<()> {
        self.samplers.insert(id.0, ())?;
        Ok(())
    }
    fn destroy_sampler(&mut self, id: SamplerId) -> Result<()> {
        self.samplers.remove(id.0)?;
        Ok(())
    }

    fn create_shader(&mut self, id: ShaderId, spirv: &[u32]) -> Result<()> {
        self.shaders.insert(id.0, ())?;
        self.log.push(Rec::CreateShader(id.0, spirv.len()));
        Ok(())
    }
    fn destroy_shader(&mut self, id: ShaderId) -> Result<()> {
        self.shaders.remove(id.0)?;
        self.log.push(Rec::DestroyShader(id.0));
        Ok(())
    }

    fn create_render_pipeline(&mut self, id: PipelineId, desc: &RenderPipelineDesc) -> Result<()> {
        // validate referenced shader modules exist
        self.shaders.get(desc.vertex.module)?;
        if let Some(f) = &desc.fragment {
            self.shaders.get(f.module)?;
        }
        self.pipelines.insert(id.0, ())?;
        self.log.push(Rec::CreateRenderPipeline(id.0));
        Ok(())
    }
    fn create_compute_pipeline(&mut self, id: PipelineId, desc: &ComputePipelineDesc) -> Result<()> {
        self.shaders.get(desc.compute.module)?;
        self.pipelines.insert(id.0, ())?;
        self.log.push(Rec::CreateComputePipeline(id.0));
        Ok(())
    }
    fn destroy_pipeline(&mut self, id: PipelineId) -> Result<()> {
        self.pipelines.remove(id.0)?;
        Ok(())
    }

    fn create_bind_group(&mut self, id: BindGroupId, desc: &BindGroupDesc) -> Result<()> {
        // validate every referenced resource is live
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
        self.bind_groups.insert(id.0, ())?;
        self.log.push(Rec::CreateBindGroup(id.0));
        Ok(())
    }
    fn destroy_bind_group(&mut self, id: BindGroupId) -> Result<()> {
        self.bind_groups.remove(id.0)?;
        Ok(())
    }

    fn create_surface(&mut self, id: SurfaceId, _desc: &SurfaceDesc) -> Result<()> {
        self.surfaces.insert(id.0, ())?;
        self.log.push(Rec::CreateSurface(id.0));
        Ok(())
    }
    fn destroy_surface(&mut self, id: SurfaceId) -> Result<()> {
        self.surfaces.remove(id.0)?;
        Ok(())
    }

    fn create_fence(&mut self, id: FenceId) -> Result<()> {
        self.fences.insert(id.0, ())?;
        self.log.push(Rec::CreateFence(id.0));
        Ok(())
    }
    fn destroy_fence(&mut self, id: FenceId) -> Result<()> {
        self.fences.remove(id.0)?;
        Ok(())
    }
    fn wait_fence(&mut self, id: FenceId, value: u64) -> Result<()> {
        self.fences.get(id.0)?;
        self.log.push(Rec::WaitFence(id.0, value));
        Ok(())
    }

    fn submit(&mut self, cb: &CommandBuffer) -> Result<()> {
        self.log.push(Rec::Submit(cb.encoder.len()));
        for op in &cb.encoder {
            match op {
                Enc::BeginRenderPass { color, .. } => {
                    // validate targets exist
                    for c in color {
                        self.textures.get(c.texture)?;
                    }
                    self.log.push(Rec::BeginRenderPass(color.len()));
                }
                Enc::EndRenderPass => self.log.push(Rec::EndRenderPass),
                Enc::ClearRect { texture, .. } => {
                    self.textures.get(*texture)?;
                }
                Enc::SetPipeline(p) => {
                    self.pipelines.get(*p)?;
                    self.log.push(Rec::SetPipeline(*p));
                }
                Enc::Draw { vertex_count, instance_count, .. } => {
                    self.log.push(Rec::Draw(*vertex_count, *instance_count));
                }
                Enc::Dispatch { x, y, z } => self.log.push(Rec::Dispatch(*x, *y, *z)),
                _ => {}
            }
        }
        if let Some((f, _)) = cb.signal {
            self.fences.get(f)?;
        }
        Ok(())
    }

    fn present(&mut self, surface: SurfaceId, texture: TextureId) -> Result<PresentToken> {
        self.surfaces.get(surface.0)?;
        self.textures.get(texture.0)?;
        self.log.push(Rec::Present(surface.0, texture.0));
        Ok(PresentToken {
            surface: surface.0,
            kind: PresentKind::Shm,
            handle: texture.0 as u64,
            width: 0,
            height: 0,
            format_ok: true,
        })
    }
}

/// A tiny fallible helper so tests can assert a specific `GpuError` variant escaped the trait.
pub fn expect_err<T>(r: Result<T>) -> GpuError {
    match r {
        Ok(_) => panic!("expected an error, got Ok"),
        Err(e) => e,
    }
}
