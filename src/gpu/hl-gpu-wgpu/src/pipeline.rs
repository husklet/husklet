//! Native pipeline objects — a compiled `wgpu::RenderPipeline` or `wgpu::ComputePipeline`.
//!
//! A **compute** pipeline lowers its kernel's [`KernelProgram`] to WGSL (`wgsl::kernel_to_wgsl`), compiles
//! it, and builds a pipeline with an *auto* bind-group layout — so the storage bindings the WGSL declares
//! (param blob at 0, region `r` at `r+1`) are exactly the layout a bind group is later built against. A
//! **render** pipeline is assembled from the naga-translated graphics modules and the IR's color-target /
//! topology descriptors, again with an auto layout. Both use auto layouts because the protocol creates
//! bind groups independently of pipelines; the concrete `wgpu::BindGroup` is built at draw/dispatch time
//! from the bound pipeline's own layout (see `bindgroup.rs`).

use std::collections::BTreeMap;
use std::time::Instant;

use hl_gpu::protocol::model::descriptor::{
    ComputePipelineDesc, PipelineBindingKind, PipelineLayout, RenderPipelineDesc, StencilFaceState,
    VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    compare, stencil_op, TextureFormat, TextureNumericClass, Topology,
};
use hl_gpu::protocol::model::kernel::GlslDescriptor;
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::convert::Format;
use crate::reflect::{BindingKind, TexDim, TexSample};
use crate::shader::{self, ShaderNative};
use crate::wgsl;
use crate::WgpuExecutor;

const SAMPLER_BINDING_OFFSET: u32 = 16;

