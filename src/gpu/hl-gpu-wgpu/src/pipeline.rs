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

use hl_gpu::protocol::model::descriptor::{
    ComputePipelineDesc, RenderPipelineDesc, StencilFaceState, VertexLayout,
};
use hl_gpu::protocol::model::enums::{compare, stencil_op, TextureFormat, Topology};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::convert::texture_format;
use crate::reflect::{BindingKind, TexDim, TexSample};
use crate::shader::{self, ShaderNative};
use crate::wgsl;
use crate::WgpuExecutor;

/// The wgpu-native backing of one protocol pipeline.
pub enum PipelineNative {
    Render {
        pipeline: wgpu::RenderPipeline,
        /// The color-target formats the pipeline was built for — retained for draw-time attachment
        /// compatibility checks (the CPU oracle rejects a format mismatch); the frozen suite's single
        /// target already matches, so it is not yet consulted.
        #[allow(dead_code)]
        color_formats: Vec<TextureFormat>,
        /// The `(group, binding)` slots this pipeline's shaders actually READ — the union of its vertex +
        /// fragment entry points' usage ([`crate::reflect`]), which is exactly the set the EXPLICIT pipeline
        /// layout exposes (that layout is built from the same merge). A bind group `submit` builds is
        /// FILTERED to these bindings so the GL driver's per-bound-resource entries (which routinely include
        /// textures/samplers the compiled shader never samples) match the layout's set instead of NACKing
        /// (5-vs-3). Empty ⇒ no filtering (a bindingless pipeline, e.g. the conformance triangle).
        used_bindings: Vec<(u32, u32)>,
        /// The dedup-cache backing id this render pipeline aliases. Identical descriptors share one
        /// compiled `wgpu::RenderPipeline`; this is the handle a `DestroyPipeline` releases so the backing
        /// is freed only when its last alias is gone (see [`crate::dedup`]).
        backing: u64,
    },
    /// A compute pipeline. Both the PTX-kernel ABI path (built with an *explicit* group-0 layout so a
    /// binding the WGSL doesn't read — e.g. a kernel's `params` blob — is still declared) and the SPIR-V/
    /// GLSL path (built with wgpu's *auto* layout, which derives the bind-group layouts + push-constant
    /// range from the module) store just the pipeline: at dispatch the concrete per-group layout is taken
    /// from the pipeline itself via `get_bind_group_layout(index)`, which returns the explicit layout for
    /// the kernel path and the auto-derived one for the SPIR-V path — so a bind group built against it
    /// matches in both cases, and 2+ groups bind at their declared indices.
    Compute { pipeline: wgpu::ComputePipeline },
}

/// Downcast a live pipeline id to its native handle.
pub fn native<'a>(res: &'a SessionResources, id: u32) -> Result<&'a PipelineNative> {
    res.pipelines
        .get(id)?
        .downcast_ref::<PipelineNative>()
        .ok_or(GpuError::Invalid("wgpu: pipeline native type mismatch"))
}

/// A compute storage-buffer bind-group-layout entry at `binding` (`read_only` selects the param blob vs a
/// writable region), runtime-sized (`min_binding_size: None`) to match the WGSL `array<u32>` view.
fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn topology(t: Topology) -> wgpu::PrimitiveTopology {
    match t {
        Topology::PointList => wgpu::PrimitiveTopology::PointList,
        Topology::LineList => wgpu::PrimitiveTopology::LineList,
        Topology::LineStrip => wgpu::PrimitiveTopology::LineStrip,
        Topology::TriangleList => wgpu::PrimitiveTopology::TriangleList,
        Topology::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
    }
}

/// Map the protocol's `cull` code (`RenderPipelineDesc::cull`: 0 = none, 1 = front, 2 = back — the GL
/// driver's `glCullFace`/`GL_CULL_FACE` state) to wgpu's optional culled face. `0` (the neutral wire
/// default, and the only value the frozen suite used) is `None` — byte-for-byte the previous `..default()`
/// behavior; a real `glCullFace(GL_BACK)` guest now actually culls instead of the state silently vanishing.
fn cull_mode(cull: u32) -> Option<wgpu::Face> {
    match cull {
        1 => Some(wgpu::Face::Front),
        2 => Some(wgpu::Face::Back),
        _ => None,
    }
}

/// Map the protocol's `front_face` code (`RenderPipelineDesc::front_face`: 0 = CCW, 1 = CW — the GL
/// driver's `glFrontFace`) to a `wgpu::FrontFace`. `0` (the neutral default) is `Ccw`, identical to the
/// previous hardcoded default; it only changes an observable result together with a non-zero `cull`.
fn front_face(front_face: u32) -> wgpu::FrontFace {
    match front_face {
        1 => wgpu::FrontFace::Cw,
        _ => wgpu::FrontFace::Ccw,
    }
}

