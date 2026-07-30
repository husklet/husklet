use super::*;
use crate::protocol::model::command::etag;

/// The encoder ops this oracle actually replays. It is [`ALL_COMMANDS`] MINUS the two explicit-region
/// buffer↔texture copies: the oracle materializes only mip 0 of a single layer, so it refuses those two
/// ops outright (`software: layered or offset buffer-texture copy`). Advertising a command the executor
/// then refuses is the failure the capability handshake exists to prevent — a guest that requires them
/// must fail cleanly at negotiation, not at replay.
const REPLAYED_COMMANDS: &[u8] = &[
    etag::BEGIN_RENDER_PASS,
    etag::END_RENDER_PASS,
    etag::SET_PIPELINE,
    etag::SET_BIND_GROUP,
    etag::SET_VERTEX_BUFFER,
    etag::SET_INDEX_BUFFER,
    etag::SET_VIEWPORT,
    etag::SET_SCISSOR,
    etag::CLEAR_RECT,
    etag::DRAW,
    etag::DRAW_INDEXED,
    etag::BEGIN_COMPUTE_PASS,
    etag::END_COMPUTE_PASS,
    etag::DISPATCH,
    etag::COPY_B2B,
    etag::COPY_B2T,
    etag::COPY_T2B,
    etag::COPY_T2T,
    etag::BLIT_TEXTURE,
    etag::RESOLVE_TEXTURE,
    etag::FILL_BUFFER,
    etag::SET_STENCIL_REFERENCE,
    etag::SET_BLEND_CONSTANT,
];

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
            command_bits: Capabilities::command_bits(REPLAYED_COMMANDS),
            // Executes compiled kernels; it cannot run a graphics (SPIR-V/MSL) shader.
            shader_payloads: shader_payload::KERNEL,
            // Color formats plus every depth format the oracle materializes for depth-tested rendering.
            // The shared `DEPTH_FORMATS` is depth-only; this oracle also materializes the COMBINED
            // depth+stencil plane and runs the full stencil test/op set against it (see
            // `raster::raster_draw_depth`), so it must advertise that format too — otherwise a guest
            // negotiates the stencil ops and then has every stencil attachment refused at validation.
            texture_formats: TextureFormat::bits(COLOR_FORMATS)
                | TextureFormat::bits(DEPTH_FORMATS)
                | TextureFormat::bits(&[TextureFormat::Depth24PlusStencil8]),
            // Browser-class per-frame wire-byte ceiling (256 MiB): a hostile-DoS guard, not a correctness
            // bound — the `GlobalLedger` is the true host-OOM guard. Raised from 64 MiB, which tripped
            // healthy browser frames. (Matches the wgpu executor; see its rationale.)
            max_frame_bytes: 256 << 20,
            max_buffer_bytes: 256 << 20,
            max_bind_groups: 4,
            // Synchronous; a fence only reaches a value a submit signalled.
            supports_timeline_fences: false,
            binding_arrays: 0,
            non_uniform_binding_arrays: 0,
            gpu_features: 0,
        }
    }

    fn execute(&mut self, res: &mut SessionResources, batch: &[Cmd]) -> Result<Vec<Presentation>> {
        let _span = hl_log::hl_span!(hl_log::tag::CPU, "dispatch");
        hl_log::hl_debug!(hl_log::tag::CPU, "execute cmds={}", batch.len());
        let mut presents = Vec::new();
        for cmd in batch {
            match cmd {
                Cmd::CreateBuffer(id, d) => res.buffers.insert(
                    *id,
                    Box::new(Buffer {
                        data: vec![0u8; d.size as usize],
                        usage: d.usage,
                    }),
                )?,
                Cmd::DestroyBuffer(id) => {
                    res.buffers.remove(*id)?;
                }
                Cmd::WriteBuffer { id, offset, data } => {
                    self.write_buffer(res, *id, *offset, data)?
                }
                Cmd::CreateTexture(id, d) => self.create_texture(res, *id, d)?,
                Cmd::DestroyTexture(id) => {
                    res.textures.remove(*id)?;
                }
                Cmd::CreateTextureView(id, view) => {
                    if view.base_mip != 0
                        || view.mip_count != 1
                        || view.base_layer != 0
                        || view.layer_count != 1
                    {
                        return Err(GpuError::Unsupported("software: texture subresource views"));
                    }
                    let texture = texture(res, view.texture)?.clone();
                    if view.format != texture.desc.format
                        || view.aspect != crate::protocol::model::enums::TextureAspect::All
                    {
                        return Err(GpuError::Invalid("software: incompatible texture view"));
                    }
                    res.textures.insert(*id, Box::new(texture))?;
                }
                Cmd::DestroyTextureView(id) => {
                    res.textures.remove(*id)?;
                }
                Cmd::CreateSampler(id, _) => res.samplers.insert(*id, Box::new(Sampler))?,
                Cmd::DestroySampler(id) => {
                    res.samplers.remove(*id)?;
                }
                Cmd::CreateShader { id, kind, spirv } => {
                    self.create_shader(res, *id, *kind, spirv)?
                }
                Cmd::DestroyShader(id) => {
                    res.shaders.remove(*id)?;
                }
                Cmd::CreateRenderPipeline(id, d) => self.create_render_pipeline(res, *id, d)?,
                Cmd::CreateComputePipeline(id, d) => self.create_compute_pipeline(res, *id, d)?,
                Cmd::CreateRenderPipelineLayout(id, d, _, _) => {
                    self.create_render_pipeline(res, *id, d)?
                }
                Cmd::CreateComputePipelineLayout(id, d, _) => {
                    self.create_compute_pipeline(res, *id, d)?
                }
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
                        return Err(GpuError::Invalid(
                            "wait on a fence value that was never signalled",
                        ));
                    }
                }
                Cmd::Present {
                    surface,
                    texture,
                    serial,
                } => {
                    presents.push(self.present(res, *surface, *texture, *serial)?);
                }
            }
        }
        Ok(presents)
    }

    fn wait(
        &mut self,
        resources: &mut SessionResources,
        fence_id: FenceId,
        value: u64,
    ) -> Result<()> {
        let v = *fence(resources, fence_id.0)?;
        if v < value {
            return Err(GpuError::Invalid(
                "wait on a fence value that was never signalled",
            ));
        }
        Ok(())
    }

    fn poll_fence(&mut self, res: &SessionResources, fence: FenceId, value: u64) -> Result<bool> {
        Ok(res
            .fences
            .get(fence.0)?
            .downcast_ref::<u64>()
            .is_some_and(|signaled| *signaled >= value))
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
