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
        // Every dimension is materialized, as one tight plane per array layer / depth slice / cube face.
        // 1D, 3D and cube were refused here on the recorded grounds that the executor had no operation on
        // them for this reference to agree with. RE-MEASURED on current code, that is false: the executor
        // serves `ClearRect`, both buffer↔texture copies and a texture-to-texture copy on all three, and
        // readback returns their base slice, so the class is comparable. Only the blit still declines 1D
        // and 3D, and this executor declines them too (`operation::validate_op`).
        //
        // The SHAPE rules below are the executor's, not this reference's preference, and each was
        // measured against it. Getting one wrong would not refuse a legal texture — it would allocate a
        // different number of planes from the subject for the same descriptor, which is a divergence in
        // the storage rather than in an operation, and every later comparison would inherit it.
        if desc.dim == TextureDim::D1 && desc.height != 1 {
            return Err(GpuError::Invalid("1D texture must have height == 1"));
        }
        if desc.dim == TextureDim::Cube {
            if !Texture::planes(desc).is_multiple_of(6) {
                return Err(GpuError::Invalid(
                    "cube texture layer count must be a multiple of 6",
                ));
            }
            if desc.width != desc.height {
                return Err(GpuError::Invalid("cube texture faces must be square"));
            }
        }
        if desc.depth == 0 && desc.dim != TextureDim::Cube {
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
        if desc.sample_count > 1 && (Texture::planes(desc) > 1 || desc.dim != TextureDim::D2) {
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
        // One rule for how many bytes a texel occupies here, shared with the plane addressing in
        // `Texture::plane_at`. Sizing the allocation by one rule and indexing it by another is how a
        // depth plane comes to be allocated and then unreachable.
        let bpt = match Texture::texel_bytes(desc) {
            Some(n) => n,
            None => desc.format.software_texel_bytes()?,
        };
        // Every LEVEL of every layer. The mip chain used to be accepted in the descriptor and never
        // allocated, which was sound only because every path refused a non-zero level; the executor
        // materializes and serves the whole pyramid, so this now does too.
        let n = Texture::total_bytes(desc, bpt).ok_or(GpuError::OutOfBounds)?;
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