/// Map the protocol's RGBA `write_mask` (`ColorTargetState::write_mask`, low 4 bits `R<<0|G<<1|B<<2|A<<3` —
/// the GL driver's `glColorMask`) to `wgpu::ColorWrites`. `0xF` (the neutral default) is `ALL`, identical to
/// the previous hardcoded value; a guest that masks a channel (e.g. `glColorMask(1,1,1,0)` to preserve the
/// destination alpha) now actually leaves that channel untouched instead of the mask silently vanishing.
fn color_writes(mask: u32) -> wgpu::ColorWrites {
    let mut w = wgpu::ColorWrites::empty();
    if mask & 1 != 0 {
        w |= wgpu::ColorWrites::RED;
    }
    if mask & 2 != 0 {
        w |= wgpu::ColorWrites::GREEN;
    }
    if mask & 4 != 0 {
        w |= wgpu::ColorWrites::BLUE;
    }
    if mask & 8 != 0 {
        w |= wgpu::ColorWrites::ALPHA;
    }
    w
}

impl WgpuExecutor {
    pub(crate) fn create_compute_pipeline(
        &self,
        res: &mut SessionResources,
        id: u32,
        desc: &ComputePipelineDesc,
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(hl_log::tag::WGPU, "pipeline_create");
        let pipeline = match shader::native(res, desc.compute.module)? {
            // PTX-kernel ABI: lower the neutral kernel IR to a WGSL compute entry point and build with an
            // EXPLICIT group-0 layout — binding 0 the read-only param blob, binding r+1 the read_write
            // pointer region r. Declaring every binding (even one the WGSL doesn't read) keeps the bind
            // group the protocol builds in lock-step with the layout that `get_bind_group_layout(0)` returns.
            ShaderNative::Kernel(p) => {
                let prog = p.clone();
                let src = wgsl::kernel_to_wgsl(&prog)?;
                let module = self
                    .gpu
                    .device
                    .create_shader_module(wgpu::ShaderModuleDescriptor {
                        label: Some("hl-kernel"),
                        source: wgpu::ShaderSource::Wgsl(src.into()),
                    });
                let mut entries = vec![storage_entry(0, true)];
                for r in 0..prog.num_regions {
                    entries.push(storage_entry(r + 1, false));
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
                self.gpu
                    .device
                    .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some("hl-compute"),
                        layout: Some(&pipeline_layout),
                        module: &module,
                        entry_point: Some(desc.compute.entry.as_str()),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        cache: None,
                    })
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
            ShaderNative::Module { module: m, .. } => {
                let module = m.clone();
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
                            layout: None,
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
                pipeline
            }
        };
        res.pipelines
            .insert(id, Box::new(PipelineNative::Compute { pipeline }))
    }

