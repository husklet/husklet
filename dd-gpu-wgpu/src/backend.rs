//! [`WgpuBackend`] — a `wgpu` executor for the dd-GPU command IR.
//!
//! Implements [`dd_gpu::GpuBackend`] on wgpu 24 + naga 24. Resource lifecycle, host<->device copies,
//! render/compute pipelines built from the IR descriptors, an encoder-op replay (draw / dispatch / copy /
//! viewport / scissor / scissored clear), cached bind-group layouts (via each pipeline's auto layout),
//! and `WaitFence` emulated on wgpu's submission index + `poll(Wait)` (wgpu has no timeline semaphore).
//!
//! Shader translation runs host-side in naga (see `crate::shader`): the IR's SPIR-V is lowered to WGSL.
//! The current GL shim still packs MSL-as-bytes into the same field; naga can't consume MSL, so such a
//! payload falls back to a builtin WGSL pipeline (a vertex-color and a textured one, matching the
//! semantics the bespoke Metal replay's captures use) — enough to reproduce the solid-quad and
//! textured-glyph golden cases through wgpu while the guest is migrated to forward SPIR-V/GLSL.

use std::collections::HashMap;
use std::num::NonZeroU64;

use dd_gpu::backend::{Capabilities, GpuBackend, PresentKind, PresentToken};
use dd_gpu::id::*;
use dd_gpu::ir::*;
use dd_gpu::{GpuError, Result};

use wgpu::util::DeviceExt as _;

// ---------------------------------------------------------------------------------------------------
// builtin shaders (WGSL) — used when the guest shipped MSL-as-bytes (not naga-translatable). They mirror
// the FLAT/TEX MSL the bespoke Metal replay's captures carry: pixel-space position -> NDC via a
// `sk_RTAdjust` uniform, passthrough vertex color, and (textured) an atlas sample modulated by color.
// ---------------------------------------------------------------------------------------------------

const FLAT_WGSL: &str = r#"
struct Uniforms { rt: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };
@vertex
fn vs(@location(0) position: vec2<f32>, @location(1) color: vec4<f32>) -> VOut {
    var o: VOut;
    let dp = vec4<f32>(position, 0.0, 1.0);
    o.pos = vec4<f32>(dp.xy * u.rt.xz + dp.ww * u.rt.yw, 0.0, dp.w);
    o.color = color;
    return o;
}
@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> { return i.color; }
"#;

const TEX_WGSL: &str = r#"
struct Uniforms { rt: vec4<f32>, inv: vec4<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32>, @location(1) uv: vec2<f32> };
@vertex
fn vs(@location(0) position: vec2<f32>, @location(1) color: vec4<f32>, @location(2) texcoord: vec2<u32>) -> VOut {
    var o: VOut;
    let dp = vec4<f32>(position, 0.0, 1.0);
    o.pos = vec4<f32>(dp.xy * u.rt.xz + dp.ww * u.rt.yw, 0.0, dp.w);
    o.color = color;
    o.uv = vec2<f32>(f32(texcoord.x), f32(texcoord.y)) * u.inv.xy;
    return o;
}
@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> { return textureSample(atlas, samp, i.uv) * i.color; }
"#;

// Fullscreen-triangle clear: draws `color` (a group-0 uniform) over the scissored region — how the IR's
// `ClearRect` (a mid-stream sub-rect clear, which wgpu has no direct op for) is emulated.
const CLEAR_WGSL: &str = r#"
struct C { color: vec4<f32> };
@group(0) @binding(0) var<uniform> c: C;
@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -3.0), vec2<f32>(-1.0, 1.0), vec2<f32>(3.0, 1.0));
    return vec4<f32>(p[vi], 0.0, 1.0);
}
@fragment
fn fs() -> @location(0) vec4<f32> { return c.color; }
"#;

// ---------------------------------------------------------------------------------------------------
// enum / bit mapping helpers (IR -> wgpu)
// ---------------------------------------------------------------------------------------------------

fn tex_format(f: TextureFormat) -> wgpu::TextureFormat {
    use wgpu::TextureFormat as W;
    match f {
        TextureFormat::Rgba8Unorm => W::Rgba8Unorm,
        TextureFormat::Bgra8Unorm => W::Bgra8Unorm,
        TextureFormat::Rgba8Srgb => W::Rgba8UnormSrgb,
        TextureFormat::Bgra8Srgb => W::Bgra8UnormSrgb,
        TextureFormat::R8Unorm => W::R8Unorm,
        TextureFormat::Rg8Unorm => W::Rg8Unorm,
        TextureFormat::Rgba16Float => W::Rgba16Float,
        TextureFormat::Rgba32Float => W::Rgba32Float,
        TextureFormat::R32Float => W::R32Float,
        TextureFormat::Depth32Float => W::Depth32Float,
        TextureFormat::Depth24PlusStencil8 => W::Depth24PlusStencil8,
    }
}

fn is_depth(f: TextureFormat) -> bool {
    matches!(f, TextureFormat::Depth32Float | TextureFormat::Depth24PlusStencil8)
}