impl WgpuExecutor {
    pub(crate) fn create_compute_pipeline(
        &mut self,
        res: &mut SessionResources,
        id: u32,
        desc: &ComputePipelineDesc,
        authoritative_layout: Option<&PipelineLayout>,
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(hl_log::tag::WGPU, "pipeline_create");
        let shader = shader::ShaderNative::get(res, desc.compute.module)?;
        let pipe_key = match shader {
            ShaderNative::Module { key, .. } => {
                crate::dedup::ComputePipeKey::module(desc, key.clone(), authoritative_layout)
            }
            ShaderNative::Kernel(program) => crate::dedup::ComputePipeKey::Kernel {
                program: (**program).clone(),
                entry: desc.compute.entry.clone(),
            },
        };
        if let Some((pipeline, remap_group_zero, texel, backing)) =
            self.dedup.compute_pipeline_get(&pipe_key)
        {
            res.pipelines.insert(
                id,
                Box::new(PipelineNative::Compute {
                    pipeline,
                    backing,
                    remap_group_zero,
                    texel,
                }),
            )?;
            self.pipeline_journal.push(Mutation::ComputeHit(pipe_key));
            return Ok(());
        }
        if let Some(artifact) = self.pipelines.compute_get(&pipe_key) {
            let backing = self.dedup.compute_pipeline_install(
                pipe_key.clone(),
                artifact.pipeline.clone(),
                artifact.remap_group_zero,
                artifact.texel.clone(),
            );
            if let Err(error) = res.pipelines.insert(
                id,
                Box::new(PipelineNative::Compute {
                    pipeline: artifact.pipeline,
                    backing,
                    remap_group_zero: artifact.remap_group_zero,
                    texel: artifact.texel,
                }),
            ) {
                self.dedup.compute_pipeline_release(backing);
                return Err(error);
            }
            self.pipeline_journal.push(Mutation::ComputeHit(pipe_key));
            return Ok(());
        }
        if let Some(profile) = self.profile.borrow_mut().as_mut() {
            profile.compute_pipeline_compilations =
                profile.compute_pipeline_compilations.saturating_add(1);
        }
        let mut texel = None;
        let (pipeline, remap_group_zero) = match shader {
            // PTX-kernel ABI: lower the neutral kernel IR to a WGSL compute entry point and build with an
            // EXPLICIT group-0 layout — binding 0 the read-only param blob, binding r+1 the read_write
            // pointer region r. Declaring every binding (even one the WGSL doesn't read) keeps the bind
            // group the protocol builds in lock-step with the layout that `get_bind_group_layout(0)` returns.
            ShaderNative::Kernel(p) => {
                let prog = p.clone();
                let src = wgsl::Kernel::translate(&prog)?;
                let module = self.gpu.shader_module("hl-kernel", src)?;
                let mut entries = vec![ComputeLayout::storage(0, true)];
                for r in 0..prog.num_regions {
                    entries.push(ComputeLayout::storage(r + 1, false));
                }
                let layout =
                    self.gpu
                        .device
                        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                            label: Some("hl-compute-bgl"),
                            entries: &entries,
                        });
                let pipeline_layout =
                    self.gpu
                        .device
                        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            label: Some("hl-compute-pl"),
                            bind_group_layouts: &[&layout],
                            push_constant_ranges: &[],
                        });
                (
                    self.gpu
                        .device
                        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                            label: Some("hl-compute"),
                            layout: Some(&pipeline_layout),
                            module: &module,
                            entry_point: Some(desc.compute.entry.as_str()),
                            compilation_options: wgpu::PipelineCompilationOptions::default(),
                            cache: None,
                        }),
                    false,
                )
            }
            // SPIR-V / GLSL compute: naga already translated the payload to a wgpu `ShaderModule` carrying
            // a compute entry point (`@compute @workgroup_size(..)`), exactly as it does for graphics
            // stages. Build with an AUTO layout (`layout: None`): wgpu reflects the module and derives every
            // bind-group layout AND the push-constant range from what the shader declares. This is the path
            // wgpu-core's OWN internal indirect-draw-VALIDATION compute pipeline needs — the one Zed's guest
            // wgpu builds during device creation (2 bind groups, a dynamic-offset buffer, a `var<push_constant>`).
            // Restricting compute to Kernel here was what made that pipeline "needs-kernel-shader"-reject and
            // cost Zed its device. Per-group layouts come from the pipeline itself at dispatch
            // (`get_bind_group_layout(index)`), so 2+ groups bind at their declared set indices. Push
            // constants require the PUSH_CONSTANTS feature, which `device::acquire` requests when the
            // adapter advertises it (lavapipe does).
            ShaderNative::Module {
                module: m,
                reflected,
                key,
            } => {
                if let Some(layout) = authoritative_layout.filter(|layout| {
                    key.kind == hl_gpu::ShaderPayloadKind::SpirV as u8
                        && layout.bindings.iter().any(|binding| {
                            matches!(
                                binding.kind,
                                PipelineBindingKind::UniformTexelBuffer
                                    | PipelineBindingKind::StorageTexelBuffer
                            )
                        })
                }) {
                    texel = Some(std::sync::Arc::new(
                        crate::texel_buffer::ComputeSpecializer::new(
                            desc.clone(),
                            layout.clone(),
                            key.words.clone(),
                        ),
                    ));
                }
                let (module, reflected_override);
                let reflected = if authoritative_layout
                    .is_some_and(|layout| layout.bindings.iter().any(|binding| binding.count > 1))
                    && key.kind == hl_gpu::ShaderPayloadKind::SpirV as u8
                {
                    let (source, reflected) =
                        wgsl::Spirv::translate_reflect_layout(&key.words, authoritative_layout)?;
                    module = self.gpu.shader_module("hl-spirv-layout", source)?;
                    reflected_override = reflected;
                    &reflected_override
                } else {
                    module = m.clone();
                    reflected
                };
                let mut merged: BTreeMap<_, _> = reflected
                    .used_for(&desc.compute.entry)
                    .iter()
                    .map(|binding| {
                        (
                            (binding.group, binding.binding),
                            (wgpu::ShaderStages::COMPUTE, binding.kind, binding.count),
                        )
                    })
                    .collect();
                Self::apply_authoritative_counts(&mut merged, authoritative_layout)?;
                let group_layouts = self.build_render_bind_group_layouts(&merged)?;
                let layout_refs: Vec<_> = group_layouts.iter().collect();
                let push_constant_ranges = self
                    .gpu
                    .device
                    .features()
                    .contains(wgpu::Features::PUSH_CONSTANTS)
                    .then(|| wgpu::PushConstantRange {
                        stages: wgpu::ShaderStages::COMPUTE,
                        range: 0..self.gpu.device.limits().max_push_constant_size,
                    })
                    .into_iter()
                    .collect::<Vec<_>>();
                let pipeline_layout =
                    self.gpu
                        .device
                        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                            label: Some("hl-compute-module-pl"),
                            bind_group_layouts: &layout_refs,
                            push_constant_ranges: &push_constant_ranges,
                        });
                // A validation error scope turns wgpu's async device error (raised when the module has no
                // compute entry point matching `entry` — e.g. a graphics-only SPIR-V module used for
                // compute) into a clean typed error, instead of the default uncaptured handler PANICKING.
                self.gpu
                    .device
                    .push_error_scope(wgpu::ErrorFilter::Validation);
                let pipeline =
                    self.gpu
                        .device
                        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                            label: Some("hl-compute-spirv"),
                            layout: Some(&pipeline_layout),
                            module: &module,
                            entry_point: Some(desc.compute.entry.as_str()),
                            compilation_options: wgpu::PipelineCompilationOptions::default(),
                            cache: None,
                        });
                if let Some(e) = pollster::block_on(self.gpu.device.pop_error_scope()) {
                    hl_log::hl_warn!(
                        hl_log::tag::WGPU,
                        "pipeline rejected kind=compute reason=spirv-no-compute-entry err={}",
                        e
                    );
                    return Err(GpuError::Kernel(format!(
                        "wgpu: SPIR-V compute pipeline creation failed (entry {:?}): {e}",
                        desc.compute.entry
                    )));
                }
                (pipeline, true)
            }
        };
        let backing = self.dedup.compute_pipeline_install(
            pipe_key.clone(),
            pipeline.clone(),
            remap_group_zero,
            texel.clone(),
        );
        if let Err(error) = res.pipelines.insert(
            id,
            Box::new(PipelineNative::Compute {
                pipeline: pipeline.clone(),
                backing,
                remap_group_zero,
                texel: texel.clone(),
            }),
        ) {
            self.dedup.compute_pipeline_release(backing);
            return Err(error);
        }
        self.pipeline_journal.push(Mutation::ComputeInstall(
            pipe_key,
            crate::pipeline::ComputeArtifact {
                pipeline,
                remap_group_zero,
                texel,
            },
        ));
        Ok(())
    }

    pub(crate) fn create_render_pipeline(
        &mut self,
        res: &mut SessionResources,
        id: u32,
        desc: &RenderPipelineDesc,
        authoritative_layout: Option<&PipelineLayout>,
        multisample: hl_gpu::protocol::model::descriptor::RenderMultisample,
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(hl_log::tag::WGPU, "pipeline_create");
        // Clone the module + the used bindings (slot + type) of the entry point this pipeline binds out of
        // `res` (an immutable borrow), so the pipeline can be inserted (a mutable borrow) below carrying the
        // explicit layout's exact bindings for the draw-time bind-group filter (see `PipelineNative::Render`).
        // Also capture each stage module's CONTENT key so identical descriptors built from different shader
        // ids (but the same source) dedup to one compiled pipeline.
        let (vs, vs_used, vs_push_constant, vs_key) =
            match shader::ShaderNative::get(res, desc.vertex.module)? {
                ShaderNative::Module {
                    module,
                    reflected,
                    key,
                } => (
                    module.clone(),
                    reflected.used_for(&desc.vertex.entry).to_vec(),
                    reflected.uses_push_constant(&desc.vertex.entry),
                    key.clone(),
                ),
                ShaderNative::Kernel(_) => {
                    hl_log::hl_warn!(
                        hl_log::tag::WGPU,
                        "pipeline rejected kind=render stage=vertex reason=needs-graphics-shader"
                    );
                    return Err(GpuError::Unsupported(
                        "wgpu: render pipeline vertex needs a graphics shader",
                    ));
                }
            };
        let (mut fs, mut fs_used, mut fs_push_constant, fs_key) = match &desc.fragment {
            Some(f) => {
                match shader::ShaderNative::get(res, f.module)? {
                    ShaderNative::Module {
                        module,
                        reflected,
                        key,
                    } => (
                        Some((module.clone(), f.entry.clone())),
                        reflected.used_for(&f.entry).to_vec(),
                        reflected.uses_push_constant(&f.entry),
                        Some(key.clone()),
                    ),
                    ShaderNative::Kernel(_) => {
                        hl_log::hl_warn!(hl_log::tag::WGPU, "pipeline rejected kind=render stage=fragment reason=needs-graphics-shader");
                        return Err(GpuError::Unsupported(
                            "wgpu: render pipeline fragment needs a graphics shader",
                        ));
                    }
                }
            }
            None => (None, Vec::new(), false, None),
        };
        let render_texel = authoritative_layout
            .filter(|layout| {
                layout.bindings.iter().any(|binding| {
                    matches!(
                        binding.kind,
                        PipelineBindingKind::UniformTexelBuffer
                            | PipelineBindingKind::StorageTexelBuffer
                    )
                }) && vs_key.kind == hl_gpu::ShaderPayloadKind::SpirV as u8
                    && fs_key
                        .as_ref()
                        .is_none_or(|key| key.kind == hl_gpu::ShaderPayloadKind::SpirV as u8)
            })
            .map(|layout| {
                std::sync::Arc::new(crate::texel_buffer::RenderSpecializer::new(
                    desc.clone(),
                    layout.clone(),
                    multisample,
                    vs_key.words.clone(),
                    fs_key.as_ref().map(|key| key.words.clone()),
                ))
            });
        // wgpu's Metal backend does not program a raster sample mask at all (`wgpu-hal`'s
        // `metal::device` carries a literal `//TODO: handle sample mask`), so a non-default mask would be
        // silently dropped and the pass would shade every sample — wrong pixels with no error. Refuse it
        // explicitly instead. The neutral default (all bits set) is unaffected, so no ordinary pipeline
        // changes behavior.
        if multisample.mask != u64::MAX
            && desc.sample_count > 1
            && self.gpu.info.backend == wgpu::Backend::Metal
        {
            return Err(GpuError::Unsupported(
                "wgpu: this backend does not honour a multisample sample mask",
            ));
        }
        let integer_outputs = desc
            .color_targets
            .iter()
            .any(|target| target.format.numeric_class() != TextureNumericClass::Float);
        if multisample.sample_shading || integer_outputs {
            let Some((key, entry)) = fs_key.as_ref().zip(
                desc.fragment
                    .as_ref()
                    .map(|fragment| fragment.entry.as_str()),
            ) else {
                return Err(GpuError::Invalid(
                    "wgpu: sample shading requires a fragment stage",
                ));
            };
            if key.kind != hl_gpu::ShaderPayloadKind::SpirV as u8 {
                if multisample.sample_shading {
                    return Err(GpuError::Unsupported(
                        "wgpu: forced sample shading currently requires SPIR-V",
                    ));
                }
            } else {
                let (source, reflected) = wgsl::Spirv::translate_reflect_fragment_outputs(
                    &key.words,
                    authoritative_layout,
                    multisample.sample_shading,
                    entry,
                    &desc.color_targets,
                )?;
                fs = Some((
                    self.gpu.shader_module("hl-spirv-fragment-output", source)?,
                    entry.to_string(),
                ));
                fs_used = reflected.used_for(entry).to_vec();
                fs_push_constant = reflected.uses_push_constant(entry);
            }
        }

        // Content-dedup on the full pipeline identity: each stage's deduped shader CONTENT + entry point,
        // plus every fixed-function state field. An identical descriptor ALIASES the already-compiled
        // `wgpu::RenderPipeline` (a cheap `Arc` clone, ~0 incremental residency) and skips the naga merge +
        // layout build + PSO compile entirely. Distinct descriptors never share (full-value key compare).
        let pipe_key = crate::dedup::RenderPipeKey::from_desc(desc, vs_key, fs_key, multisample);
        if let Some((pipeline, color_formats, used_bindings, backing)) =
            self.dedup.pipeline_get(&pipe_key)
        {
            res.pipelines.insert(
                id,
                Box::new(PipelineNative::Render {
                    pipeline,
                    vertex_buffers: desc.vertex_buffers.clone(),
                    color_formats,
                    used_bindings,
                    backing,
                    texel: render_texel.clone(),
                }),
            )?;
            // Local reuse is also real device-artifact use. Commit the shared LRU touch only after the guest
            // id insertion and the surrounding batch succeed; eviction may already have removed the entry,
            // in which case the touch is intentionally a no-op.
            self.pipeline_journal.push(Mutation::Hit(pipe_key));
            hl_log::hl_count!(hl_log::tag::WGPU, "pipeline_dedup_hit");
            return Ok(());
        }
        if let Some(artifact) = self.pipelines.get(&pipe_key) {
            let backing = self.dedup.pipeline_install(
                pipe_key.clone(),
                artifact.pipeline.clone(),
                artifact.color_formats.clone(),
                artifact.used_bindings.clone(),
                crate::dedup::PIPELINE_BACKING_BYTES,
            );
            if let Err(error) = res.pipelines.insert(
                id,
                Box::new(PipelineNative::Render {
                    pipeline: artifact.pipeline,
                    vertex_buffers: desc.vertex_buffers.clone(),
                    color_formats: artifact.color_formats,
                    used_bindings: artifact.used_bindings,
                    backing,
                    texel: render_texel.clone(),
                }),
            ) {
                self.dedup.pipeline_release(backing);
                return Err(error);
            }
            self.pipeline_journal.push(Mutation::Hit(pipe_key));
            hl_log::hl_count!(hl_log::tag::WGPU, "pipeline_device_hit");
            return Ok(());
        }
        if let Some(profile) = self.profile.borrow_mut().as_mut() {
            profile.render_pipeline_compilations =
                profile.render_pipeline_compilations.saturating_add(1);
        }
        let diagnostics = hl_log::Logging::global()
            .enabled(hl_log::Tags::from(hl_log::tag::WGPU), hl_log::Level::Debug);
        let total_started = diagnostics.then(Instant::now);
        // One line per UNIQUE pipeline backing (never per draw): enough live evidence to correlate Chrome's
        // glyph/image shaders with their decoded fixed-function state without enabling the command/translator
        // firehose. Keep both halves of dual-source blending visible: a SRC1 factor is only meaningful when
        // the fragment module also declares an indexed second output.
        let dual_factor = desc.color_targets.iter().any(|target| {
            target.blend.as_ref().is_some_and(|blend| {
                [
                    blend.src_color,
                    blend.dst_color,
                    blend.src_alpha,
                    blend.dst_alpha,
                ]
                .into_iter()
                .any(|factor| {
                    matches!(
                        factor,
                        hl_gpu::protocol::model::enums::blend_factor::SRC1_COLOR
                            | hl_gpu::protocol::model::enums::blend_factor::ONE_MINUS_SRC1_COLOR
                            | hl_gpu::protocol::model::enums::blend_factor::SRC1_ALPHA
                            | hl_gpu::protocol::model::enums::blend_factor::ONE_MINUS_SRC1_ALPHA
                    )
                })
            })
        });
        let dual_output = pipe_key
            .fragment
            .as_ref()
            .and_then(|(key, _)| GlslDescriptor::from_words(&key.words))
            .and_then(|result| result.ok())
            .is_some_and(|shader| {
                shader.source.split("layout").skip(1).any(|tail| {
                    tail.split_once(')')
                        .is_some_and(|(qualifiers, _)| qualifiers.contains("index"))
                })
            });
        // Merge the vertex + fragment entry points' used resources into ONE reconciled layout description,
        // keyed by `(group, binding)`. For each slot: the `visibility` is the UNION of the stages that use
        // it (VERTEX and/or FRAGMENT), and the binding TYPE is reconciled across the two stages
        // (`reconcile_kind`). A genuine type collision (e.g. a buffer in one stage, a texture in the other)
        // is a real shader bug and is reported, not papered over. This SUBSUMES the old `layout: None` +
        // used-set filter: wgpu's auto-derivation fails outright when a binding's type is not consistent
        // between stages (Zed's `(group 1, binding 1)`), so we build the layout EXPLICITLY from this merge.
        let merge_started = diagnostics.then(Instant::now);
        let mut merged: BTreeMap<
            (u32, u32),
            (
                wgpu::ShaderStages,
                BindingKind,
                Option<std::num::NonZeroU32>,
            ),
        > = BTreeMap::new();
        for (stage, binds) in [
            (wgpu::ShaderStages::VERTEX, &vs_used),
            (wgpu::ShaderStages::FRAGMENT, &fs_used),
        ] {
            for b in binds {
                match merged.entry((b.group, b.binding)) {
                    std::collections::btree_map::Entry::Vacant(v) => {
                        v.insert((stage, b.kind, b.count));
                    }
                    std::collections::btree_map::Entry::Occupied(mut o) => {
                        let (vis, kind, count) = o.get_mut();
                        if *count != b.count {
                            return Err(GpuError::Invalid(
                                "wgpu: binding array count differs across shader stages",
                            ));
                        }
                        *vis |= stage;
                        *kind = BindingLayout::reconcile(*kind, b.kind).ok_or_else(|| {
                            hl_log::hl_warn!(
                                hl_log::tag::WGPU,
                                "pipeline rejected kind=render reason=binding-type-collision group={} binding={} vs={:?} fs={:?}",
                                b.group, b.binding, *kind, b.kind
                            );
                            GpuError::Invalid(
                                "wgpu: render pipeline binding declared with incompatible types across stages",
                            )
                        })?;
                    }
                }
            }
        }
        Self::apply_authoritative_counts(&mut merged, authoritative_layout)?;
        let merge_us = merge_started
            .map(|started| started.elapsed().as_micros())
            .unwrap_or_default();

        // The used `(group, binding)` set = the merged slots; the bind group `submit` builds at draw time is
        // filtered to these (see `PipelineNative::Render.used_bindings`), so it matches this explicit layout
        // EXACTLY even when the GL driver binds resources the compiled shader never samples.
        let used_bindings: Vec<(u32, u32)> = merged.keys().copied().collect();

        // Build one `BindGroupLayout` per group in `0..=max_group` (a gap group gets an empty layout, so the
        // `PipelineLayout`'s array stays dense from index 0), then the `PipelineLayout`. `submit` binds group
        // 0 at draw time (`get_bind_group_layout(0)`); a pipeline that uses a higher group reaches the layout
        // but its multi-group draw is the next gap, not an executor error here.
        let layout_started = diagnostics.then(Instant::now);
        let group_layouts = self.build_render_bind_group_layouts(&merged)?;
        let layout_refs: Vec<&wgpu::BindGroupLayout> = group_layouts.iter().collect();
        let mut push_constant_stages = wgpu::ShaderStages::empty();
        if vs_push_constant {
            push_constant_stages |= wgpu::ShaderStages::VERTEX;
        }
        if fs_push_constant {
            push_constant_stages |= wgpu::ShaderStages::FRAGMENT;
        }
        let push_constant_ranges = (!push_constant_stages.is_empty())
            .then(|| wgpu::PushConstantRange {
                stages: push_constant_stages,
                range: 0..self.gpu.device.limits().max_push_constant_size,
            })
            .into_iter()
            .collect::<Vec<_>>();
        let pipeline_layout =
            self.gpu
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("hl-render-pl"),
                    bind_group_layouts: &layout_refs,
                    push_constant_ranges: &push_constant_ranges,
                });
        let layout_us = layout_started
            .map(|started| started.elapsed().as_micros())
            .unwrap_or_default();

        // Vertex-buffer layouts: lower each protocol `VertexLayout` (stride + step mode + packed-format
        // attributes) into a `wgpu::VertexBufferLayout`. The attribute vecs must outlive the pipeline
        // descriptor, so they are materialized here and borrowed below. A guest that draws from
        // `@builtin(vertex_index)` (the conformance triangle) has no vertex buffers → an empty list.
        let state_started = diagnostics.then(Instant::now);
        let attr_sets: Vec<Vec<wgpu::VertexAttribute>> = desc
            .vertex_buffers
            .iter()
            .map(|vl| {
                vl.attrs
                    .iter()
                    .map(|a| {
                        Ok(wgpu::VertexAttribute {
                            format: VertexState::format(a.format)?,
                            offset: a.offset as u64,
                            shader_location: a.location,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let vbuffers: Vec<wgpu::VertexBufferLayout> = desc
            .vertex_buffers
            .iter()
            .zip(attr_sets.iter())
            .map(|(vl, attrs)| wgpu::VertexBufferLayout {
                array_stride: vl.stride as u64,
                step_mode: VertexState::step_mode(vl),
                attributes: attrs,
            })
            .collect();
        // A depth-stencil state (format + write-enable + compare) if the pipeline is depth-tested. The
        // opaque WebGPU compare code is mapped through the protocol's neutral `compare` constants, matching
        // the CPU oracle's per-fragment test. The pass this pipeline draws in must carry a matching depth
        // attachment (see `submit::run_render_pass`).
        let depth_stencil = desc.depth.as_ref().map(|ds| wgpu::DepthStencilState {
            format: Format::from(ds.format).native(),
            depth_write_enabled: ds.depth_write,
            depth_compare: CompareFunction(ds.depth_compare).native(),
            // Lower the protocol's real stencil state: front/back faces (compare + fail/depth-fail/pass
            // ops) + read/write masks. The neutral default (front+back `DISABLED` → `ALWAYS`+`KEEP`)
            // maps to `wgpu::StencilFaceState::IGNORE`, so `wgpu` sees the stencil as disabled and a
            // depth-only (`Depth32Float`) pipeline still validates exactly as before; a real stencil
            // pipeline requires (and here carries) the `Depth24PlusStencil8` stencil aspect.
            stencil: wgpu::StencilState {
                front: StencilState::face(&ds.stencil_front),
                back: StencilState::face(&ds.stencil_back),
                read_mask: ds.stencil_read_mask,
                write_mask: ds.stencil_write_mask,
            },
            bias: wgpu::DepthBiasState {
                constant: ds.bias_constant,
                slope_scale: ds.bias_slope_scale,
                clamp: ds.bias_clamp,
            },
        });

        let color_formats: Vec<TextureFormat> =
            desc.color_targets.iter().map(|c| c.format).collect();
        let mut targets: Vec<Option<wgpu::ColorTargetState>> = Vec::new();
        for c in &desc.color_targets {
            targets.push(Some(wgpu::ColorTargetState {
                format: Format::from(c.format).native(),
                // Honor the protocol's fixed-function blend: `Some(_)` lowers the GL `glBlendFunc`/
                // `GL_BLEND` (and Vulkan `VkPipelineColorBlendStateCreateInfo`) state into wgpu's
                // per-target blend so an overlapping translucent draw composites instead of overwriting;
                // `None` stays an opaque replace.
                blend: c.blend.as_ref().map(BlendState::lower),
                // Honor the protocol RGBA write mask (`glColorMask`): a masked channel is left untouched in
                // the target instead of the mask silently vanishing. `0xF` maps to `ALL` (the prior default).
                write_mask: ColorMask(c.write_mask).native(),
            }));
        }
        let state_us = state_started
            .map(|started| started.elapsed().as_micros())
            .unwrap_or_default();

        // A validation error scope turns wgpu's async device error (e.g. a shader that uses a binding the
        // explicit layout does not expose, or a stage/type the reconciliation could not satisfy) into a
        // clean typed error instead of the default uncaptured handler PANICKING on the wgpu thread — which
        // is exactly what marked the device lost and cost Zed its device before this explicit layout.
        let native_started = diagnostics.then(Instant::now);
        self.gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = self
            .gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("hl-render"),
                // Explicit layout: built from the reconciled union of the vertex + fragment entry points' used
                // bindings (types + stage visibility), so a binding whose type auto-derivation cannot merge
                // across stages no longer aborts pipeline creation. The bind group `submit` builds is filtered
                // to the same used set (`used_bindings`), so the two match even when the driver binds resources
                // the compiled shader never samples.
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &vs,
                    entry_point: Some(desc.vertex.entry.as_str()),
                    buffers: &vbuffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: PrimitiveTopology(desc.topology).native(),
                    // Honor the protocol face-culling + winding (`glCullFace`/`GL_CULL_FACE` + `glFrontFace`):
                    // the neutral wire defaults (cull 0 → None, front_face 0 → Ccw) reproduce the previous
                    // `..default()` byte-for-byte, while a real culling guest now actually discards the culled
                    // face instead of the state silently vanishing.
                    front_face: FrontFace(desc.front_face).native()?,
                    cull_mode: CullMode(desc.cull).native()?,
                    ..Default::default()
                },
                depth_stencil,
                // MSAA: rasterize at the descriptor's sample count. `1` (the neutral default) is
                // `MultisampleState::default()` (single-sampled) byte-for-byte; `> 1` (e.g. 4) builds a
                // multisampled pipeline that wgpu requires to draw into a color attachment of the SAME
                // sample count — its result is later averaged into a single-sample texture by `ResolveTexture`.
                multisample: wgpu::MultisampleState {
                    count: desc.sample_count.max(1),
                    mask: multisample.mask,
                    ..Default::default()
                },
                fragment: fs.as_ref().map(|(m, entry)| wgpu::FragmentState {
                    module: m,
                    entry_point: Some(entry.as_str()),
                    targets: &targets,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview: None,
                cache: None,
            });
        if let Some(e) = pollster::block_on(self.gpu.device.pop_error_scope()) {
            hl_log::hl_warn!(
                hl_log::tag::WGPU,
                "pipeline rejected kind=render reason=explicit-layout-invalid err={}",
                e
            );
            return Err(GpuError::Kernel(format!(
                "wgpu: render pipeline creation failed: {e}"
            )));
        }
        let native_us = native_started
            .map(|started| started.elapsed().as_micros())
            .unwrap_or_default();
        hl_log::hl_debug!(
            hl_log::tag::WGPU,
            "pipeline_create id={} targets={:?} dual_factor={} dual_output={} sample_count={} merge_us={} layout_us={} state_us={} native_us={} total_us={}",
            id,
            desc.color_targets,
            dual_factor,
            dual_output,
            desc.sample_count,
            merge_us,
            layout_us,
            state_us,
            native_us,
            total_started
                .map(|started| started.elapsed().as_micros())
                .unwrap_or_default()
        );
        // Register the freshly-compiled pipeline as a new shared backing (refcount 1) and store its backing
        // id on the per-id native. Insert the id FIRST (it may reject a duplicate id), then install the
        // backing, so a `DuplicateId` leaves no phantom refcount.
        let artifact = Artifact {
            pipeline: pipeline.clone(),
            color_formats: color_formats.clone(),
            used_bindings: used_bindings.clone(),
        };
        let backing = self.dedup.pipeline_install(
            pipe_key.clone(),
            pipeline.clone(),
            color_formats.clone(),
            used_bindings.clone(),
            crate::dedup::PIPELINE_BACKING_BYTES,
        );
        if let Err(e) = res.pipelines.insert(
            id,
            Box::new(PipelineNative::Render {
                pipeline,
                vertex_buffers: desc.vertex_buffers.clone(),
                color_formats,
                used_bindings,
                backing,
                texel: render_texel,
            }),
        ) {
            // The id was already live: undo the backing we just installed so residency does not drift, then
            // surface the duplicate. (The whole batch will also roll back, but keep this path self-consistent.)
            self.dedup.pipeline_release(backing);
            return Err(e);
        }
        self.pipeline_journal
            .push(Mutation::Install(pipe_key, artifact));
        Ok(())
    }

    /// Destroy a pipeline id, releasing its alias of the shared compiled backing. Reads the backing id
    /// BEFORE removing it, then
    /// removes (which may raise `UnknownId` for a double-free — propagated before any refcount change).
    pub(crate) fn destroy_pipeline(&mut self, res: &mut SessionResources, id: u32) -> Result<()> {
        let backing = match PipelineNative::get(res, id) {
            Ok(PipelineNative::Render { backing, .. }) => Some((*backing, false)),
            Ok(PipelineNative::Compute { backing, .. }) => Some((*backing, true)),
            _ => None,
        };
        res.pipelines.remove(id)?;
        if let Some((backing, compute)) = backing {
            if compute {
                self.dedup.compute_pipeline_release(backing);
            } else {
                self.dedup.pipeline_release(backing);
            }
        }
        Ok(())
    }

    /// Build the per-group `BindGroupLayout`s for a render pipeline from the reconciled `merged` binding map.
    /// One layout per group index in `0..=max_group`; a group with no bindings gets an EMPTY layout so the
    /// returned vec is dense from index 0 (a `PipelineLayout`'s `bind_group_layouts` array must have no
    /// gaps). An empty map ⇒ no layouts (a bindingless pipeline, e.g. the conformance triangle).
    pub(crate) fn build_render_bind_group_layouts(
        &self,
        merged: &BTreeMap<
            (u32, u32),
            (
                wgpu::ShaderStages,
                BindingKind,
                Option<std::num::NonZeroU32>,
            ),
        >,
    ) -> Result<Vec<wgpu::BindGroupLayout>> {
        let max_group = match merged.keys().map(|(g, _)| *g).max() {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };
        // A layout entry with a descriptor-array `count` needs a wgpu device feature this adapter may not
        // hold (Metal has no buffer/storage-resource binding arrays). `create_bind_group_layout` reports
        // that through the device's error handler, whose default PANICS the host — so capture it and return
        // a typed refusal instead. The advertised `binding_arrays` set is gated on the same features, so a
        // guest that negotiated honestly never lands here.
        self.gpu
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let layouts: Vec<wgpu::BindGroupLayout> = (0..=max_group)
            .map(|group| {
                let entries: Vec<wgpu::BindGroupLayoutEntry> = merged
                    .iter()
                    .filter(|((g, _), _)| *g == group)
                    .map(
                        |((_, binding), (visibility, kind, count))| wgpu::BindGroupLayoutEntry {
                            binding: *binding,
                            visibility: *visibility,
                            ty: if group == 0
                                && *binding == crate::wgsl::viewport::BINDING
                                && visibility.contains(wgpu::ShaderStages::VERTEX)
                            {
                                wgpu::BindingType::Buffer {
                                    ty: wgpu::BufferBindingType::Uniform,
                                    has_dynamic_offset: true,
                                    min_binding_size: std::num::NonZeroU64::new(16),
                                }
                            } else {
                                BindingLayout::binding_type(*kind)
                            },
                            count: *count,
                        },
                    )
                    .collect();
                self.gpu
                    .device
                    .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        label: Some("hl-render-bgl"),
                        entries: &entries,
                    })
            })
            .collect();
        if let Some(error) = pollster::block_on(self.gpu.device.pop_error_scope()) {
            hl_log::hl_warn!(
                hl_log::tag::WGPU,
                "bind-group layout rejected reason=device-cannot-express err={error}"
            );
            return Err(GpuError::Unsupported(
                "wgpu: bind-group layout is not expressible on this device",
            ));
        }
        Ok(layouts)
    }

    pub(crate) fn apply_authoritative_counts(
        merged: &mut BTreeMap<
            (u32, u32),
            (
                wgpu::ShaderStages,
                BindingKind,
                Option<std::num::NonZeroU32>,
            ),
        >,
        layout: Option<&PipelineLayout>,
    ) -> Result<()> {
        let Some(layout) = layout else {
            return Ok(());
        };
        for (&(group, binding), (_, reflected_kind, count)) in merged.iter_mut() {
            if group == 0 && binding == crate::wgsl::viewport::BINDING {
                continue;
            }
            let guest_binding = if group == 0 {
                binding
                    .checked_sub(crate::wgsl::viewport::GUEST_OFFSET)
                    .ok_or(GpuError::Invalid("wgpu: invalid shifted guest binding"))?
            } else {
                binding
            };
            let (declared, sampler_half) = layout
                .bindings
                .iter()
                .find(|item| item.group == group && item.binding == guest_binding)
                .map(|item| (item, false))
                .or_else(|| {
                    layout.bindings.iter().find_map(|item| {
                        (item.count > 1
                            && (1..item.count).any(|element| {
                                layout.scalar_binding(group, item.binding, element).ok()
                                    == Some(guest_binding)
                            }))
                        .then_some((item, false))
                    })
                })
                .or_else(|| {
                    guest_binding
                        .checked_sub(SAMPLER_BINDING_OFFSET)
                        .and_then(|source| {
                            layout
                                .bindings
                                .iter()
                                .find(|item| item.group == group && item.binding == source)
                                .map(|item| (item, true))
                        })
                })
                .ok_or(GpuError::Invalid(
                    "wgpu: shader binding absent from authoritative pipeline layout",
                ))?;
            let kind_matches = matches!(
                (declared.kind, *reflected_kind, sampler_half),
                (
                    PipelineBindingKind::UniformBuffer,
                    BindingKind::UniformBuffer,
                    false
                ) | (
                    PipelineBindingKind::StorageBuffer,
                    BindingKind::StorageBuffer { .. },
                    false,
                ) | (
                    PipelineBindingKind::UniformTexelBuffer,
                    BindingKind::StorageBuffer { read_only: true },
                    false,
                ) | (
                    PipelineBindingKind::StorageTexelBuffer,
                    BindingKind::StorageBuffer { .. },
                    false,
                ) | (
                    PipelineBindingKind::SampledTexture,
                    BindingKind::Texture { .. },
                    false,
                ) | (
                    PipelineBindingKind::StorageTexture,
                    BindingKind::StorageTexture { .. },
                    false,
                ) | (
                    PipelineBindingKind::Sampler,
                    BindingKind::Sampler { .. },
                    false
                ) | (
                    PipelineBindingKind::CombinedImageSampler,
                    BindingKind::Texture { .. },
                    false,
                ) | (
                    PipelineBindingKind::CombinedImageSampler,
                    BindingKind::Sampler { .. },
                    true,
                )
            );
            if !kind_matches {
                return Err(GpuError::Invalid(
                    "wgpu: shader binding kind differs from authoritative pipeline layout",
                ));
            }
            // Only a kind that survives as a NATIVE descriptor array keeps an array count. Uniform,
            // storage-buffer, and storage-image arrays are lowered to separate scalar bindings by
            // `wgsl::descriptor::ScalarArrays` (Metal exposes no buffer/storage resource arrays), so
            // re-attaching the authoritative count here would ask the device for an array the shader no
            // longer declares — a layout `create_bind_group_layout` refuses outright.
            *count = if declared.count > 1
                && !matches!(
                    declared.kind,
                    PipelineBindingKind::UniformBuffer
                        | PipelineBindingKind::StorageBuffer
                        | PipelineBindingKind::StorageTexture
                        | PipelineBindingKind::UniformTexelBuffer
                        | PipelineBindingKind::StorageTexelBuffer
                ) {
                std::num::NonZeroU32::new(declared.count)
            } else {
                None
            };
        }
        Ok(())
    }
}

mod layout;
mod native;
mod residency;
mod state;
mod texel;
mod vertex;

use layout::*;
pub use native::PipelineNative;
pub(crate) use residency::{Artifact, ComputeArtifact, Mutation, Residency};
use state::*;
use vertex::*;

#[cfg(test)]
#[path = "pipeline/explicit_layout.rs"]
mod explicit_layout_proof;