    pub(crate) fn create_render_pipeline(
        &mut self,
        res: &mut SessionResources,
        id: u32,
        desc: &RenderPipelineDesc,
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(hl_log::tag::WGPU, "pipeline_create");
        // Clone the module + the used bindings (slot + type) of the entry point this pipeline binds out of
        // `res` (an immutable borrow), so the pipeline can be inserted (a mutable borrow) below carrying the
        // explicit layout's exact bindings for the draw-time bind-group filter (see `PipelineNative::Render`).
        // Also capture each stage module's CONTENT key so identical descriptors built from different shader
        // ids (but the same source) dedup to one compiled pipeline.
        let (vs, vs_used, vs_key) = match shader::native(res, desc.vertex.module)? {
            ShaderNative::Module {
                module,
                reflected,
                key,
            } => (
                module.clone(),
                reflected.used_for(&desc.vertex.entry).to_vec(),
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
        let (fs, fs_used, fs_key) = match &desc.fragment {
            Some(f) => {
                match shader::native(res, f.module)? {
                    ShaderNative::Module {
                        module,
                        reflected,
                        key,
                    } => (
                        Some((module.clone(), f.entry.clone())),
                        reflected.used_for(&f.entry).to_vec(),
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
            None => (None, Vec::new(), None),
        };

        // Content-dedup on the full pipeline identity: each stage's deduped shader CONTENT + entry point,
        // plus every fixed-function state field. An identical descriptor ALIASES the already-compiled
        // `wgpu::RenderPipeline` (a cheap `Arc` clone, ~0 incremental residency) and skips the naga merge +
        // layout build + PSO compile entirely. Distinct descriptors never share (full-value key compare).
        let pipe_key = crate::dedup::RenderPipeKey::from_desc(desc, vs_key, fs_key);
        if let Some((pipeline, color_formats, used_bindings, backing)) =
            self.dedup.pipeline_get(&pipe_key)
        {
            hl_log::hl_count!(hl_log::tag::WGPU, "pipeline_dedup_hit");
            return res.pipelines.insert(
                id,
                Box::new(PipelineNative::Render {
                    pipeline,
                    color_formats,
                    used_bindings,
                    backing,
                }),
            );
        }

        // Merge the vertex + fragment entry points' used resources into ONE reconciled layout description,
        // keyed by `(group, binding)`. For each slot: the `visibility` is the UNION of the stages that use
        // it (VERTEX and/or FRAGMENT), and the binding TYPE is reconciled across the two stages
        // (`reconcile_kind`). A genuine type collision (e.g. a buffer in one stage, a texture in the other)
        // is a real shader bug and is reported, not papered over. This SUBSUMES the old `layout: None` +
        // used-set filter: wgpu's auto-derivation fails outright when a binding's type is not consistent
        // between stages (Zed's `(group 1, binding 1)`), so we build the layout EXPLICITLY from this merge.
        let mut merged: BTreeMap<(u32, u32), (wgpu::ShaderStages, BindingKind)> = BTreeMap::new();
        for (stage, binds) in [
            (wgpu::ShaderStages::VERTEX, &vs_used),
            (wgpu::ShaderStages::FRAGMENT, &fs_used),
        ] {
            for b in binds {
                match merged.entry((b.group, b.binding)) {
                    std::collections::btree_map::Entry::Vacant(v) => {
                        v.insert((stage, b.kind));
                    }
                    std::collections::btree_map::Entry::Occupied(mut o) => {
                        let (vis, kind) = o.get_mut();
                        *vis |= stage;
                        *kind = reconcile_kind(*kind, b.kind).ok_or_else(|| {
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

        // The used `(group, binding)` set = the merged slots; the bind group `submit` builds at draw time is
        // filtered to these (see `PipelineNative::Render.used_bindings`), so it matches this explicit layout
        // EXACTLY even when the GL driver binds resources the compiled shader never samples.
        let used_bindings: Vec<(u32, u32)> = merged.keys().copied().collect();

        // Build one `BindGroupLayout` per group in `0..=max_group` (a gap group gets an empty layout, so the
        // `PipelineLayout`'s array stays dense from index 0), then the `PipelineLayout`. `submit` binds group
        // 0 at draw time (`get_bind_group_layout(0)`); a pipeline that uses a higher group reaches the layout
        // but its multi-group draw is the next gap, not an executor error here.
        let group_layouts = self.build_render_bind_group_layouts(&merged);
        let layout_refs: Vec<&wgpu::BindGroupLayout> = group_layouts.iter().collect();
        let pipeline_layout =
            self.gpu
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("hl-render-pl"),
                    bind_group_layouts: &layout_refs,
                    push_constant_ranges: &[],
                });

        // Vertex-buffer layouts: lower each protocol `VertexLayout` (stride + step mode + packed-format
        // attributes) into a `wgpu::VertexBufferLayout`. The attribute vecs must outlive the pipeline
        // descriptor, so they are materialized here and borrowed below. A guest that draws from
        // `@builtin(vertex_index)` (the conformance triangle) has no vertex buffers → an empty list.
        let attr_sets: Vec<Vec<wgpu::VertexAttribute>> = desc
            .vertex_buffers
            .iter()
            .map(|vl| {
                vl.attrs
                    .iter()
                    .map(|a| {
                        Ok(wgpu::VertexAttribute {
                            format: vertex_format(a.format)?,
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
                step_mode: step_mode(vl),
                attributes: attrs,
            })
            .collect();

        // A depth-stencil state (format + write-enable + compare) if the pipeline is depth-tested. The
        // opaque WebGPU compare code is mapped through the protocol's neutral `compare` constants, matching
        // the CPU oracle's per-fragment test. The pass this pipeline draws in must carry a matching depth
        // attachment (see `submit::run_render_pass`).
        let depth_stencil = match &desc.depth {
            Some(ds) => Some(wgpu::DepthStencilState {
                format: texture_format(ds.format)?,
                depth_write_enabled: ds.depth_write,
                depth_compare: compare_function(ds.depth_compare),
                // Lower the protocol's real stencil state: front/back faces (compare + fail/depth-fail/pass
                // ops) + read/write masks. The neutral default (front+back `DISABLED` → `ALWAYS`+`KEEP`)
                // maps to `wgpu::StencilFaceState::IGNORE`, so `wgpu` sees the stencil as disabled and a
                // depth-only (`Depth32Float`) pipeline still validates exactly as before; a real stencil
                // pipeline requires (and here carries) the `Depth24PlusStencil8` stencil aspect.
                stencil: wgpu::StencilState {
                    front: stencil_face(&ds.stencil_front),
                    back: stencil_face(&ds.stencil_back),
                    read_mask: ds.stencil_read_mask,
                    write_mask: ds.stencil_write_mask,
                },
                bias: wgpu::DepthBiasState::default(),
            }),
            None => None,
        };

        let color_formats: Vec<TextureFormat> =
            desc.color_targets.iter().map(|c| c.format).collect();
        let mut targets: Vec<Option<wgpu::ColorTargetState>> = Vec::new();
        for c in &desc.color_targets {
            targets.push(Some(wgpu::ColorTargetState {
                format: texture_format(c.format)?,
                // Honor the protocol's fixed-function blend: `Some(_)` lowers the GL `glBlendFunc`/
                // `GL_BLEND` (and Vulkan `VkPipelineColorBlendStateCreateInfo`) state into wgpu's
                // per-target blend so an overlapping translucent draw composites instead of overwriting;
                // `None` stays an opaque replace.
                blend: c.blend.as_ref().map(blend_state),
                // Honor the protocol RGBA write mask (`glColorMask`): a masked channel is left untouched in
                // the target instead of the mask silently vanishing. `0xF` maps to `ALL` (the prior default).
                write_mask: color_writes(c.write_mask),
            }));
        }

        // A validation error scope turns wgpu's async device error (e.g. a shader that uses a binding the
        // explicit layout does not expose, or a stage/type the reconciliation could not satisfy) into a
        // clean typed error instead of the default uncaptured handler PANICKING on the wgpu thread — which
        // is exactly what marked the device lost and cost Zed its device before this explicit layout.
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
                    topology: topology(desc.topology),
                    // Honor the protocol face-culling + winding (`glCullFace`/`GL_CULL_FACE` + `glFrontFace`):
                    // the neutral wire defaults (cull 0 → None, front_face 0 → Ccw) reproduce the previous
                    // `..default()` byte-for-byte, while a real culling guest now actually discards the culled
                    // face instead of the state silently vanishing.
                    front_face: front_face(desc.front_face),
                    cull_mode: cull_mode(desc.cull),
                    ..Default::default()
                },
                depth_stencil,
                // MSAA: rasterize at the descriptor's sample count. `1` (the neutral default) is
                // `MultisampleState::default()` (single-sampled) byte-for-byte; `> 1` (e.g. 4) builds a
                // multisampled pipeline that wgpu requires to draw into a color attachment of the SAME
                // sample count — its result is later averaged into a single-sample texture by `ResolveTexture`.
                multisample: wgpu::MultisampleState {
                    count: desc.sample_count.max(1),
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
        // Register the freshly-compiled pipeline as a new shared backing (refcount 1) and store its backing
        // id on the per-id native. Insert the id FIRST (it may reject a duplicate id), then install the
        // backing, so a `DuplicateId` leaves no phantom refcount.
        let backing = self.dedup.pipeline_install(
            pipe_key,
            pipeline.clone(),
            color_formats.clone(),
            used_bindings.clone(),
            crate::dedup::PIPELINE_BACKING_BYTES,
        );
        if let Err(e) = res.pipelines.insert(
            id,
            Box::new(PipelineNative::Render {
                pipeline,
                color_formats,
                used_bindings,
                backing,
            }),
        ) {
            // The id was already live: undo the backing we just installed so residency does not drift, then
            // surface the duplicate. (The whole batch will also roll back, but keep this path self-consistent.)
            self.dedup.pipeline_release(backing);
            return Err(e);
        }
        Ok(())
    }

    /// Destroy a pipeline id, releasing a render pipeline's alias of its shared compiled backing (a no-op
    /// for a compute pipeline, which is not deduped). Reads the id's backing id BEFORE removing it, then
    /// removes (which may raise `UnknownId` for a double-free — propagated before any refcount change).
    pub(crate) fn destroy_pipeline(&mut self, res: &mut SessionResources, id: u32) -> Result<()> {
        let backing = match native(res, id) {
            Ok(PipelineNative::Render { backing, .. }) => Some(*backing),
            _ => None,
        };
        res.pipelines.remove(id)?;
        if let Some(backing) = backing {
            self.dedup.pipeline_release(backing);
        }
        Ok(())
    }

    /// Build the per-group `BindGroupLayout`s for a render pipeline from the reconciled `merged` binding map.
    /// One layout per group index in `0..=max_group`; a group with no bindings gets an EMPTY layout so the
    /// returned vec is dense from index 0 (a `PipelineLayout`'s `bind_group_layouts` array must have no
    /// gaps). An empty map ⇒ no layouts (a bindingless pipeline, e.g. the conformance triangle).
    fn build_render_bind_group_layouts(
        &self,
        merged: &BTreeMap<(u32, u32), (wgpu::ShaderStages, BindingKind)>,
    ) -> Vec<wgpu::BindGroupLayout> {
        let max_group = match merged.keys().map(|(g, _)| *g).max() {
            Some(m) => m,
            None => return Vec::new(),
        };
        (0..=max_group)
            .map(|group| {
                let entries: Vec<wgpu::BindGroupLayoutEntry> = merged
                    .iter()
                    .filter(|((g, _), _)| *g == group)
                    .map(
                        |((_, binding), (visibility, kind))| wgpu::BindGroupLayoutEntry {
                            binding: *binding,
                            visibility: *visibility,
                            ty: binding_type(*kind),
                            count: None,
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
            .collect()
    }
}

/// Reconcile the binding TYPE the same `(group, binding)` slot declares in the two graphics stages. Equal
/// types pass through; two storage buffers merge to the WIDER access (writable subsumes read-only). Any
/// other disagreement (buffer vs texture, uniform vs storage, texture-shape mismatch, …) is a genuine
/// shader bug across stages and yields `None` so the caller reports it rather than guessing.
fn reconcile_kind(a: BindingKind, b: BindingKind) -> Option<BindingKind> {
    match (a, b) {
        (
            BindingKind::StorageBuffer { read_only: r1 },
            BindingKind::StorageBuffer { read_only: r2 },
        ) => Some(BindingKind::StorageBuffer {
            read_only: r1 && r2,
        }),
        _ if a == b => Some(a),
        _ => None,
    }
}

/// Lower a neutral [`BindingKind`] to the `wgpu::BindingType` a `BindGroupLayoutEntry` carries. Buffers use
/// `min_binding_size: None` (so a per-stage size disagreement never rejects the layout — the shader's own
/// access is validated against the module, not the layout) and no dynamic offset (the shim bakes offsets
/// into each `BindResource::Buffer.offset`).
fn binding_type(kind: BindingKind) -> wgpu::BindingType {
    match kind {
        BindingKind::UniformBuffer => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        BindingKind::StorageBuffer { read_only } => wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        BindingKind::Texture { dim, sample, multi } => wgpu::BindingType::Texture {
            sample_type: match sample {
                TexSample::Float { filterable } => wgpu::TextureSampleType::Float { filterable },
                TexSample::Sint => wgpu::TextureSampleType::Sint,
                TexSample::Uint => wgpu::TextureSampleType::Uint,
                TexSample::Depth => wgpu::TextureSampleType::Depth,
            },
            view_dimension: match dim {
                TexDim::D1 => wgpu::TextureViewDimension::D1,
                TexDim::D2 => wgpu::TextureViewDimension::D2,
                TexDim::D2Array => wgpu::TextureViewDimension::D2Array,
                TexDim::D3 => wgpu::TextureViewDimension::D3,
                TexDim::Cube => wgpu::TextureViewDimension::Cube,
                TexDim::CubeArray => wgpu::TextureViewDimension::CubeArray,
            },
            multisampled: multi,
        },
        BindingKind::Sampler { comparison } => wgpu::BindingType::Sampler(if comparison {
            wgpu::SamplerBindingType::Comparison
        } else {
            wgpu::SamplerBindingType::Filtering
        }),
    }
}

/// Map the protocol's opaque WebGPU depth-compare code (carried through the neutral [`compare`] constants,
/// Vulkan `VkCompareOp` ordering) to a `wgpu::CompareFunction`. An unrecognized code is treated as
/// `Always` — matching the CPU oracle's `compare::passes`, which never hard-fails a draw on a code it does
/// not model.
fn compare_function(code: u32) -> wgpu::CompareFunction {
    use wgpu::CompareFunction as C;
    match code {
        compare::NEVER => C::Never,
        compare::LESS => C::Less,
        compare::EQUAL => C::Equal,
        compare::LESS_EQUAL => C::LessEqual,
        compare::GREATER => C::Greater,
        compare::NOT_EQUAL => C::NotEqual,
        compare::GREATER_EQUAL => C::GreaterEqual,
        _ => C::Always, // compare::ALWAYS and any unmodeled code
    }
}

/// Map the protocol's opaque stencil-operation code (the neutral [`stencil_op`] numbering, Vulkan
/// `VkStencilOp` ordering) to a `wgpu::StencilOperation`. An unrecognized code is treated as `Keep`,
/// mirroring `compare_function`'s `Always` fallback — an honest bring-up never hard-fails on a code it does
/// not model, it just leaves the stencil untouched.
fn stencil_operation(code: u32) -> wgpu::StencilOperation {
    use wgpu::StencilOperation as S;
    match code {
        stencil_op::ZERO => S::Zero,
        stencil_op::REPLACE => S::Replace,
        stencil_op::INCREMENT_CLAMP => S::IncrementClamp,
        stencil_op::DECREMENT_CLAMP => S::DecrementClamp,
        stencil_op::INVERT => S::Invert,
        stencil_op::INCREMENT_WRAP => S::IncrementWrap,
        stencil_op::DECREMENT_WRAP => S::DecrementWrap,
        _ => S::Keep, // stencil_op::KEEP and any unmodeled code
    }
}

/// Lower one protocol [`StencilFaceState`] (opaque compare + the three stencil ops) into a
/// `wgpu::StencilFaceState`. Front+back both `DISABLED` collapse to `wgpu::StencilFaceState::IGNORE`.
fn stencil_face(f: &StencilFaceState) -> wgpu::StencilFaceState {
    wgpu::StencilFaceState {
        compare: compare_function(f.compare),
        fail_op: stencil_operation(f.fail_op),
        depth_fail_op: stencil_operation(f.depth_fail_op),
        pass_op: stencil_operation(f.pass_op),
    }
}

/// Decode a protocol blend-factor wire value into a `wgpu::BlendFactor`. The wire numbering is the neutral
/// one the GL driver emits from `glBlendFunc`/`glBlendFuncSeparate` (`hl-gl` `blend_factor_wire`):
/// 0=ZERO 1=ONE 2=SRC_COLOR 3=1-SRC_COLOR 4=SRC_ALPHA 5=1-SRC_ALPHA 6=DST_COLOR 7=1-DST_COLOR
/// 8=DST_ALPHA 9=1-DST_ALPHA 10=SRC_ALPHA_SATURATE 11=CONSTANT 12=1-CONSTANT. Every value the protocol can carry maps to a concrete
/// wgpu factor; an unmodeled code defaults to `One` (matching the GL driver's own fallback) rather than
/// silently dropping the blend.
fn blend_factor(code: u32) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as F;
    match code {
        0 => F::Zero,
        1 => F::One,
        2 => F::Src,
        3 => F::OneMinusSrc,
        4 => F::SrcAlpha,
        5 => F::OneMinusSrcAlpha,
        6 => F::Dst,
        7 => F::OneMinusDst,
        8 => F::DstAlpha,
        9 => F::OneMinusDstAlpha,
        10 => F::SrcAlphaSaturated,
        11 => F::Constant,
        12 => F::OneMinusConstant,
        _ => F::One,
    }
}

/// Decode a protocol blend-op wire value into a `wgpu::BlendOperation`. The wire numbering is the neutral
/// one the GL driver emits from `glBlendEquation` (`hl-gl` `blend_op_wire`): 0=ADD 1=SUBTRACT
/// 2=REVERSE_SUBTRACT 3=MIN 4=MAX. An unmodeled code defaults to `Add`.
fn blend_operation(code: u32) -> wgpu::BlendOperation {
    use wgpu::BlendOperation as O;
    match code {
        1 => O::Subtract,
        2 => O::ReverseSubtract,
        3 => O::Min,
        4 => O::Max,
        _ => O::Add,
    }
}

/// Lower a protocol [`BlendState`] into a `wgpu::BlendState`, translating the separate color/alpha
/// src+dst factors and equations. A target whose protocol blend is `None` is an opaque replace, which
/// wgpu represents as `blend: None` on the color target.
fn blend_state(b: &hl_gpu::protocol::model::descriptor::BlendState) -> wgpu::BlendState {
    wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: blend_factor(b.src_color),
            dst_factor: blend_factor(b.dst_color),
            operation: blend_operation(b.op_color),
        },
        alpha: wgpu::BlendComponent {
            src_factor: blend_factor(b.src_alpha),
            dst_factor: blend_factor(b.dst_alpha),
            operation: blend_operation(b.op_alpha),
        },
    }
}

/// Per-slot vertex step mode: `step_mode == 0` steps per-vertex, non-zero steps per-instance (the encoding
/// the GL driver emits from `glVertexAttribDivisor`).
fn step_mode(vl: &VertexLayout) -> wgpu::VertexStepMode {
    if vl.step_mode == 0 {
        wgpu::VertexStepMode::Vertex
    } else {
        wgpu::VertexStepMode::Instance
    }
}

/// Decode a protocol vertex-attribute format into a `wgpu::VertexFormat`. The wire packs
/// `comps | (kind<<8) | (normalized<<16) | (integer<<17)` (the GL driver's `vertex_format_wire`): `comps`
/// in 1..=4, `kind` 0=f32 1=u8 2=i8 3=u16 4=i16 5=u32 6=i32 7=f16. WebGPU has no 1-/3-component 8-/16-bit
/// formats, so those combinations are rejected honestly rather than silently widened.
pub(crate) fn vertex_format(packed: u32) -> Result<wgpu::VertexFormat> {
    use wgpu::VertexFormat as F;
    let comps = packed & 0xff;
    let kind = (packed >> 8) & 0xff;
    let normalized = (packed >> 16) & 1 != 0;
    let bad = || GpuError::Unsupported("wgpu: unsupported vertex attribute format");
    Ok(match (kind, comps) {
        // 32-bit float
        (0, 1) => F::Float32,
        (0, 2) => F::Float32x2,
        (0, 3) => F::Float32x3,
        (0, 4) => F::Float32x4,
        // 32-bit unsigned / signed integer
        (5, 1) => F::Uint32,
        (5, 2) => F::Uint32x2,
        (5, 3) => F::Uint32x3,
        (5, 4) => F::Uint32x4,
        (6, 1) => F::Sint32,
        (6, 2) => F::Sint32x2,
        (6, 3) => F::Sint32x3,
        (6, 4) => F::Sint32x4,
        // 16-bit float (x2 / x4 only)
        (7, 2) => F::Float16x2,
        (7, 4) => F::Float16x4,
        // 8-bit (x2 / x4 only), normalized → Unorm/Snorm else Uint/Sint
        (1, 2) => {
            if normalized {
                F::Unorm8x2
            } else {
                F::Uint8x2
            }
        }
        (1, 4) => {
            if normalized {
                F::Unorm8x4
            } else {
                F::Uint8x4
            }
        }
        (2, 2) => {
            if normalized {
                F::Snorm8x2
            } else {
                F::Sint8x2
            }
        }
        (2, 4) => {
            if normalized {
                F::Snorm8x4
            } else {
                F::Sint8x4
            }
        }
        // 16-bit integer (x2 / x4 only), normalized → Unorm/Snorm else Uint/Sint
        (3, 2) => {
            if normalized {
                F::Unorm16x2
            } else {
                F::Uint16x2
            }
        }
        (3, 4) => {
            if normalized {
                F::Unorm16x4
            } else {
                F::Uint16x4
            }
        }
        (4, 2) => {
            if normalized {
                F::Snorm16x2
            } else {
                F::Sint16x2
            }
        }
        (4, 4) => {
            if normalized {
                F::Snorm16x4
            } else {
                F::Sint16x4
            }
        }
        _ => return Err(bad()),
    })
}

#[cfg(test)]
mod explicit_layout_proof {
    //! The Zed render-pipeline unblock, FAIL-before / PASS-after in one test.
    //!
    //! Zed's GPUI/wgpu renderer builds a render pipeline whose vertex + fragment stages BOTH declare the
    //! same `(group, binding)` UBO but with DIFFERENT block layouts (the fragment reaches a member at a
    //! higher offset), so each stage's naga usage derives a different `min_binding_size` for that buffer.
    //! wgpu's AUTO layout (`layout: None`) merges the per-stage derived layouts and, seeing two different
    //! derived types for one binding, aborts with `InconsistentlyDerivedType` — "Derived bind group layout
    //! type is not consistent between stages". That validation error was UNCAPTURED on the executor's wgpu
    //! thread, panicked it, and cost Zed its device.
    //!
    //! `create_render_pipeline` now builds an EXPLICIT layout from the reconciled union of the two stages'
    //! used bindings (visibility = VERTEX|FRAGMENT, buffer `min_binding_size: None` so the per-stage size
    //! disagreement no longer collides), so the pipeline creates, a bind group of exactly the used entries
    //! matches it, a draw runs, and the readback carries the sampled texel — the pixel a mis-built pipeline
    //! could never produce.

    use hl_gpu::protocol::model::descriptor::{
        BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
        RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
    };
    use hl_gpu::protocol::model::enums::{
        buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat,
        Topology,
    };
    use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
    use hl_gpu::{
        Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
        ShaderPayloadKind,
    };

    use crate::{DeviceConfig, WgpuExecutor};

    // Vertex: declares binding 0 as a ONE-vec4 block (16 bytes) → naga derives a 16-byte min size.
    const VS: &str = r#"#version 460
layout(std140, binding = 0) uniform U { vec4 scale; } u;
layout(location = 0) out vec2 uv;
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    uv = vec2(0.5, 0.5);
    gl_Position = vec4(p[gl_VertexIndex], 0.0, u.scale.w);
}
"#;

    // Fragment: declares binding 0 as a TWO-vec4 block (32 bytes) → naga derives a 32-byte min size,
    // DIFFERENT from the vertex stage's 16 — the inconsistency wgpu's auto-derive cannot merge. It also
    // samples a texture (binding 1) through a sampler (binding 2), the fragment-only part of the union.
    const FS: &str = r#"#version 460
layout(std140, binding = 0) uniform U { vec4 scale; vec4 tint; } u;
layout(binding = 1) uniform texture2D t0_tex;
layout(binding = 2) uniform sampler   t0_smp;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;
void main() {
    color = texture(sampler2D(t0_tex, t0_smp), uv) * u.tint;
}
"#;

    fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
        GlslDescriptor {
            stage,
            entry: entry.to_string(),
            source: source.to_string(),
        }
        .to_words()
    }

    fn tex(w: u32, h: u32, usage: u32) -> TextureDesc {
        TextureDesc {
            width: w,
            height: h,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage,
            label: String::new(),
        }
    }

    fn nearest() -> SamplerDesc {
        SamplerDesc {
            min_filter: Filter::Nearest,
            mag_filter: Filter::Nearest,
            mip_filter: Filter::Nearest,
            address_u: AddressMode::ClampToEdge,
            address_v: AddressMode::ClampToEdge,
            address_w: AddressMode::ClampToEdge,
        }
    }

    /// FAIL-before: raw wgpu auto-derive (`layout: None`) over the two stages' translated WGSL rejects the
    /// pipeline because binding 0's derived type is inconsistent between the stages. Returns the wgpu error
    /// text so the test can assert it is the cross-stage inconsistency (not some unrelated failure).
    fn autoderive_error(exec: &WgpuExecutor) -> Option<String> {
        let dev = &exec.gpu.device;
        let vs_wgsl =
            crate::wgsl::glsl_to_wgsl(VS, naga::ShaderStage::Vertex, "vmain").expect("vs wgsl");
        let fs_wgsl =
            crate::wgsl::glsl_to_wgsl(FS, naga::ShaderStage::Fragment, "fmain").expect("fs wgsl");
        let vs = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(vs_wgsl.into()),
        });
        let fs = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(fs_wgsl.into()),
        });
        let targets = [Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];
        dev.push_error_scope(wgpu::ErrorFilter::Validation);
        let _p = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None, // the OLD path — auto-derive
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: Some("vmain"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: Some("fmain"),
                targets: &targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        });
        pollster::block_on(dev.pop_error_scope()).map(|e| e.to_string())
    }

    #[test]
    fn cross_stage_inconsistent_binding_builds_via_explicit_layout() {
        let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
            Ok(e) => e,
            // No adapter (no lavapipe/Vulkan ICD reachable) — skip, mirroring the suite's other gpu tests.
            Err(_) => return,
        };

        // FAIL-BEFORE: the old auto-derive path rejects this exact stage pair as inconsistent.
        let err = autoderive_error(&exec).expect(
            "auto-derive (layout: None) MUST reject binding 0 as inconsistent between stages — if it did \
             not, this test no longer reproduces the Zed pipeline gap",
        );
        assert!(
            err.contains("consistent") || err.contains("Derived bind group"),
            "the auto-derive failure must be the cross-stage inconsistency, got: {err}"
        );

        // PASS-AFTER: the executor's explicit-layout path creates the pipeline, matches a bind group of the
        // used entries, draws, and reads back the sampled texel.
        let texel: [u8; 4] = [30, 150, 220, 255]; // texture-0's single texel
        let ubo: [f32; 8] = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]; // scale=(…), tint=(1,1,1,1) → passthrough

        let caps = exec.capabilities();
        let mut limits = Limits::from_capabilities(caps);
        limits.copy_alignment = 1;
        let mut s = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );

        hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[
                Cmd::CreateTexture(1, tex(4, 4, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
                Cmd::CreateTexture(2, tex(1, 1, texture_usage::SAMPLED | texture_usage::COPY_DST)),
                Cmd::CreateBuffer(1, BufferDesc { size: 32, usage: buffer_usage::UNIFORM, label: String::new() }),
                Cmd::WriteBuffer { id: 1, offset: 0, data: ubo.iter().flat_map(|f| f.to_le_bytes()).collect() },
                Cmd::CreateBuffer(2, BufferDesc { size: 4, usage: buffer_usage::COPY_SRC, label: String::new() }),
                Cmd::WriteBuffer { id: 2, offset: 0, data: texel.to_vec() },
                Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", VS) },
                Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS) },
                Cmd::CreateSampler(1, nearest()),
                Cmd::CreateRenderPipeline(
                    1,
                    RenderPipelineDesc {
                        vertex: ShaderRef { module: 1, entry: "vmain".into() },
                        fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
                        vertex_buffers: vec![],
                        color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                        depth: None,
                        topology: Topology::TriangleList,
                        cull: 0,
                        front_face: 0,
                        sample_count: 1,
                        label: String::new(),
                    },
                ),
                // The bind group = exactly the used union {0,1,2}: UBO@0 (used by BOTH stages), texture@1
                // and sampler@2 (fragment). This matches the explicit reconciled layout entry-for-entry.
                Cmd::CreateBindGroup(
                    1,
                    BindGroupDesc {
                        set: 0,
                        entries: vec![
                            BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 32 } },
                            BindEntry { binding: 1, resource: BindResource::Texture { id: 2 } },
                            BindEntry { binding: 2, resource: BindResource::Sampler { id: 1 } },
                        ],
                    },
                ),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![
                        Enc::CopyBufferToTexture { src: 2, src_offset: 0, bytes_per_row: 4, dst: 2, mip: 0, width: 1, height: 1 },
                        Enc::BeginRenderPass {
                            color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                            depth: None,
                        },
                        Enc::SetPipeline(1),
                        Enc::SetBindGroup { index: 0, group: 1 },
                        Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                        Enc::EndRenderPass,
                    ],
                    signal: None,
                }),
            ],
        )
        .expect(
            "the cross-stage-inconsistent binding must create + draw cleanly through the explicit reconciled \
             layout (the exact pipeline auto-derive rejected above)",
        );

        let px = exec.read_texture(&s.resources, 1).unwrap();
        for (i, out) in px.chunks_exact(4).enumerate() {
            assert_eq!(
                out, texel,
                "pixel {i}: must be the sampled texture-0 texel {texel:?} (tint is white), proving the \
                 explicit-layout pipeline drew and the bind group matched its layout"
            );
        }
    }
}
