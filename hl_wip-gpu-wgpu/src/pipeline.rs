//! Native pipeline objects — a compiled `wgpu::RenderPipeline` or `wgpu::ComputePipeline`.
//!
//! A **compute** pipeline lowers its kernel's [`KernelProgram`] to WGSL (`wgsl::kernel_to_wgsl`), compiles
//! it, and builds a pipeline with an *auto* bind-group layout — so the storage bindings the WGSL declares
//! (param blob at 0, region `r` at `r+1`) are exactly the layout a bind group is later built against. A
//! **render** pipeline is assembled from the naga-translated graphics modules and the IR's color-target /
//! topology descriptors, again with an auto layout. Both use auto layouts because the protocol creates
//! bind groups independently of pipelines; the concrete `wgpu::BindGroup` is built at draw/dispatch time
//! from the bound pipeline's own layout (see `bindgroup.rs`).

use hl_gpu::protocol::model::descriptor::{ComputePipelineDesc, RenderPipelineDesc, VertexLayout};
use hl_gpu::protocol::model::enums::{compare, TextureFormat, Topology};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};
use hl_log::tag;

use crate::convert::texture_format;
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

impl WgpuExecutor {
    pub(crate) fn create_compute_pipeline(
        &self,
        res: &mut SessionResources,
        id: u32,
        desc: &ComputePipelineDesc,
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(tag::WGPU, "pipeline_create");
        let pipeline = match shader::native(res, desc.compute.module)? {
            // PTX-kernel ABI: lower the neutral kernel IR to a WGSL compute entry point and build with an
            // EXPLICIT group-0 layout — binding 0 the read-only param blob, binding r+1 the read_write
            // pointer region r. Declaring every binding (even one the WGSL doesn't read) keeps the bind
            // group the protocol builds in lock-step with the layout that `get_bind_group_layout(0)` returns.
            ShaderNative::Kernel(p) => {
                let prog = p.clone();
                let src = wgsl::kernel_to_wgsl(&prog)?;
                let module = self.gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("hl-kernel"),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                });
                let mut entries = vec![storage_entry(0, true)];
                for r in 0..prog.num_regions {
                    entries.push(storage_entry(r + 1, false));
                }
                let layout =
                    self.gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                        label: Some("hl-compute-bgl"),
                        entries: &entries,
                    });
                let pipeline_layout =
                    self.gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                        label: Some("hl-compute-pl"),
                        bind_group_layouts: &[&layout],
                        push_constant_ranges: &[],
                    });
                self.gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
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
            ShaderNative::Module(m) => {
                let module = m.clone();
                // A validation error scope turns wgpu's async device error (raised when the module has no
                // compute entry point matching `entry` — e.g. a graphics-only SPIR-V module used for
                // compute) into a clean typed error, instead of the default uncaptured handler PANICKING.
                self.gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
                let pipeline =
                    self.gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some("hl-compute-spirv"),
                        layout: None,
                        module: &module,
                        entry_point: Some(desc.compute.entry.as_str()),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        cache: None,
                    });
                if let Some(e) = pollster::block_on(self.gpu.device.pop_error_scope()) {
                    hl_log::hl_warn!(
                        tag::WGPU,
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
        res.pipelines.insert(id, Box::new(PipelineNative::Compute { pipeline }))
    }

    pub(crate) fn create_render_pipeline(
        &self,
        res: &mut SessionResources,
        id: u32,
        desc: &RenderPipelineDesc,
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(tag::WGPU, "pipeline_create");
        let vs = match shader::native(res, desc.vertex.module)? {
            ShaderNative::Module(m) => m.clone(),
            ShaderNative::Kernel(_) => {
                hl_log::hl_warn!(tag::WGPU, "pipeline rejected kind=render stage=vertex reason=needs-graphics-shader");
                return Err(GpuError::Unsupported("wgpu: render pipeline vertex needs a graphics shader"))
            }
        };
        let fs = match &desc.fragment {
            Some(f) => match shader::native(res, f.module)? {
                ShaderNative::Module(m) => Some((m.clone(), f.entry.clone())),
                ShaderNative::Kernel(_) => {
                    hl_log::hl_warn!(tag::WGPU, "pipeline rejected kind=render stage=fragment reason=needs-graphics-shader");
                    return Err(GpuError::Unsupported(
                        "wgpu: render pipeline fragment needs a graphics shader",
                    ))
                }
            },
            None => None,
        };

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
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            None => None,
        };

        let color_formats: Vec<TextureFormat> = desc.color_targets.iter().map(|c| c.format).collect();
        let mut targets: Vec<Option<wgpu::ColorTargetState>> = Vec::new();
        for c in &desc.color_targets {
            targets.push(Some(wgpu::ColorTargetState {
                format: texture_format(c.format)?,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            }));
        }

        let pipeline = self.gpu.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hl-render"),
            layout: None, // auto
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: Some(desc.vertex.entry.as_str()),
                buffers: &vbuffers,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: topology(desc.topology),
                ..Default::default()
            },
            depth_stencil,
            multisample: wgpu::MultisampleState::default(),
            fragment: fs.as_ref().map(|(m, entry)| wgpu::FragmentState {
                module: m,
                entry_point: Some(entry.as_str()),
                targets: &targets,
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        });
        res.pipelines.insert(id, Box::new(PipelineNative::Render { pipeline, color_formats }))
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
        (1, 2) => if normalized { F::Unorm8x2 } else { F::Uint8x2 },
        (1, 4) => if normalized { F::Unorm8x4 } else { F::Uint8x4 },
        (2, 2) => if normalized { F::Snorm8x2 } else { F::Sint8x2 },
        (2, 4) => if normalized { F::Snorm8x4 } else { F::Sint8x4 },
        // 16-bit integer (x2 / x4 only), normalized → Unorm/Snorm else Uint/Sint
        (3, 2) => if normalized { F::Unorm16x2 } else { F::Uint16x2 },
        (3, 4) => if normalized { F::Unorm16x4 } else { F::Uint16x4 },
        (4, 2) => if normalized { F::Snorm16x2 } else { F::Sint16x2 },
        (4, 4) => if normalized { F::Snorm16x4 } else { F::Sint16x4 },
        _ => return Err(bad()),
    })
}
