use super::*;
use crate::protocol::model::command::etag;

/// The encoder ops this oracle actually replays. It is [`ALL_COMMANDS`] MINUS the two explicit-region
/// buffer↔texture copies: the oracle materializes only mip 0, so it refuses those two ops outright
/// (`software: layered or offset buffer-texture copy`). Advertising a command the executor then refuses
/// is the failure the capability handshake exists to prevent — a guest that requires them must fail
/// cleanly at negotiation, not at replay.
///
/// These are ENCODER etags. `Cmd::CreateTextureView`, which this executor also refuses, is a top-level
/// command with no bit in this set, so its refusal reaches the caller at replay rather than at
/// negotiation — a gap in the handshake's coverage rather than in this list.
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
            // Per-frame wire-byte ceiling (browser-class, 256 MiB): a hostile-DoS guard on one decoded
            // frame's transient allocation, not a correctness bound — the `GlobalLedger` is the true
            // host-OOM guard. Raised from 64 MiB, which tripped healthy browser frames. Stays under the
            // 512 MiB transport pre-read guard (`transport::adapter::unix::MAX_FRAME_BYTES`).
            max_frame_bytes: 256 << 20,
            // Single-allocation ceiling, stated on THIS oracle's own terms rather than copied from the
            // wgpu executor's (which derives its figure from the adapter's `max_buffer_size` and clamps to
            // 1 GiB). This oracle has no device: `Cmd::CreateBuffer` is `vec![0u8; size]` and every texture
            // is a materialized host-RAM plane, so the ceiling is committed, zero-filled process memory the
            // moment the command is replayed — there is no lazily-paged device address space to hide in.
            // 256 MiB per allocation is what a software oracle can honestly serve on a normal host; it is
            // deliberately smaller than the wgpu path's and must not be raised to match it.
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
                // A texture VIEW is an ALIAS of its texture, and this reference cannot express one.
                //
                // It used to accept the base view — whole mip, whole layer — by CLONING the texture into
                // the view's id. That is a snapshot, not an alias, and the difference is observable in
                // both directions. Measured against the executor on one program: clear the texture red,
                // then clear THROUGH a base view green, then read the texture. The executor reports green
                // (the view names the same image); this reference reported red, because the write landed
                // in a copy. Reading a view after writing its texture diverges the same way with the
                // roles swapped. Nothing caught it because no differential generator emits a view.
                //
                // Refused rather than modelled, because a wrong answer here is worse than no answer: an
                // oracle that quietly disagrees with the subject is the thing the differential is built
                // to detect, and it cannot detect it in itself. A faithful alias needs two ids to name
                // one object, which contradicts the singular-ownership rule this executor's storage is
                // built on (see `cpu::model`) and outlives the parent on the executor besides — so it is
                // a change to the resource model, not to this arm.
                //
                // Non-base views were already refused, and their message is kept for that case so the
                // narrower refusal stays distinguishable from this general one.
                Cmd::CreateTextureView(_, view) => {
                    if view.base_mip != 0
                        || view.mip_count != 1
                        || view.base_layer != 0
                        || view.layer_count != 1
                    {
                        return Err(GpuError::Unsupported("software: texture subresource views"));
                    }
                    return Err(GpuError::Unsupported(
                        "software: texture views (a view aliases its texture; this reference has no alias)",
                    ));
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
