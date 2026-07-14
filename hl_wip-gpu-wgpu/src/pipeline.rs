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
use hl_gpu::protocol::model::enums::{TextureFormat, Topology};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

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
    /// A compute pipeline plus the *explicit* group-0 layout it was built with. The layout is explicit
    /// (not wgpu's auto layout) so a binding the WGSL happens not to read — e.g. the `params` blob in a
    /// kernel that only writes its output region — is still present, and a bind group built from the full
    /// protocol descriptor matches it (auto layouts drop unused bindings, which would mismatch).
    Compute { pipeline: wgpu::ComputePipeline, layout: wgpu::BindGroupLayout },
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
        let prog = match shader::native(res, desc.compute.module)? {
            ShaderNative::Kernel(p) => p.clone(),
            ShaderNative::Graphics(_) => {
                return Err(GpuError::Unsupported("wgpu: compute pipeline needs a kernel shader"))
            }
        };
        let src = wgsl::kernel_to_wgsl(&prog)?;
        let module = self.gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hl-kernel"),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });

        // Explicit group-0 layout matching the kernel ABI: binding 0 is the read-only param blob, binding
        // r+1 is pointer region r (read_write). Declaring every binding (even one the WGSL doesn't read)
        // keeps the bind group the protocol builds in lock-step with the layout.
        let mut entries = vec![storage_entry(0, true)];
        for r in 0..prog.num_regions {
            entries.push(storage_entry(r + 1, false));
        }
        let layout = self.gpu.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hl-compute-bgl"),
            entries: &entries,
        });
        let pipeline_layout =
            self.gpu.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("hl-compute-pl"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let pipeline = self.gpu.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("hl-compute"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some(desc.compute.entry.as_str()),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        res.pipelines.insert(id, Box::new(PipelineNative::Compute { pipeline, layout }))
    }

    pub(crate) fn create_render_pipeline(
        &self,
        res: &mut SessionResources,
        id: u32,
        desc: &RenderPipelineDesc,
    ) -> Result<()> {
        let vs = match shader::native(res, desc.vertex.module)? {
            ShaderNative::Graphics(m) => m.clone(),
            ShaderNative::Kernel(_) => {
                return Err(GpuError::Unsupported("wgpu: render pipeline vertex needs a graphics shader"))
            }
        };
        let fs = match &desc.fragment {
            Some(f) => match shader::native(res, f.module)? {
                ShaderNative::Graphics(m) => Some((m.clone(), f.entry.clone())),
                ShaderNative::Kernel(_) => {
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
            depth_stencil: None,
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