fn buffer_usages(bits: u32) -> wgpu::BufferUsages {
    use buffer_usage as b;
    let mut u = wgpu::BufferUsages::empty();
    if bits & b::VERTEX != 0 { u |= wgpu::BufferUsages::VERTEX; }
    if bits & b::INDEX != 0 { u |= wgpu::BufferUsages::INDEX; }
    if bits & b::UNIFORM != 0 { u |= wgpu::BufferUsages::UNIFORM; }
    if bits & b::STORAGE != 0 { u |= wgpu::BufferUsages::STORAGE; }
    if bits & b::INDIRECT != 0 { u |= wgpu::BufferUsages::INDIRECT; }
    // write_buffer needs COPY_DST; read_buffer stages via COPY_SRC. Always allow both — cheap on unified
    // memory and it keeps every guest buffer host-copyable regardless of the declared usage bits.
    u | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC
}

fn texture_usages(bits: u32, format: TextureFormat) -> wgpu::TextureUsages {
    use texture_usage as t;
    let mut u = wgpu::TextureUsages::empty();
    if bits & t::SAMPLED != 0 { u |= wgpu::TextureUsages::TEXTURE_BINDING; }
    if bits & t::STORAGE != 0 { u |= wgpu::TextureUsages::STORAGE_BINDING; }
    if bits & t::RENDER_TARGET != 0 || bits & t::PRESENT != 0 { u |= wgpu::TextureUsages::RENDER_ATTACHMENT; }
    // Always allow host copy both ways (upload atlases / read back) for color formats.
    if !is_depth(format) {
        u |= wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC;
    }
    u
}

fn filter(f: Filter) -> wgpu::FilterMode {
    match f {
        Filter::Nearest => wgpu::FilterMode::Nearest,
        Filter::Linear => wgpu::FilterMode::Linear,
    }
}

fn address(a: AddressMode) -> wgpu::AddressMode {
    match a {
        AddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        AddressMode::Repeat => wgpu::AddressMode::Repeat,
        AddressMode::MirrorRepeat => wgpu::AddressMode::MirrorRepeat,
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

fn blend_factor(v: u32) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as F;
    match v {
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
        11 | 13 => F::Constant,
        12 | 14 => F::OneMinusConstant,
        _ => F::One,
    }
}

fn blend_op(v: u32) -> wgpu::BlendOperation {
    match v {
        1 => wgpu::BlendOperation::Subtract,
        2 => wgpu::BlendOperation::ReverseSubtract,
        3 => wgpu::BlendOperation::Min,
        4 => wgpu::BlendOperation::Max,
        _ => wgpu::BlendOperation::Add,
    }
}

fn color_writes(mask: u32) -> wgpu::ColorWrites {
    let mut w = wgpu::ColorWrites::empty();
    if mask & 0x1 != 0 { w |= wgpu::ColorWrites::RED; }
    if mask & 0x2 != 0 { w |= wgpu::ColorWrites::GREEN; }
    if mask & 0x4 != 0 { w |= wgpu::ColorWrites::BLUE; }
    if mask & 0x8 != 0 { w |= wgpu::ColorWrites::ALPHA; }
    w
}

/// Decode the shim's compact vertex-attribute format code (`comps | kind<<8 | norm<<16`, see
/// `metal_backend::metal_vertex_format`) to a wgpu `VertexFormat`. wgpu lacks 8/16-bit x1/x3 formats, so
/// odd component counts round up to the nearest supported width.
fn vertex_format(code: u32) -> wgpu::VertexFormat {
    use wgpu::VertexFormat as V;
    let comps = (code & 0xff).clamp(1, 4);
    let kind = (code >> 8) & 0xff;
    let norm = (code & (1 << 16)) != 0;
    match kind {
        1 => match (comps, norm) {
            (1 | 2, true) => V::Unorm8x2,
            (1 | 2, false) => V::Uint8x2,
            (_, true) => V::Unorm8x4,
            (_, false) => V::Uint8x4,
        },
        2 => match (comps, norm) {
            (1 | 2, true) => V::Snorm8x2,
            (1 | 2, false) => V::Sint8x2,
            (_, true) => V::Snorm8x4,
            (_, false) => V::Sint8x4,
        },
        3 => match (comps, norm) {
            (1 | 2, true) => V::Unorm16x2,
            (1 | 2, false) => V::Uint16x2,
            (_, true) => V::Unorm16x4,
            (_, false) => V::Uint16x4,
        },
        4 => match (comps, norm) {
            (1 | 2, true) => V::Snorm16x2,
            (1 | 2, false) => V::Sint16x2,
            (_, true) => V::Snorm16x4,
            (_, false) => V::Sint16x4,
        },
        5 => match comps {
            1 => V::Uint32,
            2 => V::Uint32x2,
            3 => V::Uint32x3,
            _ => V::Uint32x4,
        },
        6 => match comps {
            1 => V::Sint32,
            2 => V::Sint32x2,
            3 => V::Sint32x3,
            _ => V::Sint32x4,
        },
        _ => match comps {
            1 => V::Float32,
            2 => V::Float32x2,
            3 => V::Float32x3,
            _ => V::Float32x4,
        },
    }
}

// ---------------------------------------------------------------------------------------------------
// resources
// ---------------------------------------------------------------------------------------------------

struct TexEntry {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    #[allow(dead_code)] // retained for the Y-flip / present-orientation decisions in later increments
    format: TextureFormat,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, PartialEq)]
