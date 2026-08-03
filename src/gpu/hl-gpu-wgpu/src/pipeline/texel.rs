use super::*;

impl WgpuExecutor {
    pub(crate) fn render_pipeline_for(
        &self,
        resources: &SessionResources,
        id: u32,
        specialization: &[crate::texel_buffer::Specialization],
    ) -> Result<wgpu::RenderPipeline> {
        let PipelineNative::Render {
            pipeline, texel, ..
        } = PipelineNative::get(resources, id)?
        else {
            return Err(GpuError::Unsupported("wgpu: draw on a compute pipeline"));
        };
        let Some(recipe) = texel else {
            return Ok(pipeline.clone());
        };
        let mut variants = recipe
            .variants
            .lock()
            .map_err(|_| GpuError::Invalid("wgpu: render texel pipeline cache poisoned"))?;
        if let Some(pipeline) = variants.get(specialization) {
            return Ok(pipeline);
        }
        let pipeline = self.compile_render_texel(recipe, specialization)?;
        variants.insert(specialization.to_vec(), pipeline.clone());
        Ok(pipeline)
    }

    fn compile_render_texel(
        &self,
        recipe: &crate::texel_buffer::RenderSpecializer,
        specialization: &[crate::texel_buffer::Specialization],
    ) -> Result<wgpu::RenderPipeline> {
        let (vertex_source, vertex_usage) = crate::wgsl::Spirv::translate_reflect_texel_sample(
            &recipe.vertex_words,
            &recipe.layout,
            specialization,
            false,
        )?;
        let vertex = self.gpu.shader_module("hl-render-texel-vertex", vertex_source)?;
        let (fragment, fragment_usage) = match (&recipe.desc.fragment, &recipe.fragment_words) {
            (Some(stage), Some(words)) => {
                let (source, usage) = crate::wgsl::Spirv::translate_reflect_texel_fragment(
                    words,
                    &recipe.layout,
                    specialization,
                    recipe.multisample.sample_shading,
                    &stage.entry,
                    &recipe.desc.color_targets,
                )?;
                (Some((self.gpu.shader_module("hl-render-texel-fragment", source)?, stage)), usage)
            }
            (None, None) => (None, crate::reflect::ModuleUsage::default()),
            _ => return Err(GpuError::Invalid("wgpu: render texel fragment recipe mismatch")),
        };
        let mut merged = BTreeMap::new();
        for (stage, bindings) in [
            (
                wgpu::ShaderStages::VERTEX,
                vertex_usage.used_for(&recipe.desc.vertex.entry),
            ),
            (
                wgpu::ShaderStages::FRAGMENT,
                recipe
                    .desc
                    .fragment
                    .as_ref()
                    .map(|fragment| fragment_usage.used_for(&fragment.entry))
                    .unwrap_or(&[]),
            ),
        ] {
            for binding in bindings {
                match merged.entry((binding.group, binding.binding)) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert((stage, binding.kind, binding.count));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let (visibility, kind, count) = entry.get_mut();
                        if *count != binding.count {
                            return Err(GpuError::Invalid(
                                "wgpu: texel binding array count differs across stages",
                            ));
                        }
                        *visibility |= stage;
                        *kind = BindingLayout::reconcile(*kind, binding.kind).ok_or(
                            GpuError::Invalid(
                                "wgpu: texel binding kind differs across render stages",
                            ),
                        )?;
                    }
                }
            }
        }
        Self::apply_authoritative_counts(&mut merged, Some(&recipe.layout))?;
        let reflected_bindings = merged.iter().map(|(&(group, binding), &(_, kind, count))| crate::reflect::Binding { group, binding, kind, count }).collect::<Vec<_>>();
        let sampler_metadata = crate::reflect::sampler_metadata(&reflected_bindings);
        Self::insert_sampler_metadata_bindings(&mut merged, &sampler_metadata)?;
        let group_layouts = self.build_render_bind_group_layouts(&merged)?;
        let layout_refs = group_layouts.iter().collect::<Vec<_>>();
        let layout = self
            .gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("hl-render-texel-pl"),
                bind_group_layouts: &layout_refs,
                push_constant_ranges: &[],
            });

        let attr_sets = recipe
            .desc
            .vertex_buffers
            .iter()
            .map(|layout| {
                layout
                    .attrs
                    .iter()
                    .map(|attribute| {
                        Ok(wgpu::VertexAttribute {
                            format: VertexState::format(attribute.format)?,
                            offset: attribute.offset as u64,
                            shader_location: attribute.location,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let vertex_buffers = recipe
            .desc
            .vertex_buffers
            .iter()
            .zip(&attr_sets)
            .map(|(layout, attributes)| wgpu::VertexBufferLayout {
                array_stride: layout.stride as u64,
                step_mode: VertexState::step_mode(layout),
                attributes,
            })
            .collect::<Vec<_>>();
        let depth_stencil = recipe.desc.depth.as_ref().map(|state| wgpu::DepthStencilState {
            format: Format::from(state.format).native(),
            depth_write_enabled: state.depth_write,
            depth_compare: CompareFunction(state.depth_compare).native(),
            stencil: wgpu::StencilState {
                front: StencilState::face(&state.stencil_front),
                back: StencilState::face(&state.stencil_back),
                read_mask: state.stencil_read_mask,
                write_mask: state.stencil_write_mask,
            },
            bias: wgpu::DepthBiasState {
                constant: state.bias_constant,
                slope_scale: state.bias_slope_scale,
                clamp: state.bias_clamp,
            },
        });
        let targets = recipe
            .desc
            .color_targets
            .iter()
            .map(|target| {
                Some(wgpu::ColorTargetState {
                    format: Format::from(target.format).native(),
                    blend: target.blend.as_ref().map(BlendState::lower),
                    write_mask: ColorMask(target.write_mask).native(),
                })
            })
            .collect::<Vec<_>>();
        self.gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = self
            .gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("hl-render-texel"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &vertex,
                    entry_point: Some(recipe.desc.vertex.entry.as_str()),
                    buffers: &vertex_buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: PrimitiveTopology(recipe.desc.topology).native(),
                    front_face: FrontFace(recipe.desc.front_face).native()?,
                    cull_mode: CullMode(recipe.desc.cull).native()?,
                    ..Default::default()
                },
                depth_stencil,
                multisample: wgpu::MultisampleState {
                    count: recipe.desc.sample_count.max(1),
                    mask: recipe.multisample.mask,
                    ..Default::default()
                },
                fragment: fragment.as_ref().map(|(module, stage)| wgpu::FragmentState {
                    module,
                    entry_point: Some(stage.entry.as_str()),
                    targets: &targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview: None,
                cache: None,
            });
        if let Some(error) = pollster::block_on(self.gpu.device.pop_error_scope()) {
            return Err(GpuError::Kernel(format!(
                "wgpu: specialized texel render pipeline failed: {error:?}"
            )));
        }
        Ok(pipeline)
    }
}
