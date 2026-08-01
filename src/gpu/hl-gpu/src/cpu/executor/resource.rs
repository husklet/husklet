use super::validation::texture_with_usage;
use super::*;

impl CpuExecutor {
    // --- per-command handlers (ported from SoftwareBackend's create_*/write/submit/present) ---------

    pub(super) fn create_texture(
        &self,
        res: &mut SessionResources,
        id: u32,
        desc: &TextureDesc,
    ) -> Result<()> {
        if desc.width == 0 || desc.height == 0 {
            return Err(GpuError::Invalid("zero-sized texture"));
        }
        // A 2D texture may be LAYERED. Only the operations the executor itself serves on a layered
        // texture are served here — a layered `ClearRect` writes a chosen layer range, and a region copy
        // reads one out — and everything else refuses a non-base layer exactly as the executor does. That
        // symmetry is the point: a reference that accepted what the subject refuses would manufacture a
        // divergence in the other direction.
        //
        // 1D, 3D and cube stay refused. They are not blocked by storage — this plane is layer-major and
        // a depth slice or a cube face would sit in it identically — but by the executor, which refuses a
        // non-base subresource on every copy, blit and resolve, so there is no operation for the reference
        // to agree with. Widen this when the executor grows one, not before.
        if desc.dim != TextureDim::D2 {
            return Err(GpuError::Unsupported(
                "software: only 2D textures (1D, 3D and cube have no layered operation to agree on)",
            ));
        }
        if desc.depth == 0 {
            return Err(GpuError::Invalid("texture layer count must be >= 1"));
        }
        if desc.mip_levels == 0 {
            return Err(GpuError::Invalid("texture mip_levels must be >= 1"));
        }
        if !matches!(desc.sample_count, 1 | 2 | 4 | 8) {
            return Err(GpuError::Unsupported("software: unsupported sample count"));
        }
        // MULTISAMPLED and LAYERED together is refused because the executor refuses it: measured, wgpu
        // rejects the creation outright with "Multisampled texture depth or array layers must be 1". This
        // reference could allocate one — the plane is layer-major and samples interleave within a texel —
        // and that is the point. Accepting a shape the subject cannot create would put a texture in the
        // differential that only one backend can hold.
        if desc.sample_count > 1 && desc.depth > 1 {
            return Err(GpuError::Unsupported(
                "software: a multisampled texture cannot be layered",
            ));
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
            _ => desc.format.software_texel_bytes()?,
        };
        let n = bpt
            .checked_mul(desc.width as usize)
            .and_then(|v| v.checked_mul(desc.height as usize))
            .and_then(|v| v.checked_mul(desc.sample_count as usize))
            .and_then(|v| v.checked_mul(desc.depth.max(1) as usize))
            .ok_or(GpuError::OutOfBounds)?;
        res.textures.insert(
            id,
            Box::new(Texture {
                desc: desc.clone(),
                pixels: vec![0u8; n],
            }),
        )
    }

    pub(super) fn create_shader(
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
                        let compiler = self
                            .kernel_compiler
                            .as_ref()
                            .ok_or(GpuError::Unsupported(
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
            // GLSL / MSL / demo graphics payloads are opaque to the fixed-function CPU oracle: it
            // rasterizes from the pipeline + vertex data, not the shader source, so any graphics module is
            // an accepted opaque handle here (the real GLSL compile happens on the wgpu executor).
            ShaderPayloadKind::Glsl | ShaderPayloadKind::Msl | ShaderPayloadKind::DemoBuiltin => {
                ShaderModule::Spirv
            }
        };
        res.shaders.insert(id, Box::new(module))
    }

    pub(super) fn create_render_pipeline(
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

    pub(super) fn create_compute_pipeline(
        &self,
        res: &mut SessionResources,
        id: u32,
        desc: &ComputePipelineDesc,
    ) -> Result<()> {
        res.shaders.get(desc.compute.module)?;
        res.pipelines.insert(
            id,
            Box::new(Pipeline::Compute {
                shader: desc.compute.module,
            }),
        )
    }

    pub(super) fn create_bind_group(
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
                    buffers.push(GenRef {
                        id: *id,
                        gen: res.buffers.generation(*id).unwrap(),
                    });
                }
                BindResource::Texture { id } => {
                    texture_with_usage(
                        res,
                        *id,
                        texture_usage::SAMPLED,
                        "texture bound without SAMPLED usage",
                    )?;
                    textures.push(GenRef {
                        id: *id,
                        gen: res.textures.generation(*id).unwrap(),
                    });
                }
                BindResource::Sampler { id } => {
                    res.samplers.get(*id)?;
                    samplers.push(GenRef {
                        id: *id,
                        gen: res.samplers.generation(*id).unwrap(),
                    });
                }
                BindResource::BufferArray { .. }
                | BindResource::TextureArray { .. }
                | BindResource::SamplerArray { .. } => {
                    return Err(GpuError::Unsupported(
                        "cpu executor: binding arrays are unsupported",
                    ));
                }
            }
        }
        res.bind_groups.insert(
            id,
            Box::new(BindGroupState {
                desc: desc.clone(),
                buffers,
                textures,
                samplers,
            }),
        )
    }
}