enum PipeKind {
    /// Builtin vertex-color pipeline (FLAT_WGSL): bind-group binding by resource type, uniform -> 0.
    Flat,
    /// Builtin textured pipeline (TEX_WGSL): uniform -> 0, texture -> 1, sampler -> 2.
    Tex,
    /// App pipeline from naga-translated WGSL: IR binding numbers used verbatim.
    App,
}

struct RenderPipe {
    pipeline: wgpu::RenderPipeline,
    kind: PipeKind,
}

/// The wgpu executor. Constructed either on its own offscreen Metal device ([`WgpuBackend::new`]) or over
/// a shared `wgpu::Device`/`Queue` ([`WgpuBackend::from_shared`], the seam toward zero-copy IOSurface).
pub struct WgpuBackend {
    device: wgpu::Device,
    queue: wgpu::Queue,

    buffers: HashMap<u32, wgpu::Buffer>,
    textures: HashMap<u32, TexEntry>,
    /// Executor-injected render targets (present surface). Separate namespace from guest `textures` so the
    /// reserved present id can't be clobbered by a guest `create_texture` — resolved only on a guest miss.
    present_targets: HashMap<u32, TexEntry>,
    samplers: HashMap<u32, wgpu::Sampler>,
    /// naga-translated shader modules; absent id => builtin fallback for pipelines that reference it.
    shaders: HashMap<u32, wgpu::ShaderModule>,
    render_pipelines: HashMap<u32, RenderPipe>,
    compute_pipelines: HashMap<u32, wgpu::ComputePipeline>,
    bind_groups: HashMap<u32, BindGroupDesc>,

    // builtin modules (compiled once)
    flat_module: wgpu::ShaderModule,
    tex_module: wgpu::ShaderModule,
    clear_pipeline: wgpu::RenderPipeline,

    /// Fence values reached, and the submission index that will reach them. `wait_fence` polls to Wait.
    last_submission: Option<wgpu::SubmissionIndex>,
    fences: HashMap<u32, u64>,
    /// Ids that are the presented surface (top-left, not offscreen). Injected via `set_render_target`.
    surface_ids: std::collections::HashSet<u32>,
}

impl WgpuBackend {
    /// Create a backend on a fresh offscreen Metal device (own `Instance`/`Adapter`/`Device`/`Queue`).
    pub fn new() -> Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok_or(GpuError::Unsupported("no Metal adapter"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("dd-gpu-wgpu"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|_| GpuError::Unsupported("request_device"))?;
        Ok(Self::from_shared(device, queue))
    }

    /// Build over an existing `wgpu::Device`/`Queue`. This is the seam toward zero-copy IOSurface interop:
    /// once a `wgpu::Device` is created over dd-display's shared `MTLDevice` via `wgpu-hal`'s
    /// `Device::device_from_raw` (crossing the objc2-metal <-> metal-rs raw-pointer boundary), the
    /// executor renders straight into dd's IOSurface-backed texture with no readback.
    pub fn from_shared(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let flat_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dd-flat"),
            source: wgpu::ShaderSource::Wgsl(FLAT_WGSL.into()),
        });
        let tex_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dd-tex"),
            source: wgpu::ShaderSource::Wgsl(TEX_WGSL.into()),
        });
        let clear_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dd-clear"),
            source: wgpu::ShaderSource::Wgsl(CLEAR_WGSL.into()),
        });
        let clear_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dd-clear-pipe"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &clear_module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &clear_module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        Self {
            device,
            queue,
            buffers: HashMap::new(),
            textures: HashMap::new(),
            present_targets: HashMap::new(),
            samplers: HashMap::new(),
            shaders: HashMap::new(),
            render_pipelines: HashMap::new(),
            compute_pipelines: HashMap::new(),
            bind_groups: HashMap::new(),
            flat_module,
            tex_module,
            clear_pipeline,
            last_submission: None,
            fences: HashMap::new(),
            surface_ids: std::collections::HashSet::new(),
        }
    }

    /// Access the underlying device/queue (for interop / test harnesses building wgpu textures).
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Register `id` as the executor's presented render target (mirrors `MetalBackend::set_render_target`).
    /// Presented surfaces are stored top-left and are NOT Y-flipped; offscreen render targets would be.
    pub fn set_render_target(&mut self, id: u32, texture: wgpu::Texture, format: TextureFormat) {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let (width, height) = (texture.width(), texture.height());
        self.present_targets.insert(id, TexEntry { texture, view, format, width, height });
        self.surface_ids.insert(id);
    }

    fn resolve_tex(&self, id: u32) -> Result<&TexEntry> {
        self.textures
            .get(&id)
            .or_else(|| self.present_targets.get(&id))
            .ok_or(GpuError::UnknownId { kind: "texture", id })
    }

    /// Read an RGBA8 render target / texture back to host bytes (tight rows, no padding). Used by the
    /// readback-present fallback and the golden verification harness.
    pub fn read_target(&self, id: u32) -> Result<Vec<u8>> {
        let t = self.resolve_tex(id)?;
        let mut out = vec![0u8; (t.width * t.height * 4) as usize];
        self.read_texture_into(t, &mut out)?;
        Ok(out)
    }

    fn read_texture_into(&self, t: &TexEntry, out: &mut [u8]) -> Result<()> {
        let unpadded = t.width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dd-readback"),
            size: (padded * t.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &t.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(t.height),
                },
            },
            wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        for row in 0..t.height {
            let src = (row * padded) as usize;
            let dst = (row * unpadded) as usize;
            out[dst..dst + unpadded as usize].copy_from_slice(&data[src..src + unpadded as usize]);
        }
        drop(data);
        staging.unmap();
        Ok(())
    }

    /// Build a `wgpu::BindGroup` for group `desc` used with pipeline `kind`, borrowing the group's layout
    /// from the pipeline (auto layout). Bindings are remapped by resource type for the builtins.
    fn build_bind_group(
        &self,
        layout: &wgpu::BindGroupLayout,
        kind: PipeKind,
        desc: &BindGroupDesc,
    ) -> Result<wgpu::BindGroup> {
        // Resolve each entry to a binding number + resource, keeping owned temporaries alive.
        let mut entries: Vec<wgpu::BindGroupEntry> = Vec::with_capacity(desc.entries.len());
        for e in &desc.entries {
            let binding = match (kind, &e.resource) {
                (PipeKind::App, _) => e.binding,
                (_, BindResource::Buffer { .. }) => 0,
                (_, BindResource::Texture { .. }) => 1,
                (_, BindResource::Sampler { .. }) => 2,
            };
            let resource = match &e.resource {
                BindResource::Buffer { id, offset, size } => {
                    let buf = self.buffers.get(id).ok_or(GpuError::UnknownId { kind: "buffer", id: *id })?;
                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: buf,
                        offset: *offset,
                        size: NonZeroU64::new(*size),
                    })
                }
                BindResource::Texture { id } => {
                    wgpu::BindingResource::TextureView(&self.resolve_tex(*id)?.view)
                }
                BindResource::Sampler { id } => wgpu::BindingResource::Sampler(
                    self.samplers.get(id).ok_or(GpuError::UnknownId { kind: "sampler", id: *id })?,
                ),
            };
            entries.push(wgpu::BindGroupEntry { binding, resource });
        }
        Ok(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &entries,
        }))
    }
}

