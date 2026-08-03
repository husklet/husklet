//! Scaled/filtered texture→texture blit by RENDERING (`Enc::BlitTexture`).
//!
//! wgpu has NO native image blit, so a scaling `glBlitFramebuffer` (source extent != destination extent)
//! cannot lower to a copy. This module implements it the only way wgpu allows: it SAMPLES the source
//! texture with the requested filter (`GL_LINEAR` → linear, `GL_NEAREST` → nearest) through a full-viewport
//! textured-triangle draw whose viewport+scissor are the destination rect, so the rasterizer resamples the
//! `src_extent` source region into the `dst_extent` destination region. This reproduces the CPU oracle's
//! `blit_texture` (`hl-gpu/src/cpu/service/copy.rs`): for a dest texel `(dx, dy)` the sample point is
//! the pixel-CENTER source coordinate `src_origin + (d + 0.5) * src_extent / dst_extent`, which is exactly
//! what a viewport-mapped fullscreen triangle with clamp-to-edge sampling produces.
//!
//! A MIRRORED blit needs no new capability here. The shader's remap is a plain `uv_off + uv * uv_scale`
//! with nothing constraining the sign, so a flipped axis is the origin moved to the far edge of the source
//! rect and the scale negated; clamp-to-edge handles the boundary exactly as it does unmirrored.
//!
//! The blit pipeline/sampler are CACHED on the executor (`BlitCache`): the WGSL module + bind-group layout
//! + the two samplers are built once, and one render pipeline per `(dst format, filter)` is memoized — a
//!
//! GL app blits constantly, so a per-call rebuild would be wasteful. The concrete source-texture bind group
//! + a 16-byte UV-transform uniform are the only per-call allocations.

use std::collections::HashMap;

use hl_gpu::protocol::model::descriptor::{Extent3d, Mirror, Origin3d, TextureSubresource};
use hl_gpu::protocol::model::enums::{Filter, TextureAspect, TextureNumericClass};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::convert::Format;
use crate::{texture, WgpuExecutor};

