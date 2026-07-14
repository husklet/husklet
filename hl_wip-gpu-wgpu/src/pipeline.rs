//! Native pipeline objects — a compiled `wgpu::RenderPipeline` or `wgpu::ComputePipeline`.
//!
//! A **compute** pipeline lowers its kernel's [`KernelProgram`] to WGSL (`wgsl::kernel_to_wgsl`), compiles
//! it, and builds a pipeline with an *auto* bind-group layout — so the storage bindings the WGSL declares
//! (param blob at 0, region `r` at `r+1`) are exactly the layout a bind group is later built against. A
//! **render** pipeline is assembled from the naga-translated graphics modules and the IR's color-target /
//! topology descriptors, again with an auto layout. Both use auto layouts because the protocol creates
//! bind groups independently of pipelines; the concrete `wgpu::BindGroup` is built at draw/dispatch time
//! from the bound pipeline's own layout (see `bindgroup.rs`).

use hl_gpu::protocol::model::descriptor::{ComputePipelineDesc, RenderPipelineDesc};
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

        // Vertex-buffer layouts: the conformance triangle draws from `@builtin(vertex_index)` with no
        // vertex buffers, so only the empty case is exercised. A non-empty layout would need the opaque
        // WebGPU VertexFormat map and is rejected honestly until a case needs it.
        if !desc.vertex_buffers.is_empty() {
            return Err(GpuError::Unsupported("wgpu: vertex-buffer layouts not yet lowered"));
        }

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
                buffers: &[],
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