// ---------------------------------------------------------------------------------------------------
// GpuBackend impl
// ---------------------------------------------------------------------------------------------------

impl GpuBackend for WgpuBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "dd-gpu-wgpu (Metal)".into(),
            unified_memory: true,
            supports_compute: true,
            supports_graphics: true,
            max_texture_2d: self.device.limits().max_texture_dimension_2d,
            present_kinds: vec![PresentKind::IoSurface],
        }
    }

    fn create_buffer(&mut self, id: BufferId, desc: &BufferDesc) -> Result<()> {
        if self.buffers.contains_key(&id.0) {
            return Err(GpuError::DuplicateId { kind: "buffer", id: id.0 });
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(desc.label.as_str()),
            size: desc.size.max(4),
            usage: buffer_usages(desc.usage),
            mapped_at_creation: false,
        });
        self.buffers.insert(id.0, buffer);
        Ok(())
    }

    fn destroy_buffer(&mut self, id: BufferId) -> Result<()> {
        self.buffers.remove(&id.0).ok_or(GpuError::UnknownId { kind: "buffer", id: id.0 }).map(|_| ())
    }

    fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> Result<()> {
        let buf = self.buffers.get(&id.0).ok_or(GpuError::UnknownId { kind: "buffer", id: id.0 })?;
        self.queue.write_buffer(buf, offset, data);
        Ok(())
    }

    fn read_buffer(&mut self, id: BufferId, offset: u64, out: &mut [u8]) -> Result<()> {
        let buf = self.buffers.get(&id.0).ok_or(GpuError::UnknownId { kind: "buffer", id: id.0 })?;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dd-buf-readback"),
            size: out.len() as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(buf, offset, &staging, 0, out.len() as u64);
        self.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::Maintain::Wait);
        out.copy_from_slice(&slice.get_mapped_range());
        staging.unmap();
        Ok(())
    }

    fn create_texture(&mut self, id: TextureId, desc: &TextureDesc) -> Result<()> {
        if self.textures.contains_key(&id.0) {
            return Err(GpuError::DuplicateId { kind: "texture", id: id.0 });
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(desc.label.as_str()),
            size: wgpu::Extent3d {
                width: desc.width.max(1),
                height: desc.height.max(1),
                depth_or_array_layers: desc.depth.max(1),
            },
            mip_level_count: desc.mip_levels.max(1),
            sample_count: desc.sample_count.max(1),
            dimension: wgpu::TextureDimension::D2,
            format: tex_format(desc.format),
            usage: texture_usages(desc.usage, desc.format),
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.textures.insert(
            id.0,
            TexEntry { texture, view, format: desc.format, width: desc.width.max(1), height: desc.height.max(1) },
        );
        Ok(())
    }

    fn destroy_texture(&mut self, id: TextureId) -> Result<()> {
        self.textures.remove(&id.0).ok_or(GpuError::UnknownId { kind: "texture", id: id.0 }).map(|_| ())
    }

    fn read_texture(&mut self, id: TextureId, out: &mut [u8]) -> Result<()> {
        let t = self.resolve_tex(id.0)?;
        self.read_texture_into(t, out)
    }

    fn create_sampler(&mut self, id: SamplerId, desc: &SamplerDesc) -> Result<()> {
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: None,
            address_mode_u: address(desc.address_u),
            address_mode_v: address(desc.address_v),
            address_mode_w: address(desc.address_w),
            mag_filter: filter(desc.mag_filter),
            min_filter: filter(desc.min_filter),
            mipmap_filter: filter(desc.mip_filter),
            ..Default::default()
        });
        self.samplers.insert(id.0, sampler);
        Ok(())
    }

    fn destroy_sampler(&mut self, id: SamplerId) -> Result<()> {
        self.samplers.remove(&id.0);
        Ok(())
    }

    fn create_shader(&mut self, id: ShaderId, spirv: &[u32]) -> Result<()> {
        match crate::shader::spirv_to_wgsl(spirv) {
            Ok(Some(wgsl)) => {
                let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("dd-app-shader"),
                    source: wgpu::ShaderSource::Wgsl(wgsl.into()),
                });
                self.shaders.insert(id.0, module);
            }
            // Not SPIR-V (legacy MSL-as-bytes / opaque): drop any prior module so pipelines fall back to
            // the builtin WGSL. Not an error — the guest is mid-migration to the SPIR-V ABI.
            Ok(None) => {
                self.shaders.remove(&id.0);
            }
            Err(e) => {
                eprintln!("dd-gpu-wgpu: shader {} translation failed: {e}", id.0);
                self.shaders.remove(&id.0);
            }
        }
        Ok(())
    }

    fn destroy_shader(&mut self, id: ShaderId) -> Result<()> {
        self.shaders.remove(&id.0);
        Ok(())
    }

    fn create_render_pipeline(&mut self, id: PipelineId, desc: &RenderPipelineDesc) -> Result<()> {
        // App pipeline iff the guest shipped a naga-translatable vertex module.
        let app = self.shaders.contains_key(&desc.vertex.module);
        let kind = if app {
            PipeKind::App
        } else if desc.vertex_buffers.iter().any(|l| l.attrs.iter().any(|a| a.location >= 2)) {
            PipeKind::Tex
        } else {
            PipeKind::Flat
        };

        // Vertex buffer layouts (owned, referenced below). One wgpu layout per IR VertexLayout; slot i ==
        // layout i (matches SetVertexBuffer.slot). Empty attrs -> the builtin float2+float4 layout.
        let mut attr_store: Vec<Vec<wgpu::VertexAttribute>> = Vec::new();
        for layout in &desc.vertex_buffers {
            let attrs = if layout.attrs.is_empty() {
                vec![
                    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
                    wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 8, shader_location: 1 },
                ]
            } else {
                layout
                    .attrs
                    .iter()
                    .map(|a| wgpu::VertexAttribute {
                        format: vertex_format(a.format),
                        offset: a.offset as u64,
                        shader_location: a.location,
                    })
                    .collect()
            };
            attr_store.push(attrs);
        }
        let vbuffers: Vec<wgpu::VertexBufferLayout> = desc
            .vertex_buffers
            .iter()
            .enumerate()
            .map(|(i, layout)| wgpu::VertexBufferLayout {
                array_stride: layout.stride.max(4) as u64,
                step_mode: if layout.step_mode == 1 {
                    wgpu::VertexStepMode::Instance
                } else {
                    wgpu::VertexStepMode::Vertex
                },
                attributes: &attr_store[i],
            })
            .collect();
        // Builtins with no IR vertex layout still need one buffer slot.
        let fallback_attrs = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 8, shader_location: 1 },
        ];
        let fallback_layout = [wgpu::VertexBufferLayout {
            array_stride: 24,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &fallback_attrs,
        }];
        let vbuffers_ref: &[wgpu::VertexBufferLayout] = if vbuffers.is_empty() { &fallback_layout } else { &vbuffers };

        // Color targets (owned).
        let targets: Vec<Option<wgpu::ColorTargetState>> = desc
            .color_targets
            .iter()
            .map(|c| {
                Some(wgpu::ColorTargetState {
                    format: tex_format(c.format),
                    blend: c.blend.as_ref().map(|b| wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: blend_factor(b.src_color),
                            dst_factor: blend_factor(b.dst_color),
                            operation: blend_op(b.op_color),
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: blend_factor(b.src_alpha),
                            dst_factor: blend_factor(b.dst_alpha),
                            operation: blend_op(b.op_alpha),
                        },
                    }),
                    write_mask: color_writes(c.write_mask),
                })
            })
            .collect();

        // Resolve the modules + entry points.
        let (vmodule, ventry, fmodule, fentry): (&wgpu::ShaderModule, &str, &wgpu::ShaderModule, &str) = match kind {
            PipeKind::App => {
                let vm = self.shaders.get(&desc.vertex.module).unwrap();
                let fm = desc
                    .fragment
                    .as_ref()
                    .and_then(|f| self.shaders.get(&f.module))
                    .unwrap_or(vm);
                let fe = desc.fragment.as_ref().map(|f| f.entry.as_str()).unwrap_or("fs");
                (vm, desc.vertex.entry.as_str(), fm, fe)
            }
            PipeKind::Flat => (&self.flat_module, "vs", &self.flat_module, "fs"),
            PipeKind::Tex => (&self.tex_module, "vs", &self.tex_module, "fs"),
        };

        let depth_stencil = desc.depth.as_ref().map(|d| wgpu::DepthStencilState {
            format: tex_format(d.format),
            depth_write_enabled: d.depth_write,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        });

        let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(desc.label.as_str()),
            layout: None,
            vertex: wgpu::VertexState {
                module: vmodule,
                entry_point: Some(ventry),
                compilation_options: Default::default(),
                buffers: vbuffers_ref,
            },
            fragment: Some(wgpu::FragmentState {
                module: fmodule,
                entry_point: Some(fentry),
                compilation_options: Default::default(),
                targets: &targets,
            }),
            primitive: wgpu::PrimitiveState {
                topology: topology(desc.topology),
                front_face: if desc.front_face == 1 { wgpu::FrontFace::Cw } else { wgpu::FrontFace::Ccw },
                cull_mode: match desc.cull {
                    1 => Some(wgpu::Face::Front),
                    2 => Some(wgpu::Face::Back),
                    _ => None,
                },
                ..Default::default()
            },
            depth_stencil,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        self.render_pipelines.insert(id.0, RenderPipe { pipeline, kind });
        Ok(())
    }

    fn create_compute_pipeline(&mut self, id: PipelineId, desc: &ComputePipelineDesc) -> Result<()> {
        let module = self
            .shaders
            .get(&desc.compute.module)
            .ok_or(GpuError::Unsupported("compute shader not translatable (needs SPIR-V)"))?;
        let pipeline = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(desc.label.as_str()),
            layout: None,
            module,
            entry_point: Some(desc.compute.entry.as_str()),
            compilation_options: Default::default(),
            cache: None,
        });
        self.compute_pipelines.insert(id.0, pipeline);
        Ok(())
    }

    fn destroy_pipeline(&mut self, id: PipelineId) -> Result<()> {
        self.render_pipelines.remove(&id.0);
        self.compute_pipelines.remove(&id.0);
        Ok(())
    }

    fn create_bind_group(&mut self, id: BindGroupId, desc: &BindGroupDesc) -> Result<()> {
        // Deferred: a wgpu bind group needs a concrete layout, which comes from the pipeline it is used
        // with. Store the descriptor and materialize it at draw time against the bound pipeline's layout.
        self.bind_groups.insert(id.0, desc.clone());
        Ok(())
    }

    fn destroy_bind_group(&mut self, id: BindGroupId) -> Result<()> {
        self.bind_groups.remove(&id.0);
        Ok(())
    }

    fn create_fence(&mut self, id: FenceId) -> Result<()> {
        self.fences.insert(id.0, 0);
        Ok(())
    }

    fn destroy_fence(&mut self, id: FenceId) -> Result<()> {
        self.fences.remove(&id.0);
        Ok(())
    }

    fn wait_fence(&mut self, _id: FenceId, _value: u64) -> Result<()> {
        // wgpu has no timeline semaphore. Over-synchronize: block until all submitted work is done. This
        // is conservative but correct — a later increment can track per-fence submission indices.
        let _ = self.device.poll(wgpu::Maintain::Wait);
        Ok(())
    }

    fn submit(&mut self, cb: &CommandBuffer) -> Result<()> {
        let ops = &cb.encoder;

        // --- pass 1: pre-build every bind group referenced by a SetBindGroup, keyed by op index, using
        // the pipeline current at that point. Done before opening any pass so the owned BindGroups outlive
        // the render pass that borrows them (and so we never mutate self during a pass). ---
        let mut built: HashMap<usize, wgpu::BindGroup> = HashMap::new();
        let mut clear_uniforms: HashMap<usize, (wgpu::Buffer, wgpu::BindGroup)> = HashMap::new();
        let mut cur_pipe: Option<u32> = None;
        for (k, op) in ops.iter().enumerate() {
            match op {
                Enc::SetPipeline(p) => cur_pipe = Some(*p),
                Enc::SetBindGroup { group, .. } => {
                    let pid = cur_pipe.ok_or(GpuError::Invalid("SetBindGroup before SetPipeline"))?;
                    let rp = self
                        .render_pipelines
                        .get(&pid)
                        .ok_or(GpuError::UnknownId { kind: "pipeline", id: pid })?;
                    let desc = self
                        .bind_groups
                        .get(group)
                        .ok_or(GpuError::UnknownId { kind: "bind_group", id: *group })?;
                    let layout = rp.pipeline.get_bind_group_layout(desc.set);
                    let bg = self.build_bind_group(&layout, rp.kind, desc)?;
                    built.insert(k, bg);
                }
                Enc::ClearRect { color, .. } => {
                    // Uniform + bind group for the scissored clear draw.
                    let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("dd-clear-color"),
                        contents: bytemuck_color(color),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                    let layout = self.clear_pipeline.get_bind_group_layout(0);
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: None,
                        layout: &layout,
                        entries: &[wgpu::BindGroupEntry {
                            binding: 0,
                            resource: buf.as_entire_binding(),
                        }],
                    });
                    clear_uniforms.insert(k, (buf, bg));
                }
                _ => {}
            }
        }

        // --- pass 2: record the encoder. ---
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("dd-submit") });
        // Staging buffers created to satisfy wgpu's 256-byte row alignment on buffer<->texture copies
        // (Metal has no such rule, so guest IR uses tight rows). They must outlive `enc` until submit.
        let mut keep_alive: Vec<wgpu::Buffer> = Vec::new();
        let mut i = 0usize;
        while i < ops.len() {
            match &ops[i] {
                Enc::BeginRenderPass { color, depth } => {
                    // Resolve attachment views up front.
                    let color_views: Vec<(&TexEntry, &ColorAttachment)> = color
                        .iter()
                        .map(|c| self.resolve_tex(c.texture).map(|t| (t, c)))
                        .collect::<Result<_>>()?;
                    let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = color_views
                        .iter()
                        .map(|(t, c)| {
                            Some(wgpu::RenderPassColorAttachment {
                                view: &t.view,
                                resolve_target: None,
                                ops: wgpu::Operations {
                                    load: match c.load {
                                        LoadOp::Clear => wgpu::LoadOp::Clear(wgpu::Color {
                                            r: c.clear[0] as f64,
                                            g: c.clear[1] as f64,
                                            b: c.clear[2] as f64,
                                            a: c.clear[3] as f64,
                                        }),
                                        _ => wgpu::LoadOp::Load,
                                    },
                                    store: if c.store { wgpu::StoreOp::Store } else { wgpu::StoreOp::Discard },
                                },
                            })
                        })
                        .collect();
                    let depth_entry = match depth {
                        Some(d) => Some((self.resolve_tex(d.texture)?, d)),
                        None => None,
                    };
                    let depth_attachment = depth_entry.as_ref().map(|(t, d)| wgpu::RenderPassDepthStencilAttachment {
                        view: &t.view,
                        depth_ops: Some(wgpu::Operations {
                            load: match d.load {
                                LoadOp::Clear => wgpu::LoadOp::Clear(d.clear_depth),
                                _ => wgpu::LoadOp::Load,
                            },
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    });

                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("dd-render-pass"),
                        color_attachments: &color_attachments,
                        depth_stencil_attachment: depth_attachment,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });

                    // Replay inner ops until EndRenderPass.
                    i += 1;
                    while i < ops.len() && !matches!(ops[i], Enc::EndRenderPass) {
                        match &ops[i] {
                            Enc::SetPipeline(p) => {
                                let rpipe = self
                                    .render_pipelines
                                    .get(p)
                                    .ok_or(GpuError::UnknownId { kind: "pipeline", id: *p })?;
                                rp.set_pipeline(&rpipe.pipeline);
                            }
                            Enc::SetBindGroup { index, .. } => {
                                if let Some(bg) = built.get(&i) {
                                    rp.set_bind_group(*index, bg, &[]);
                                }
                            }
                            Enc::SetVertexBuffer { slot, buffer, offset } => {
                                let buf = self
                                    .buffers
                                    .get(buffer)
                                    .ok_or(GpuError::UnknownId { kind: "buffer", id: *buffer })?;
                                rp.set_vertex_buffer(*slot, buf.slice(*offset..));
                            }
                            Enc::SetIndexBuffer { buffer, offset, format } => {
                                let buf = self
                                    .buffers
                                    .get(buffer)
                                    .ok_or(GpuError::UnknownId { kind: "buffer", id: *buffer })?;
                                let fmt = match format {
                                    IndexFormat::U16 => wgpu::IndexFormat::Uint16,
                                    IndexFormat::U32 => wgpu::IndexFormat::Uint32,
                                };
                                rp.set_index_buffer(buf.slice(*offset..), fmt);
                            }
                            Enc::SetViewport { x, y, w, h, min_depth, max_depth } => {
                                if *w > 0.0 && *h > 0.0 {
                                    rp.set_viewport(*x, *y, *w, *h, *min_depth, *max_depth);
                                }
                            }
                            Enc::SetScissor { x, y, w, h } => {
                                if *w > 0 && *h > 0 {
                                    rp.set_scissor_rect(*x, *y, *w, *h);
                                }
                            }
                            Enc::Draw { vertex_count, instance_count, first_vertex, first_instance } => {
                                rp.draw(
                                    *first_vertex..*first_vertex + *vertex_count,
                                    *first_instance..*first_instance + *instance_count,
                                );
                            }
                            Enc::DrawIndexed { index_count, instance_count, first_index, base_vertex, first_instance } => {
                                rp.draw_indexed(
                                    *first_index..*first_index + *index_count,
                                    *base_vertex,
                                    *first_instance..*first_instance + *instance_count,
                                );
                            }
                            // ClearRect / copies / compute don't occur inside a render pass in the IR.
                            _ => {}
                        }
                        i += 1;
                    }
                    // i now points at EndRenderPass (or end); drop the pass, advance past End.
                    drop(rp);
                }
                Enc::ClearRect { texture, x, y, w, h, .. } => {
                    let t = self.resolve_tex(*texture)?;
                    let (_buf, bg) = clear_uniforms.get(&i).expect("clear uniform prebuilt");
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("dd-clear-rect"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &t.view,
                            resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
                    if *w > 0 && *h > 0 {
                        rp.set_scissor_rect(*x, *y, *w, *h);
                        rp.set_pipeline(&self.clear_pipeline);
                        rp.set_bind_group(0, bg, &[]);
                        rp.draw(0..3, 0..1);
                    }
                }
                Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size } => {
                    let s = self.buffers.get(src).ok_or(GpuError::UnknownId { kind: "buffer", id: *src })?;
                    let d = self.buffers.get(dst).ok_or(GpuError::UnknownId { kind: "buffer", id: *dst })?;
                    enc.copy_buffer_to_buffer(s, *src_offset, d, *dst_offset, *size);
                }
                Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, mip, width, height } => {
                    let s = self.buffers.get(src).ok_or(GpuError::UnknownId { kind: "buffer", id: *src })?;
                    let t = self.resolve_tex(*dst)?;
                    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                    let padded = bytes_per_row.div_ceil(align) * align;
                    // Source rows must be 256-aligned for wgpu. If they aren't, re-pack the guest buffer's
                    // rows into an aligned staging buffer first (row-by-row buffer copy on the GPU).
                    let (copy_buf, row_pitch): (&wgpu::Buffer, u32) = if padded == *bytes_per_row {
                        (s, *bytes_per_row)
                    } else {
                        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("dd-b2t-pad"),
                            size: (padded * *height) as u64,
                            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                            mapped_at_creation: false,
                        });
                        for r in 0..*height {
                            enc.copy_buffer_to_buffer(
                                s,
                                *src_offset + (r * *bytes_per_row) as u64,
                                &staging,
                                (r * padded) as u64,
                                *bytes_per_row as u64,
                            );
                        }
                        keep_alive.push(staging);
                        (keep_alive.last().unwrap(), padded)
                    };
                    let offset = if padded == *bytes_per_row { *src_offset } else { 0 };
                    enc.copy_buffer_to_texture(
                        wgpu::TexelCopyBufferInfo {
                            buffer: copy_buf,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset,
                                bytes_per_row: Some(row_pitch),
                                rows_per_image: Some(*height),
                            },
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: &t.texture,
                            mip_level: *mip,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d { width: *width, height: *height, depth_or_array_layers: 1 },
                    );
                }
                Enc::CopyTextureToBuffer { src, mip, width, height, dst, dst_offset, bytes_per_row } => {
                    let t = self.resolve_tex(*src)?;
                    let d = self.buffers.get(dst).ok_or(GpuError::UnknownId { kind: "buffer", id: *dst })?;
                    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
                    let padded = bytes_per_row.div_ceil(align) * align;
                    if padded == *bytes_per_row {
                        enc.copy_texture_to_buffer(
                            wgpu::TexelCopyTextureInfo {
                                texture: &t.texture,
                                mip_level: *mip,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyBufferInfo {
                                buffer: d,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: *dst_offset,
                                    bytes_per_row: Some(*bytes_per_row),
                                    rows_per_image: Some(*height),
                                },
                            },
                            wgpu::Extent3d { width: *width, height: *height, depth_or_array_layers: 1 },
                        );
                    } else {
                        // Copy to an aligned staging buffer, then re-pack rows tightly into the guest dst.
                        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("dd-t2b-pad"),
                            size: (padded * *height) as u64,
                            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                            mapped_at_creation: false,
                        });
                        enc.copy_texture_to_buffer(
                            wgpu::TexelCopyTextureInfo {
                                texture: &t.texture,
                                mip_level: *mip,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyBufferInfo {
                                buffer: &staging,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(padded),
                                    rows_per_image: Some(*height),
                                },
                            },
                            wgpu::Extent3d { width: *width, height: *height, depth_or_array_layers: 1 },
                        );
                        for r in 0..*height {
                            enc.copy_buffer_to_buffer(
                                &staging,
                                (r * padded) as u64,
                                d,
                                *dst_offset + (r * *bytes_per_row) as u64,
                                *bytes_per_row as u64,
                            );
                        }
                        keep_alive.push(staging);
                    }
                }
                Enc::BeginComputePass => {
                    let mut cp = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("dd-compute"),
                        timestamp_writes: None,
                    });
                    i += 1;
                    while i < ops.len() && !matches!(ops[i], Enc::EndComputePass) {
                        match &ops[i] {
                            Enc::SetPipeline(p) => {
                                let cpipe = self
                                    .compute_pipelines
                                    .get(p)
                                    .ok_or(GpuError::UnknownId { kind: "pipeline", id: *p })?;
                                cp.set_pipeline(cpipe);
                            }
                            Enc::SetBindGroup { index, .. } => {
                                if let Some(bg) = built.get(&i) {
                                    cp.set_bind_group(*index, bg, &[]);
                                }
                            }
                            Enc::Dispatch { x, y, z } => cp.dispatch_workgroups(*x, *y, *z),
                            _ => {}
                        }
                        i += 1;
                    }
                    drop(cp);
                }
                // EndRenderPass / EndComputePass consumed inline; stray ones are no-ops.
                _ => {}
            }
            i += 1;
        }

        let idx = self.queue.submit([enc.finish()]);
        self.last_submission = Some(idx);
        if let Some((fence, value)) = cb.signal {
            self.fences.insert(fence, value);
        }
        Ok(())
    }

    fn present(&mut self, _surface: SurfaceId, _texture: TextureId) -> Result<PresentToken> {
        // Present goes through the readback fallback (or, once wired, zero-copy IOSurface). The executor
        // owns the surface<->IOSurface mapping; this trait method is not the wgpu present seam yet.
        Err(GpuError::Unsupported("present (use set_render_target + read_target for the readback path)"))
    }
}

/// Reinterpret a `[f32;4]` clear color as bytes for a uniform upload (no bytemuck dependency).
fn bytemuck_color(c: &[f32; 4]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(c.as_ptr() as *const u8, 16) }
}