/// The blit shader. A fullscreen triangle whose interpolated `uv` runs `0..1` across the VIEWPORT (which
/// `submit` sets to the destination rect), then remapped by the per-call `uv_off`/`uv_scale` uniform into
/// the source rect's normalized texture coordinates and sampled with the bound filter. `uv.y` is flipped so
/// the top destination row samples the top source row (row 0 = origin y, matching the oracle's orientation).
const BLIT_WGSL: &str = r#"
struct XformUniform {
    uv_off: vec2<f32>,
    uv_scale: vec2<f32>,
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_smp: sampler;
@group(0) @binding(2) var<uniform> xform: XformUniform;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let ndc = p[vi];
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    // uv = 0 at the viewport's top-left, 1 at its bottom-right (y flipped: framebuffer y grows downward).
    out.uv = vec2<f32>((ndc.x + 1.0) * 0.5, (1.0 - ndc.y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = xform.uv_off + in.uv * xform.uv_scale;
    return textureSample(src_tex, src_smp, uv);
}
"#;

/// D3 source variant. Each destination slice gets its own normalized source-z coordinate while x/y
/// continue to come from the viewport. Sampling the volume (rather than a copied D2 source slice) is
/// load-bearing for `Filter::Linear`: Vulkan requires interpolation between adjacent z slices.
const BLIT_3D_WGSL: &str = r#"
struct XformUniform {
    uv_off: vec2<f32>,
    uv_scale: vec2<f32>,
    z: f32,
    _pad: vec3<f32>,
};

@group(0) @binding(0) var src_tex: texture_3d<f32>;
@group(0) @binding(1) var src_smp: sampler;
@group(0) @binding(2) var<uniform> xform: XformUniform;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let ndc = p[vi];
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = vec2<f32>((ndc.x + 1.0) * 0.5, (1.0 - ndc.y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = xform.uv_off + in.uv * xform.uv_scale;
    return textureSample(src_tex, src_smp, vec3<f32>(uv, xform.z));
}
"#;

const INTEGER_BLIT_WGSL: &str = r#"
struct Xform { uv_off: vec2<f32>, uv_scale: vec2<f32> };
struct Out { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs_main(@builtin(vertex_index) i:u32)->Out { var p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.)); let n=p[i]; var o:Out; o.pos=vec4(n,0.,1.); o.uv=vec2((n.x+1.)*.5,(1.-n.y)*.5); return o; }
@group(0) @binding(0) var utex:texture_2d<u32>; @group(0) @binding(1) var<uniform> x:Xform;
@fragment fn fs_uint(i:Out)->@location(0) vec4<u32> { let s=textureDimensions(utex); let p=clamp(vec2<i32>((x.uv_off+i.uv*x.uv_scale)*vec2<f32>(s)),vec2(0),vec2<i32>(s)-vec2(1)); return textureLoad(utex,p,0); }
@group(0) @binding(2) var itex:texture_2d<i32>;
@fragment fn fs_sint(i:Out)->@location(0) vec4<i32> { let s=textureDimensions(itex); let p=clamp(vec2<i32>((x.uv_off+i.uv*x.uv_scale)*vec2<f32>(s)),vec2(0),vec2<i32>(s)-vec2(1)); return textureLoad(itex,p,0); }
"#;

/// Cached, device-lifetime blit objects. The module + bind-group layout + samplers are built once; a render
/// pipeline is memoized per destination `wgpu::TextureFormat` (a pipeline is bound to its color-target
/// format). The FILTER keys the SAMPLER, not the pipeline — a single filtering sampler-binding-type serves
/// both nearest and linear — so the pipeline map keys on format alone.
pub(crate) struct BlitCache {
    module: wgpu::ShaderModule,
    /// Indexed by [`Filterable`]: the layout declaring a filterable float source, and the one declaring a
    /// non-filterable float source with a non-filtering sampler.
    bind_group_layout: [wgpu::BindGroupLayout; 2],
    pipeline_layout: [wgpu::PipelineLayout; 2],
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
    /// A non-filtering sampler, which is the only kind a `NonFiltering` binding accepts.
    non_filtering: wgpu::Sampler,
    pipelines: HashMap<(wgpu::TextureFormat, bool), wgpu::RenderPipeline>,
    d3_module: wgpu::ShaderModule,
    d3_bind_group_layout: [wgpu::BindGroupLayout; 2],
    d3_pipeline_layout: [wgpu::PipelineLayout; 2],
    d3_pipelines: HashMap<(wgpu::TextureFormat, bool), wgpu::RenderPipeline>,
    integer_module: wgpu::ShaderModule,
    integer_layouts: [wgpu::BindGroupLayout; 2],
    integer_pipeline_layouts: [wgpu::PipelineLayout; 2],
    integer_pipelines: HashMap<(wgpu::TextureFormat, bool), wgpu::RenderPipeline>,
}

/// Whether a source format can be SAMPLED with filtering on this device.
///
/// One of three independent layers that decline a linear filter on the 32-bit float formats; the others
/// are the software reference's `FILTERABLE_REFUSED` and the Vulkan surface's `FILTERABLE`. See
/// `tests/float_filter_agreement.rs`, which binds all three and records why `FLOAT32_FILTERABLE` stays
/// unrequested — the adapter measured offers it, so this is a decision rather than an absence.
///
/// WebGPU makes the 32-bit float formats non-filterable unless `FLOAT32_FILTERABLE` is enabled, and a
/// bind group whose layout says `Float { filterable: true }` cannot take such a view at all — which is
/// why this matters for NEAREST too. The blit declared filterable unconditionally, so a blit whose source
/// was `R32Float` or `Rgba32Float` failed at bind-group creation with `InvalidTextureSampleType`
/// regardless of the filter, although a nearest blit does no filtering and needs none of it.
fn filterable(format: hl_gpu::protocol::model::enums::TextureFormat) -> bool {
    use hl_gpu::protocol::model::enums::TextureFormat as F;
    !matches!(format, F::R32Float | F::Rgba32Float)
}

fn integer(format: hl_gpu::protocol::model::enums::TextureFormat) -> bool {
    format.numeric_class() != TextureNumericClass::Float
}

fn signed_integer(format: hl_gpu::protocol::model::enums::TextureFormat) -> bool {
    format.numeric_class() == TextureNumericClass::Sint
}

impl BlitCache {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hl-blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        // Two layouts, because the source's sample type is part of the layout and a non-filterable float
        // view cannot bind to a filterable declaration. Built as a pair rather than lazily so the choice
        // is visible in one place.
        let layout_for = |can_filter: bool| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(if can_filter {
                    "hl-blit-bgl"
                } else {
                    "hl-blit-bgl-nonfilterable"
                }),
                entries: &[
                    // 0: the source texture (float 2D, filterable or not).
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float {
                                filterable: can_filter,
                            },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    // 1: the sampler. A non-filterable source admits only a NonFiltering sampler.
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(if can_filter {
                            wgpu::SamplerBindingType::Filtering
                        } else {
                            wgpu::SamplerBindingType::NonFiltering
                        }),
                        count: None,
                    },
                    // 2: the uv_off / uv_scale transform (16 bytes).
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            })
        };
        let bind_group_layout = [layout_for(true), layout_for(false)];
        let pipeline_layout = [
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("hl-blit-pl"),
                bind_group_layouts: &[&bind_group_layout[0]],
                push_constant_ranges: &[],
            }),
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("hl-blit-pl-nonfilterable"),
                bind_group_layouts: &[&bind_group_layout[1]],
                push_constant_ranges: &[],
            }),
        ];
        let sampler = |mode: wgpu::FilterMode, label: &str| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: mode,
                min_filter: mode,
                mipmap_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            })
        };
        let d3_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hl-blit-3d"),
            source: wgpu::ShaderSource::Wgsl(BLIT_3D_WGSL.into()),
        });
        let d3_layout_for = |can_filter: bool| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("hl-blit-3d-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float {
                                filterable: can_filter,
                            },
                            view_dimension: wgpu::TextureViewDimension::D3,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(if can_filter {
                            wgpu::SamplerBindingType::Filtering
                        } else {
                            wgpu::SamplerBindingType::NonFiltering
                        }),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            })
        };
        let d3_bind_group_layout = [d3_layout_for(true), d3_layout_for(false)];
        let d3_pipeline_layout = [0, 1].map(|i| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("hl-blit-3d-pl"),
                bind_group_layouts: &[&d3_bind_group_layout[i]],
                push_constant_ranges: &[],
            })
        });
        let integer_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hl-integer-blit"),
            source: wgpu::ShaderSource::Wgsl(INTEGER_BLIT_WGSL.into()),
        });
        let integer_layout = |signed: bool| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("hl-integer-blit-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: if signed { 2 } else { 0 },
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: if signed {
                                wgpu::TextureSampleType::Sint
                            } else {
                                wgpu::TextureSampleType::Uint
                            },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            })
        };
        let integer_layouts = [integer_layout(false), integer_layout(true)];
        let integer_pipeline_layouts = [false, true].map(|signed| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("hl-integer-blit-pl"),
                bind_group_layouts: &[&integer_layouts[usize::from(signed)]],
                push_constant_ranges: &[],
            })
        });
        Self {
            module,
            bind_group_layout,
            pipeline_layout,
            nearest: sampler(wgpu::FilterMode::Nearest, "hl-blit-nearest"),
            linear: sampler(wgpu::FilterMode::Linear, "hl-blit-linear"),
            non_filtering: sampler(wgpu::FilterMode::Nearest, "hl-blit-nonfiltering"),
            pipelines: HashMap::new(),
            d3_module,
            d3_bind_group_layout,
            d3_pipeline_layout,
            d3_pipelines: HashMap::new(),
            integer_module,
            integer_layouts,
            integer_pipeline_layouts,
            integer_pipelines: HashMap::new(),
        }
    }

    fn sampler(&self, filter: Filter, can_filter: bool) -> &wgpu::Sampler {
        match (can_filter, filter) {
            // A non-filterable source is only ever reached with a nearest filter (linear is refused
            // before this point), and its layout admits only the non-filtering sampler.
            (false, _) => &self.non_filtering,
            (true, Filter::Nearest) => &self.nearest,
            (true, Filter::Linear) => &self.linear,
        }
    }

    /// Build (once) the blit render pipeline for a destination color `format`.
    fn ensure_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        can_filter: bool,
    ) {
        if self.pipelines.contains_key(&(format, can_filter)) {
            return;
        }
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hl-blit-pipeline"),
            layout: Some(&self.pipeline_layout[usize::from(!can_filter)]),
            vertex: wgpu::VertexState {
                module: &self.module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
            cache: None,
        });
        self.pipelines.insert((format, can_filter), pipeline);
    }

    fn ensure_d3_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        can_filter: bool,
    ) {
        if self.d3_pipelines.contains_key(&(format, can_filter)) {
            return;
        }
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hl-blit-3d-pipeline"),
            layout: Some(&self.d3_pipeline_layout[usize::from(!can_filter)]),
            vertex: wgpu::VertexState {
                module: &self.d3_module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.d3_module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });
        self.d3_pipelines.insert((format, can_filter), pipeline);
    }

    fn ensure_integer_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        signed: bool,
    ) {
        if self.integer_pipelines.contains_key(&(format, signed)) {
            return;
        }
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hl-integer-blit-pipeline"),
            layout: Some(&self.integer_pipeline_layouts[usize::from(signed)]),
            vertex: wgpu::VertexState {
                module: &self.integer_module,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.integer_module,
                entry_point: Some(if signed { "fs_sint" } else { "fs_uint" }),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview: None,
            cache: None,
        });
        self.integer_pipelines.insert((format, signed), pipeline);
    }
}

