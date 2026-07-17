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
//! The blit pipeline/sampler are CACHED on the executor (`BlitCache`): the WGSL module + bind-group layout
//! + the two samplers are built once, and one render pipeline per `(dst format, filter)` is memoized — a
//! GL app blits constantly, so a per-call rebuild would be wasteful. The concrete source-texture bind group
//! + a 16-byte UV-transform uniform are the only per-call allocations.

use std::collections::HashMap;

use hl_gpu::protocol::model::descriptor::{Extent3d, Origin3d, TextureSubresource};
use hl_gpu::protocol::model::enums::{Filter, TextureAspect};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::convert::{texel_bytes, texture_format};
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

/// Cached, device-lifetime blit objects. The module + bind-group layout + samplers are built once; a render
/// pipeline is memoized per destination `wgpu::TextureFormat` (a pipeline is bound to its color-target
/// format). The FILTER keys the SAMPLER, not the pipeline — a single filtering sampler-binding-type serves
/// both nearest and linear — so the pipeline map keys on format alone.
pub(crate) struct BlitCache {
    module: wgpu::ShaderModule,
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
}

impl BlitCache {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hl-blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hl-blit-bgl"),
            entries: &[
                // 0: the source texture (filterable float 2D).
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // 1: a filtering sampler (nearest or linear picked per call).
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hl-blit-pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
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
        Self {
            module,
            bind_group_layout,
            pipeline_layout,
            nearest: sampler(wgpu::FilterMode::Nearest, "hl-blit-nearest"),
            linear: sampler(wgpu::FilterMode::Linear, "hl-blit-linear"),
            pipelines: HashMap::new(),
        }
    }

    fn sampler(&self, filter: Filter) -> &wgpu::Sampler {
        match filter {
            Filter::Nearest => &self.nearest,
            Filter::Linear => &self.linear,
        }
    }

    /// Build (once) the blit render pipeline for a destination color `format`.
    fn ensure_pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.pipelines.contains_key(&format) {
            return;
        }
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hl-blit-pipeline"),
            layout: Some(&self.pipeline_layout),
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
        self.pipelines.insert(format, pipeline);
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
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(hl_log::tag::WGPU, "blit");
        for sub in [src_sub, dst_sub] {
            if sub.mip != 0 || sub.layer != 0 || sub.aspect != TextureAspect::All {
                return Err(GpuError::Unsupported("wgpu: non-base subresource blit"));
            }
        }
        if src_origin.z != 0 || dst_origin.z != 0 || src_extent.depth > 1 || dst_extent.depth > 1 {
            return Err(GpuError::Unsupported("wgpu: 3D/layer texture blit"));
        }
        if src_extent.width == 0
            || src_extent.height == 0
            || dst_extent.width == 0
            || dst_extent.height == 0
        {
            return Err(GpuError::Invalid("wgpu: blit with a zero-sized region"));
        }

        // Source dims (for UV normalization) + destination wgpu format (the pipeline's color-target format).
        // A blit reads the source through a sampler and writes the destination as a color attachment, so both
        // must have a packed COLOR layout — the depth/stencil formats have none and are rejected honestly.
        let (sw, sh) = {
            let t = texture::native(res, src)?;
            let _ = texel_bytes(t.format)?;
            (t.width, t.height)
        };
        let (dw, dh, dst_wfmt) = {
            let t = texture::native(res, dst)?;
            let _ = texel_bytes(t.format)?;
            (t.width, t.height, texture_format(t.format)?)
        };

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
        ) {
            return Err(GpuError::OutOfBounds);
        }

        // Normalize the source rect into UV space: uv = uv_off + local_uv * uv_scale, with local_uv running
        // 0..1 across the destination rect (see BLIT_WGSL). uv_off/scale come straight from the source rect.
        let (sw_f, sh_f) = (sw as f32, sh as f32);
        let xform: [f32; 4] = [
            src_origin.x as f32 / sw_f,
            src_origin.y as f32 / sh_f,
            src_extent.width as f32 / sw_f,
            src_extent.height as f32 / sh_f,
        ];

        // Lazily build the device-lifetime cache, then split the field borrows: `device`/`queue` borrow
        // `self.gpu` immutably, `cache` borrows `self.blit` mutably — disjoint fields, so both live at once.
        if self.blit.is_none() {
            self.blit = Some(BlitCache::new(&self.gpu.device));
        }
        let device = &self.gpu.device;
        let queue = &self.gpu.queue;
        let cache = self.blit.as_mut().expect("blit cache initialized above");
        cache.ensure_pipeline(device, dst_wfmt);

        // Per-call: the UV-transform uniform + the source-texture bind group.
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-blit-xform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform, 0, f32x4_bytes(&xform));

        let src_view = &texture::native(res, src)?.view;
        let dst_view = &texture::native(res, dst)?.view;
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hl-blit-bg"),
            layout: &cache.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(cache.sampler(filter)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let pipeline = cache
            .pipelines
            .get(&dst_wfmt)
            .expect("pipeline built by ensure_pipeline above");

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("hl-blit"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl-blit-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst_view,
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
        }
        queue.submit(Some(enc.finish()));
        self.gpu.device.poll(wgpu::Maintain::Wait);
        Ok(())
    }
}

/// Reinterpret a `[f32; 4]` as its 16 little-endian bytes for a uniform upload (dependency-free; the host
/// is little-endian, matching the shader's expected uniform byte order).
fn f32x4_bytes(v: &[f32; 4]) -> &[u8] {
    // SAFETY: `[f32; 4]` is 16 contiguous bytes with no padding and no invalid bit patterns; the returned
    // read-only slice borrows `v` for the same lifetime.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of::<[f32; 4]>()) }
}