impl WgpuExecutor {
    /// Execute one `Enc::BlitTexture`: resample the `src_extent` region of `src` into the `dst_extent`
    /// region of `dst` with `filter`, by rendering a viewport-clipped textured triangle (see the module
    /// docs). Only the base subresource (mip 0 / layer 0 / whole color aspect) of a 2D color texture is
    /// supported; a non-base subresource, a 3D/layer region, a zero-sized rect, or an out-of-range region is
    /// a clean typed error (mirroring `copy_texture_to_texture`), never a panic.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn blit_texture(
        &mut self,
        res: &SessionResources,
        src: u32,
        src_sub: &TextureSubresource,
        src_origin: &Origin3d,
        src_extent: &Extent3d,
        dst: u32,
        dst_sub: &TextureSubresource,
        dst_origin: &Origin3d,
        dst_extent: &Extent3d,
        filter: Filter,
        mirror: Mirror,
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(hl_log::tag::WGPU, "blit");
        for sub in [src_sub, dst_sub] {
            if sub.mip != 0 || sub.layer != 0 || sub.aspect != TextureAspect::All {
                return Err(GpuError::Unsupported("wgpu: non-base subresource blit"));
            }
        }
        let src_dim = texture::WgpuTexture::get(res, src)?.dim;
        let dst_dim = texture::WgpuTexture::get(res, dst)?.dim;
        let depth_spanning =
            src_extent.depth > 1 || dst_extent.depth > 1 || src_origin.z != 0 || dst_origin.z != 0;
        if depth_spanning
            && (!matches!(src_dim, hl_gpu::protocol::model::enums::TextureDim::D3)
                || !matches!(dst_dim, hl_gpu::protocol::model::enums::TextureDim::D3))
        {
            return Err(GpuError::Unsupported("wgpu: 3D/layer texture blit"));
        }
        if src_extent.width == 0
            || src_extent.height == 0
            || dst_extent.width == 0
            || dst_extent.height == 0
        {
            return Err(GpuError::Invalid("wgpu: blit with a zero-sized region"));
        }
        // A MULTISAMPLED texture on either side cannot take part. The blit resamples by RENDERING: it
        // binds the source to a single-sampled `texture_2d<f32>` and draws into the destination with a
        // pipeline built at sample count 1. A multisampled destination therefore fails the pass's
        // pipeline-compatibility check and a multisampled source cannot bind at all — both as device
        // validation, out of the pass, naming the pipeline rather than the texture the caller passed.
        //
        // Measured before this existed: a multisampled destination produced `IncompatibleSampleCount`
        // at `RenderPass::end`. The software reference has refused this pair from the start
        // ("software: multisample blit"), so the executor was the side out of step; multisampled content
        // reaches a blit only after `ResolveTexture` has made it single-sampled, which is that
        // operation's whole purpose.
        for (id, refusal) in [
            (src, "wgpu: multisample blit source (resolve first)"),
            (dst, "wgpu: multisample blit destination"),
        ] {
            if texture::WgpuTexture::get(res, id)?.sample_count != 1 {
                return Err(GpuError::Unsupported(refusal));
            }
        }
        // Source dims (for UV normalization) + destination wgpu format (the pipeline's color-target format).
        // A blit reads the source through a sampler and writes the destination as a color attachment, so both
        // must have a packed COLOR layout — the depth/stencil formats have none and are rejected honestly.
        let (sw, sh, sd, src_fmt, src_wfmt, can_filter) = {
            let t = texture::WgpuTexture::get(res, src)?;
            if Format::from(t.format).texel_bytes().is_err() && t.format.block_geometry().is_none() {
                return Err(GpuError::Unsupported("wgpu: depth/stencil blit source"));
            }
            (
                t.width,
                t.height,
                t.depth,
                t.format,
                Format::from(t.format).native(),
                filterable(t.format),
            )
        };
        let (dw, dh, dd, dst_fmt, dst_wfmt) = {
            let t = texture::WgpuTexture::get(res, dst)?;
            let _ = Format::from(t.format).texel_bytes()?;
            // A blit WRITES its destination as a colour attachment, so the destination needs the same
            // usage a render pass target needs. Refused here, where the caller can be told what it named,
            // rather than as a device-validation failure inside the pass below.
            (
                t.width,
                t.height,
                t.depth,
                t.format,
                Format::from(t.format).native(),
            )
        };
        let (src_class, dst_class) = (src_fmt.numeric_class(), dst_fmt.numeric_class());
        if src_class != dst_class {
            return Err(GpuError::Invalid(
                "wgpu: blit source and destination numeric classes differ",
            ));
        }
        if src_class != TextureNumericClass::Float && filter == Filter::Linear {
            return Err(GpuError::Unsupported(
                "wgpu: linear filtering is invalid for an integer blit source",
            ));
        }
        if src_class == TextureNumericClass::Float && !can_filter && filter == Filter::Linear {
            return Err(GpuError::Unsupported(
                "wgpu: linear blit filter for a non-filterable source format",
            ));
        }

        // Bounds: the source region must lie inside `src`, the destination region inside `dst`.
        let ok = |x: u32, y: u32, w: u32, h: u32, tw: u32, th: u32| {
            x.checked_add(w).is_some_and(|e| e <= tw) && y.checked_add(h).is_some_and(|e| e <= th)
        };
        if !ok(
            src_origin.x,
            src_origin.y,
            src_extent.width,
            src_extent.height,
            sw,
            sh,
        ) || !ok(
            dst_origin.x,
            dst_origin.y,
            dst_extent.width,
            dst_extent.height,
            dw,
            dh,
        ) || src_origin
            .z
            .checked_add(src_extent.depth)
            .is_none_or(|e| e > sd)
            || dst_origin
                .z
                .checked_add(dst_extent.depth)
                .is_none_or(|e| e > dd)
        {
            return Err(GpuError::OutOfBounds);
        }

        let integer_render = src_class != TextureNumericClass::Float;
        if integer_render {
            let source = texture::WgpuTexture::get(res, src)?;
            let destination = texture::WgpuTexture::get(res, dst)?;
            if source.usage & hl_gpu::protocol::model::enums::texture_usage::COPY_SRC == 0 {
                return Err(GpuError::Invalid(
                    "wgpu: integer blit source lacks COPY_SRC",
                ));
            }
            if destination.usage & hl_gpu::protocol::model::enums::texture_usage::COPY_DST == 0 {
                return Err(GpuError::Invalid(
                    "wgpu: integer blit destination lacks COPY_DST",
                ));
            }
            let exact_copy = integer(src_fmt)
                && src_fmt == dst_fmt
                && src_extent == dst_extent
                && filter == Filter::Nearest
                && !mirror.x
                && !mirror.y
                && !mirror.z;
            if exact_copy {
                let device = &self.gpu.device;
                let src_texture = &source.texture;
                let dst_texture = &destination.texture;
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("hl-integer-blit-copy"),
                });
                for z in 0..src_extent.depth {
                    encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: src_texture,
                            mip_level: src_sub.mip,
                            origin: wgpu::Origin3d {
                                x: src_origin.x,
                                y: src_origin.y,
                                z: src_origin.z + z,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: dst_texture,
                            mip_level: dst_sub.mip,
                            origin: wgpu::Origin3d {
                                x: dst_origin.x,
                                y: dst_origin.y,
                                z: dst_origin.z + z,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width: src_extent.width,
                            height: src_extent.height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                self.gpu.queue.submit(Some(encoder.finish()));
                return Ok(());
            }
        }

        // A scaled or converting 1D blit cannot use the render path below: it binds a D2 view, which
        // wgpu rejects for a D1 texture. Keep that refusal after the exact integer copy above, because a
        // same-format 1:1 blit needs no view or shader and is a legal native texture copy.
        //
        // A CUBE is deliberately absent: it is a 2D texture with six layers underneath, so a single-layer
        // D2 view of a face is exactly what the render path builds. D3 is served as one destination draw
        // per slice; float sources use a volume view while integer sources select the nearest slice.
        for (dim, side) in [(src_dim, "source"), (dst_dim, "destination")] {
            if matches!(dim, hl_gpu::protocol::model::enums::TextureDim::D1) {
                return Err(GpuError::Unsupported(match side {
                    "source" => "wgpu: scaled or converting 1D blit source",
                    _ => "wgpu: scaled or converting 1D blit destination",
                }));
            }
        }

        let dst_texture = texture::WgpuTexture::get(res, dst)?;
        if !dst_texture.render_attachment
            && dst_texture.dim != hl_gpu::protocol::model::enums::TextureDim::D3
        {
            return Err(GpuError::Invalid(
                "wgpu: blit destination was not created as a render target",
            ));
        }

        // Normalize the source rect into UV space: uv = uv_off + local_uv * uv_scale, with local_uv running
        // 0..1 across the destination rect (see BLIT_WGSL). uv_off/scale come straight from the source rect.
        // A MIRRORED axis puts the origin at the FAR edge of the source rect and negates the scale, so
        // `uv_off + local_uv * uv_scale` walks the rect backwards. Nothing in the shader constrains the
        // sign, and the clamp-to-edge sampler handles the boundary exactly as it does unmirrored.
        let (sw_f, sh_f) = (sw as f32, sh as f32);
        let axis = |origin: u32, extent: u32, dim: f32, flip: bool| -> (f32, f32) {
            let (o, e) = (origin as f32 / dim, extent as f32 / dim);
            if flip {
                (o + e, -e)
            } else {
                (o, e)
            }
        };
        let (uv_off_x, uv_scale_x) = axis(src_origin.x, src_extent.width, sw_f, mirror.x);
        let (uv_off_y, uv_scale_y) = axis(src_origin.y, src_extent.height, sh_f, mirror.y);
        let xform: [f32; 4] = [uv_off_x, uv_off_y, uv_scale_x, uv_scale_y];

        // Lazily build the device-lifetime cache, then split the field borrows: `device`/`queue` borrow
        // `self.gpu` immutably, `cache` borrows `self.blit` mutably — disjoint fields, so both live at once.
        if self.blit.is_none() {
            self.blit = Some(BlitCache::new(&self.gpu.device));
        }
        let device = &self.gpu.device;
        let queue = &self.gpu.queue;
        let cache = self.blit.as_mut().expect("blit cache initialized above");
        if integer_render {
            cache.ensure_integer_pipeline(device, dst_wfmt, signed_integer(src_fmt));
        } else if src_dim == hl_gpu::protocol::model::enums::TextureDim::D3 {
            cache.ensure_d3_pipeline(device, dst_wfmt, can_filter);
        } else {
            cache.ensure_pipeline(device, dst_wfmt, can_filter);
        }

        // Build a SINGLE-LAYER 2D view of each side rather than binding the texture's default view.
        //
        // The default view of an array texture is `D2Array`, and the blit's bind-group layout declares
        // `D2`, so binding it failed device validation with `InvalidTextureDimension` — for EVERY blit
        // whose source happened to have more than one layer, including at layer 0, which is the case both
        // backends otherwise support. A blit addresses one layer of each side by definition, so naming
        // that layer in the view is both the fix and the more accurate description of the operation.
        //
        // This deliberately does not unlock `layer != 0`, which is still refused above: the software
        // oracle materializes one plane per texture and has no array-layer concept, so serving layered
        // blits here alone would make the executor perform what the reference refuses — a false
        // divergence in the direction this project has spent the night removing.
        let layer_view = |id: u32,
                          sub: &TextureSubresource,
                          layer: u32,
                          label: &str|
         -> Result<wgpu::TextureView> {
            let t = texture::WgpuTexture::get(res, id)?;
            Ok(t.texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some(label),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer,
                array_layer_count: Some(1),
                // Name the MIP LEVEL too, not only the layer. The view used to span every level the
                // texture had, and the blit samples with `textureSample`, whose level of detail comes
                // from the coordinate's derivative — so a DOWNSCALING blit from a mipmapped source
                // selected a smaller level and returned its contents instead of the one the operation
                // names. Measured with a level-per-colour source: an 8x8 four-level texture blitted to
                // 1x1 returned level 3 where the operation said level 0, under BOTH filters, because the
                // sampler's mipmap filter is nearest but its LOD range was the default 0..32.
                //
                // A blit addresses one subresource of each side by definition. Naming it in the view is
                // the same correction the array layer needed and for the same reason; it also makes the
                // destination view single-level, which a colour attachment must be.
                base_mip_level: sub.mip,
                mip_level_count: Some(1),
                ..Default::default()
            }))
        };
        let d3_float = !integer_render && src_dim == hl_gpu::protocol::model::enums::TextureDim::D3;
        let pipeline = if integer_render {
            cache
                .integer_pipelines
                .get(&(dst_wfmt, signed_integer(src_fmt)))
        } else if d3_float {
            cache.d3_pipelines.get(&(dst_wfmt, can_filter))
        } else {
            cache.pipelines.get(&(dst_wfmt, can_filter))
        }
        .expect("pipeline built above");

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hl-blit"),
        });
        for dz in 0..dst_extent.depth {
            // Vulkan maps destination texel centers through the continuous source region. Integer
            // textures permit NEAREST only, so flooring this coordinate selects the same slice a nearest
            // sampler would. Float D3 sources keep the continuous normalized coordinate for nearest or
            // trilinear sampling in the volume shader.
            let z_step = (dz as f32 + 0.5) * src_extent.depth as f32 / dst_extent.depth as f32;
            let source_z = if mirror.z {
                src_origin.z as f32 + src_extent.depth as f32 - z_step
            } else {
                src_origin.z as f32 + z_step
            };
            let src_z =
                (source_z.floor() as u32).clamp(src_origin.z, src_origin.z + src_extent.depth - 1);
            let dst_z = dst_origin.z + dz;
            let staged = src_dim == hl_gpu::protocol::model::enums::TextureDim::D3;
            let (src_view, dst_view, staged_dst) = if staged {
                let temp = |label, width, height, format, usage| {
                    device.create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &[],
                    })
                };
                let dst_temp = temp(
                    "hl-blit-dst-slice",
                    dw,
                    dh,
                    dst_wfmt,
                    wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::RENDER_ATTACHMENT,
                );
                let src_tex = texture::WgpuTexture::get(res, src)?;
                let dst_tex = texture::WgpuTexture::get(res, dst)?;
                let src_view = if d3_float {
                    src_tex.texture.create_view(&wgpu::TextureViewDescriptor {
                        label: Some("hl-blit-src-volume"),
                        dimension: Some(wgpu::TextureViewDimension::D3),
                        base_mip_level: src_sub.mip,
                        mip_level_count: Some(1),
                        ..Default::default()
                    })
                } else {
                    let src_temp = temp(
                        "hl-blit-src-slice",
                        sw,
                        sh,
                        src_wfmt,
                        wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                    );
                    enc.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &src_tex.texture,
                            mip_level: src_sub.mip,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: 0,
                                z: src_z,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: &src_temp,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width: sw,
                            height: sh,
                            depth_or_array_layers: 1,
                        },
                    );
                    src_temp.create_view(&wgpu::TextureViewDescriptor::default())
                };
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &dst_tex.texture,
                        mip_level: dst_sub.mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: dst_z,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &dst_temp,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: dw,
                        height: dh,
                        depth_or_array_layers: 1,
                    },
                );
                (
                    src_view,
                    dst_temp.create_view(&wgpu::TextureViewDescriptor::default()),
                    Some(dst_temp),
                )
            } else {
                (
                    layer_view(src, src_sub, src_z, "hl-blit-src")?,
                    layer_view(dst, dst_sub, dst_z, "hl-blit-dst")?,
                    None,
                )
            };
            // WGSL aligns the trailing `vec3` to 16 bytes, making this structure 48 bytes.
            let mut slice_xform = [0.0_f32; 12];
            slice_xform[..4].copy_from_slice(&xform);
            slice_xform[4] = source_z / sd as f32;
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hl-blit-xform"),
                size: std::mem::size_of_val(&slice_xform) as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            // SAFETY: `[f32; 8]` is contiguous and the read-only byte slice has the same lifetime.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    slice_xform.as_ptr().cast::<u8>(),
                    std::mem::size_of_val(&slice_xform),
                )
            };
            queue.write_buffer(&uniform, 0, bytes);
            let signed = signed_integer(src_fmt);
            let integer_entries = [
                wgpu::BindGroupEntry {
                    binding: if signed { 2 } else { 0 },
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform.as_entire_binding(),
                },
            ];
            let float_entries = [
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(cache.sampler(filter, can_filter)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ];
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("hl-blit-bg"),
                layout: if integer_render {
                    &cache.integer_layouts[usize::from(signed)]
                } else if d3_float {
                    &cache.d3_bind_group_layout[usize::from(!can_filter)]
                } else {
                    &cache.bind_group_layout[usize::from(!can_filter)]
                },
                entries: if integer_render {
                    &integer_entries
                } else {
                    &float_entries
                },
            });
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl-blit-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst_view,
                    resolve_target: None,
                    // LOAD (not clear): a blit writes only the destination rect (the scissor clips the draw
                    // to it), so every destination texel OUTSIDE that rect must survive unchanged.
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            // The viewport maps the fullscreen triangle's NDC onto the destination rect (so `uv` interpolates
            // 0..1 across exactly that rect); the scissor clips rasterization to it so the parts of the
            // oversized triangle that fall outside the rect never touch the rest of the destination texture.
            pass.set_viewport(
                dst_origin.x as f32,
                dst_origin.y as f32,
                dst_extent.width as f32,
                dst_extent.height as f32,
                0.0,
                1.0,
            );
            pass.set_scissor_rect(
                dst_origin.x,
                dst_origin.y,
                dst_extent.width,
                dst_extent.height,
            );
            pass.draw(0..3, 0..1);
            drop(pass);
            if let Some(dst_temp) = staged_dst {
                let dst_tex = texture::WgpuTexture::get(res, dst)?;
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &dst_temp,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &dst_tex.texture,
                        mip_level: dst_sub.mip,
                        origin: wgpu::Origin3d {
                            x: 0,
                            y: 0,
                            z: dst_z,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d {
                        width: dw,
                        height: dh,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }
        queue.submit(Some(enc.finish()));
        Ok(())
    }
}
