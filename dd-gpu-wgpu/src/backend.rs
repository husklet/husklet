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
use std::hash::{Hash, Hasher};
use std::num::NonZeroU64;

use dd_gpu::backend::{Capabilities, GpuBackend, PresentKind, PresentToken};
use dd_gpu::id::*;
use dd_gpu::ir::*;
use dd_gpu::{GpuError, Result};

use wgpu::util::DeviceExt as _;

/// Content hash of a byte slice via the std default hasher — content-keys the L3 shader/pipeline caches
/// (mirrors `metal_backend::hash_bytes`).
fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

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

// Fullscreen-triangle VERTICAL flip: samples `src` with V inverted, so it converts a top-left-origin
// render into the bottom-left (GL-convention) storage the bespoke Metal replay produces via a negative-
// height viewport. wgpu forbids negative viewport heights, so offscreen render-target passes (a Chrome
// content tile, an FBO) render upright into a scratch texture and are flipped into the real target by a
// draw with this pipeline. uv.y = (clip.y+1)/2 maps screen-top (clip.y=+1) to the source BOTTOM row.
const FLIP_WGSL: &str = r#"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -3.0), vec2<f32>(-1.0, 1.0), vec2<f32>(3.0, 1.0));
    var o: VOut;
    let cp = p[vi];
    o.pos = vec4<f32>(cp, 0.0, 1.0);
    o.uv = vec2<f32>((cp.x + 1.0) * 0.5, (cp.y + 1.0) * 0.5);
    return o;
}
@fragment
fn fs(i: VOut) -> @location(0) vec4<f32> { return textureSample(src, samp, i.uv); }
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

impl PipeKind {
    fn tag(self) -> u8 {
        match self {
            PipeKind::Flat => 0,
            PipeKind::Tex => 1,
            PipeKind::App => 2,
        }
    }
}

#[derive(Clone)]
struct RenderPipe {
    pipeline: wgpu::RenderPipeline,
    kind: PipeKind,
}

/// Cache key for a materialized `wgpu::BindGroup` (L3 steady-state cache). A cached group holds `Arc`
/// handles to the exact wgpu resources it was built from, so the key must change whenever any of them
/// could have been replaced: `epoch` bumps on every buffer/texture/sampler create/destroy and on any
/// bind-group descriptor change, guaranteeing a stale group is never reused. `kind`/`set` disambiguate the
/// same bind-group id used against different pipeline layouts within one frame.
#[derive(Clone, PartialEq, Eq, Hash)]
struct BindGroupKey {
    id: u32,
    set: u32,
    kind: u8,
    epoch: u64,
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
    /// Scissored-clear (`ClearRect`) pipelines, one per render-target format. The attachment format must
    /// match the pipeline's color target, so a BGRA IOSurface present surface needs its own variant (the
    /// original hardcoded `Rgba8Unorm` pipeline errored on a BGRA target). Keyed by wgpu format.
    clear_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    /// Vertical-flip pipelines (offscreen Y-flip), one per render-target format (same format-match rule).
    flip_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    /// Shared modules for lazily building extra clear/flip format variants on demand.
    clear_module: wgpu::ShaderModule,
    flip_module: wgpu::ShaderModule,
    flip_sampler: wgpu::Sampler,
    /// Per offscreen-target scratch textures the flip renders through (reused across frames by target id).
    flip_scratch: HashMap<u32, TexEntry>,

    /// Fence values reached, and the submission index that will reach them. `wait_fence` polls to Wait.
    last_submission: Option<wgpu::SubmissionIndex>,
    fences: HashMap<u32, u64>,
    /// Ids that are the presented surface (top-left, not offscreen). Injected via `set_render_target`.
    surface_ids: std::collections::HashSet<u32>,

    // ---- L3 content-keyed caches (parity with metal_backend's shader_lib_cache / pipeline_cache) ----
    // The GL shim re-emits CreateShader + CreateRenderPipeline EVERY frame with byte-identical content, so
    // without these the two heaviest host calls (naga SPIR-V→WGSL translation + `create_render_pipeline`
    // shader compile/link) run every frame even though the backend persists. Keying by content hash makes
    // them cache hits after warmup → zero translations/compiles per steady-state frame; a genuine content
    // change (same id, new bytes) misses the cache and correctly rebuilds.
    /// naga-translated modules keyed by SPIR-V content hash (shared across ids that ship the same bytes).
    shader_content_cache: HashMap<u64, wgpu::ShaderModule>,
    /// Content hashes whose translation failed / was non-SPIR-V — a hit skips retrying naga and falls back
    /// to the builtin WGSL pipeline (the guest is mid-migration to the SPIR-V ABI).
    failed_shader_cache: std::collections::HashSet<u64>,
    /// Shader id → content hash currently installed (folded into the pipeline key so a recompiled shader
    /// under the same id forces a pipeline rebuild even when the descriptor bytes are unchanged).
    shader_id_hash: HashMap<u32, u64>,
    /// Compiled render pipelines keyed by descriptor content hash (`hash_render_pipeline_key`).
    render_pipeline_cache: HashMap<u64, RenderPipe>,
    /// Compiled compute pipelines keyed by descriptor content hash.
    compute_pipeline_cache: HashMap<u64, wgpu::ComputePipeline>,
    /// Pipeline id → descriptor hash currently installed (skip re-install when the guest re-emits identical).
    pipeline_id_hash: HashMap<u32, u64>,
    /// Materialized bind groups reused across submits (see [`BindGroupKey`]).
    bind_group_cache: HashMap<BindGroupKey, wgpu::BindGroup>,
    /// Monotonic resource generation; bumped on any buffer/texture/sampler create/destroy and bind-group
    /// descriptor change. Part of every [`BindGroupKey`] so a cached group can't outlive its resources.
    res_epoch: u64,
    /// Live present-path IOSurface wrap cache: raw `MTLTexture` pointer → wrapped `wgpu::Texture`, so
    /// re-registering the same surface as the present target each frame reuses the hal texture instead of
    /// rebuilding it (`texture_from_hal` per frame).
    iosurface_wraps: HashMap<usize, wgpu::Texture>,
    /// Raw `*mut MTLCommandQueue` the wgpu render work is submitted on, as a `usize` (a raw ptr isn't
    /// `Send`; used only on the executor thread that built the backend). Non-zero ONLY for a backend built
    /// via [`WgpuBackend::from_shared_mtl_device`] — the live tear-free present path encodes the cross-queue
    /// `MTLEvent` render/present fence on THIS exact queue so its wait/signal serialize (Metal orders
    /// command buffers committed to one queue) against wgpu's render command buffers. `0` = own-device
    /// backend (offscreen `new()`), which has no shared compositor queue to fence against.
    present_queue_raw: usize,

    // Prof counters — count only ACTUAL host work (cache misses). The steady-state regression guard is that
    // they read 0 after warmup. Read + reset by the executor when its debug/prof output is enabled.
    pub shader_compiles: u32,
    pub pipeline_compiles: u32,
    pub bind_group_builds: u32,
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
        let flip_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dd-flip"),
            source: wgpu::ShaderSource::Wgsl(FLIP_WGSL.into()),
        });
        let flip_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("dd-flip-samp"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        // Prebuild the two render-target formats that actually occur (RGBA8 offscreen/goldens, BGRA8
        // IOSurface present surface); rarer formats are materialized lazily in `clear_pipeline_for` /
        // `flip_pipeline_for`.
        let mut clear_pipelines = HashMap::new();
        let mut flip_pipelines = HashMap::new();
        for f in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Bgra8Unorm] {
            clear_pipelines.insert(f, Self::make_clear_pipeline(&device, &clear_module, f));
            flip_pipelines.insert(f, Self::make_flip_pipeline(&device, &flip_module, f));
        }
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
            clear_pipelines,
            flip_pipelines,
            clear_module,
            flip_module,
            flip_sampler,
            flip_scratch: HashMap::new(),
            last_submission: None,
            fences: HashMap::new(),
            surface_ids: std::collections::HashSet::new(),
            shader_content_cache: HashMap::new(),
            failed_shader_cache: std::collections::HashSet::new(),
            shader_id_hash: HashMap::new(),
            render_pipeline_cache: HashMap::new(),
            compute_pipeline_cache: HashMap::new(),
            pipeline_id_hash: HashMap::new(),
            bind_group_cache: HashMap::new(),
            res_epoch: 0,
            iosurface_wraps: HashMap::new(),
            present_queue_raw: 0,
            shader_compiles: 0,
            pipeline_compiles: 0,
            bind_group_builds: 0,
        }
    }

    /// Build a wgpu backend whose `Device`/`Queue` run OVER an existing, externally-owned `MTLDevice` —
    /// dd-display's process-shared device (`crate::metal::shared_device()` on the dd side). This is what
    /// makes the live present path tear-free WITHOUT a blocking poll: because the wgpu render queue and
    /// dd-display's compositor blit queue then live on the SAME `MTLDevice`, one `MTLEvent` fences
    /// render→blit across the two queues (the executor signals render-complete, the compositor's
    /// `blit_fenced` waits on it), exactly mirroring the bespoke Metal replay's `render_ev`→`present_ev`
    /// handoff. An own-device backend ([`WgpuBackend::new`]) can't do this: `MTLEvent`s are device-scoped,
    /// so a fence is only meaningful when producer and consumer share the device object.
    ///
    /// This is the inverse of the `iosurface_interop` example (which extracts wgpu's OWN raw `MTLDevice`);
    /// here we inject dd's device INTO wgpu via wgpu-hal `device_from_raw` + `queue_from_raw`, wrapped back
    /// into a `wgpu::Device`/`Queue` through `Adapter::create_device_from_hal`.
    ///
    /// # Safety
    /// `raw_device` must be a valid, live `MTLDevice` (an objc2 `ProtocolObject<dyn MTLDevice>` pointer).
    pub unsafe fn from_shared_mtl_device(raw_device: *mut std::ffi::c_void) -> Result<Self> {
        use metal::foreign_types::{ForeignType as _, ForeignTypeRef as _};
        if raw_device.is_null() {
            return Err(GpuError::Unsupported("null MTLDevice"));
        }
        // metal-rs view of the shared device (`to_owned` = retain +1 → an owned handle wgpu-hal releases on
        // drop; dd-display keeps its own ref, so the device outlives both).
        let metal_device: metal::Device = metal::DeviceRef::from_ptr(raw_device.cast()).to_owned();
        // The single command queue the wgpu render work runs on. We keep its raw pointer (below) so the
        // executor can encode the cross-queue fence's wait/signal on the SAME queue wgpu submits render on.
        let raw_queue: metal::CommandQueue = metal_device.new_command_queue();
        let present_queue_raw = raw_queue.as_ptr() as usize;

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..Default::default()
        });
        // A normal adapter over the system-default GPU. On Apple Silicon this IS the same GPU as
        // `raw_device`, so its reported limits/features are valid for the device we actually run on. Its
        // device object is NOT used for submission — the `OpenDevice` below binds the wgpu `Device` to
        // `raw_device` itself (wgpu-core's `Device::new` uses the supplied hal device, the adapter only for
        // capability/limit queries).
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .ok_or(GpuError::Unsupported("no Metal adapter"))?;

        let features = wgpu::Features::empty();
        let hal_device = wgpu_hal::metal::Device::device_from_raw(metal_device, features);
        // timestamp_period 1.0 = Apple Silicon (matches wgpu-hal's own `Adapter::open`); we don't use GPU
        // timestamps, so the exact value is immaterial.
        let hal_queue = wgpu_hal::metal::Queue::queue_from_raw(raw_queue, 1.0);
        let open = wgpu_hal::OpenDevice { device: hal_device, queue: hal_queue };
        let (device, queue) = adapter
            .create_device_from_hal::<wgpu_hal::api::Metal>(
                open,
                &wgpu::DeviceDescriptor {
                    label: Some("dd-gpu-wgpu-shared"),
                    required_features: features,
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .map_err(|_| GpuError::Unsupported("create_device_from_hal"))?;

        let mut be = Self::from_shared(device, queue);
        be.present_queue_raw = present_queue_raw;
        Ok(be)
    }

    /// Raw `*mut MTLCommandQueue` the wgpu render work is submitted on — the queue [`run_executor_wgpu`]
    /// encodes the cross-queue tearing fence on. Non-null ONLY for a [`from_shared_mtl_device`] backend
    /// (null for the offscreen own-device `new()` path, which has no compositor queue to fence against).
    ///
    /// [`from_shared_mtl_device`]: WgpuBackend::from_shared_mtl_device
    pub fn raw_mtl_queue(&self) -> *mut std::ffi::c_void {
        self.present_queue_raw as *mut std::ffi::c_void
    }

    fn make_clear_pipeline(
        device: &wgpu::Device,
        module: &wgpu::ShaderModule,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dd-clear-pipe"),
            layout: None,
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
    }

    fn make_flip_pipeline(
        device: &wgpu::Device,
        module: &wgpu::ShaderModule,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dd-flip-pipe"),
            layout: None,
            vertex: wgpu::VertexState {
                module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
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

    /// Advance the resource generation (invalidates bind groups keyed to the old epoch). Called on every
    /// buffer/texture/sampler create/destroy and bind-group descriptor change. Bounds the bind-group cache:
    /// if a guest churns resources every frame the cache would otherwise accumulate dead entries, so once it
    /// grows past a cap we drop the superseded generations (steady state keeps one epoch and never trips it).
    fn bump_epoch(&mut self) {
        self.res_epoch = self.res_epoch.wrapping_add(1);
        if self.bind_group_cache.len() > 4096 {
            let cur = self.res_epoch;
            self.bind_group_cache.retain(|k, _| k.epoch == cur);
        }
    }

    /// Read and reset the per-frame prof counters (host cache misses since the last call). The executor
    /// calls this once per replayed frame; a steady-state frame reads `(0, 0, 0)`.
    pub fn take_prof(&mut self) -> (u32, u32, u32) {
        let out = (self.shader_compiles, self.pipeline_compiles, self.bind_group_builds);
        self.shader_compiles = 0;
        self.pipeline_compiles = 0;
        self.bind_group_builds = 0;
        out
    }

    /// Content hash of a render-pipeline descriptor for the L3 PSO cache. Folds in the installed
    /// shader-content hash of each referenced module so a recompiled shader (same id, new bytes) forces a
    /// pipeline rebuild even when the descriptor bytes are unchanged (mirrors
    /// `metal_backend::hash_pipeline_key`).
    fn hash_render_pipeline_key(&self, desc: &RenderPipelineDesc) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        desc.vertex.module.hash(&mut h);
        desc.vertex.entry.hash(&mut h);
        self.shader_id_hash.get(&desc.vertex.module).copied().unwrap_or(0).hash(&mut h);
        match &desc.fragment {
            Some(f) => {
                1u8.hash(&mut h);
                f.module.hash(&mut h);
                f.entry.hash(&mut h);
                self.shader_id_hash.get(&f.module).copied().unwrap_or(0).hash(&mut h);
            }
            None => 0u8.hash(&mut h),
        }
        for l in &desc.vertex_buffers {
            l.stride.hash(&mut h);
            l.step_mode.hash(&mut h);
            for a in &l.attrs {
                a.location.hash(&mut h);
                a.format.hash(&mut h);
                a.offset.hash(&mut h);
            }
        }
        for c in &desc.color_targets {
            c.format.to_u32().hash(&mut h);
            c.write_mask.hash(&mut h);
            match &c.blend {
                Some(b) => {
                    1u8.hash(&mut h);
                    b.src_color.hash(&mut h);
                    b.dst_color.hash(&mut h);
                    b.op_color.hash(&mut h);
                    b.src_alpha.hash(&mut h);
                    b.dst_alpha.hash(&mut h);
                    b.op_alpha.hash(&mut h);
                }
                None => 0u8.hash(&mut h),
            }
        }
        match &desc.depth {
            Some(dp) => {
                1u8.hash(&mut h);
                dp.format.to_u32().hash(&mut h);
                dp.depth_write.hash(&mut h);
                dp.depth_compare.hash(&mut h);
            }
            None => 0u8.hash(&mut h),
        }
        desc.topology.to_u32().hash(&mut h);
        desc.cull.hash(&mut h);
        desc.front_face.hash(&mut h);
        h.finish()
    }

    /// Ensure a clear + flip pipeline exists for render-target `format` (materialized lazily for formats
    /// beyond the RGBA8/BGRA8 prebuilt in `from_shared`). Called from the `submit` pre-scan so the hot
    /// recording path only ever reads the maps.
    fn ensure_pipelines_for(&mut self, format: TextureFormat) {
        let wf = tex_format(format);
        if is_depth(format) {
            return;
        }
        if !self.clear_pipelines.contains_key(&wf) {
            let p = Self::make_clear_pipeline(&self.device, &self.clear_module, wf);
            self.clear_pipelines.insert(wf, p);
        }
        if !self.flip_pipelines.contains_key(&wf) {
            let p = Self::make_flip_pipeline(&self.device, &self.flip_module, wf);
            self.flip_pipelines.insert(wf, p);
        }
    }

    fn clear_pipeline_for(&self, format: TextureFormat) -> &wgpu::RenderPipeline {
        let wf = tex_format(format);
        self.clear_pipelines
            .get(&wf)
            .unwrap_or_else(|| &self.clear_pipelines[&wgpu::TextureFormat::Rgba8Unorm])
    }

    fn flip_pipeline_for(&self, format: TextureFormat) -> &wgpu::RenderPipeline {
        let wf = tex_format(format);
        self.flip_pipelines
            .get(&wf)
            .unwrap_or_else(|| &self.flip_pipelines[&wgpu::TextureFormat::Rgba8Unorm])
    }

    /// Ensure a scratch render texture for offscreen target `id` (matching size/format). Offscreen passes
    /// render upright into this scratch, then a flip draw copies it into the real target Y-mirrored (see
    /// `encode_flip`) — wgpu's stand-in for the Metal replay's negative-height viewport trick.
    fn ensure_flip_scratch(&mut self, id: u32, w: u32, h: u32, format: TextureFormat) {
        let stale = match self.flip_scratch.get(&id) {
            Some(t) => t.width != w || t.height != h || t.format != format,
            None => true,
        };
        if !stale {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dd-flip-scratch"),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: tex_format(format),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.flip_scratch.insert(id, TexEntry { texture, view, format, width: w, height: h });
    }

    /// Encode a full-target vertical-flip draw: sample `src` (V-inverted) into `dst`. Both must be the
    /// same size/format. Used to convert an upright offscreen render into GL-convention storage and back.
    fn encode_flip(&self, enc: &mut wgpu::CommandEncoder, src: &TexEntry, dst: &TexEntry) {
        let pipeline = self.flip_pipeline_for(dst.format);
        let layout = pipeline.get_bind_group_layout(0);
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dd-flip-bg"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.flip_sampler) },
            ],
        });
        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dd-flip-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &dst.view,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        rp.set_pipeline(pipeline);
        rp.set_bind_group(0, &bg, &[]);
        rp.draw(0..3, 0..1);
    }

    // ---------------------------------------------------------------------------------------------------
    // Live present-path interop (zero-copy IOSurface). The recipe is proven in
    // examples/iosurface_interop.rs; these methods make it callable from dd-display's executor without
    // dd-display depending on wgpu-hal/metal directly.
    // ---------------------------------------------------------------------------------------------------

    /// The raw `*mut MTLDevice` (objc2 `ProtocolObject<dyn MTLDevice>` pointer) underlying this backend's
    /// `wgpu::Device`. dd-display retains it and wraps the guest IOSurface as an `MTLTexture` on the SAME
    /// device, so the wgpu render and the compositor blit share one device — no cross-device copy.
    pub fn raw_mtl_device(&self) -> *mut std::ffi::c_void {
        use metal::foreign_types::ForeignType as _;
        unsafe {
            self.device.as_hal::<wgpu_hal::api::Metal, _, _>(|hal| {
                hal.map(|h| h.raw_device().lock().as_ptr() as *mut std::ffi::c_void)
                    .unwrap_or(std::ptr::null_mut())
            })
        }
    }

    /// Bridge an IOSurface-backed `MTLTexture` (raw objc2 `*mut` created on `raw_mtl_device()`) into a
    /// `wgpu::Texture` and register it as executor render target `id`. `raw_tex` must stay retained by the
    /// caller for as long as this target is used (the IOSurface's pages are its storage).
    ///
    /// # Safety
    /// `raw_tex` must be a valid, live `MTLTexture` of the given `format`/size created on this device.
    pub unsafe fn wrap_iosurface_texture(
        &mut self,
        id: u32,
        raw_tex: *mut std::ffi::c_void,
        w: u32,
        h: u32,
        format: TextureFormat,
    ) {
        let wf = tex_format(format);
        // The compositor caches one MTLTexture per guest IOSurface id for the whole connection, so the raw
        // pointer is stable across frames for a given surface. Reuse the wgpu wrap (an `Arc`-backed handle)
        // instead of rebuilding it via `texture_from_hal` each present — the live path re-registers the
        // present target every frame. A new pointer (surface resized / re-cached) rebuilds and evicts.
        let cache_key = raw_tex as usize;
        let wgpu_tex = if let Some(t) = self.iosurface_wraps.get(&cache_key) {
            if t.width() == w && t.height() == h && t.format() == wf {
                t.clone()
            } else {
                self.iosurface_wraps.remove(&cache_key);
                self.build_iosurface_wrap(cache_key, raw_tex, wf, w, h)
            }
        } else {
            self.build_iosurface_wrap(cache_key, raw_tex, wf, w, h)
        };
        self.set_render_target(id, wgpu_tex, format);
    }

    /// Wrap a raw `MTLTexture` as a `wgpu::Texture` via wgpu-hal and memoize it under `cache_key` (the raw
    /// pointer). Split out of [`wrap_iosurface_texture`] so the per-frame path can hit the cache.
    ///
    /// # Safety
    /// `raw_tex` must be a valid, live `MTLTexture` of `wf`/size created on this device.
    unsafe fn build_iosurface_wrap(
        &mut self,
        cache_key: usize,
        raw_tex: *mut std::ffi::c_void,
        wf: wgpu::TextureFormat,
        w: u32,
        h: u32,
    ) -> wgpu::Texture {
        use metal::foreign_types::ForeignTypeRef as _;
        // Borrow the raw id, then `to_owned` (retain +1) so wgpu-hal owns an independent handle it releases
        // on drop — exactly the objc2 <-> metal-rs seam from examples/iosurface_interop.rs.
        let mtl_texture: metal::Texture = metal::TextureRef::from_ptr(raw_tex.cast()).to_owned();
        let hal_texture = wgpu_hal::metal::Device::texture_from_raw(
            mtl_texture,
            wf,
            metal::MTLTextureType::D2,
            1,
            1,
            wgpu_hal::CopyExtent { width: w, height: h, depth: 1 },
        );
        let wgpu_tex = self.device.create_texture_from_hal::<wgpu_hal::api::Metal>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("dd-iosurface-target"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wf,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
        );
        self.iosurface_wraps.insert(cache_key, wgpu_tex.clone());
        wgpu_tex
    }

    /// Block until all submitted GPU work has completed. The live present path polls this before acking the
    /// guest frame so the compositor's (unfenced) blit reads a fully-rendered IOSurface.
    pub fn poll_wait(&self) {
        let _ = self.device.poll(wgpu::Maintain::Wait);
    }

    /// Create a wgpu-owned render target of `format` and register it as executor target `id` (present
    /// surface). Convenience for host-side harnesses (the golden replay) that verify against `read_target`
    /// without themselves depending on wgpu — mirrors the IOSurface target the live path registers.
    pub fn create_render_target(&mut self, id: u32, w: u32, h: u32, format: TextureFormat) {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dd-render-target"),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: tex_format(format),
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.set_render_target(id, texture, format);
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
        self.bump_epoch();
        Ok(())
    }

    fn destroy_buffer(&mut self, id: BufferId) -> Result<()> {
        let r = self.buffers.remove(&id.0).ok_or(GpuError::UnknownId { kind: "buffer", id: id.0 }).map(|_| ());
        self.bump_epoch();
        r
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
        self.bump_epoch();
        Ok(())
    }

    fn destroy_texture(&mut self, id: TextureId) -> Result<()> {
        let r = self.textures.remove(&id.0).ok_or(GpuError::UnknownId { kind: "texture", id: id.0 }).map(|_| ());
        self.bump_epoch();
        r
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
        self.bump_epoch();
        Ok(())
    }

    fn destroy_sampler(&mut self, id: SamplerId) -> Result<()> {
        self.samplers.remove(&id.0);
        self.bump_epoch();
        Ok(())
    }

    fn create_shader(&mut self, id: ShaderId, spirv: &[u32]) -> Result<()> {
        // L3 content-key: the shim re-emits identical SPIR-V under the same id every frame. Hash the bytes
        // and skip all work when this id already has this content installed; otherwise consult the shared
        // content cache before paying for a naga translation.
        let key = hash_bytes(bytemuck_u32_bytes(spirv));
        if self.shader_id_hash.get(&id.0) == Some(&key) {
            return Ok(()); // identical content already installed under this id — nothing to do
        }
        self.shader_id_hash.insert(id.0, key);

        // Cached translated module for this content → reuse (no naga, no compile).
        if let Some(module) = self.shader_content_cache.get(&key) {
            self.shaders.insert(id.0, module.clone());
            return Ok(());
        }
        // Cached failure → fall back to the builtin WGSL pipeline without retrying naga.
        if self.failed_shader_cache.contains(&key) {
            self.shaders.remove(&id.0);
            return Ok(());
        }

        match crate::shader::spirv_to_wgsl(spirv) {
            Ok(Some(wgsl)) => {
                let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("dd-app-shader"),
                    source: wgpu::ShaderSource::Wgsl(wgsl.into()),
                });
                self.shader_content_cache.insert(key, module.clone());
                self.shaders.insert(id.0, module);
                self.shader_compiles += 1;
            }
            // Not SPIR-V (legacy MSL-as-bytes / opaque): drop any prior module so pipelines fall back to
            // the builtin WGSL. Not an error — the guest is mid-migration to the SPIR-V ABI.
            Ok(None) => {
                self.failed_shader_cache.insert(key);
                self.shaders.remove(&id.0);
            }
            Err(e) => {
                eprintln!("dd-gpu-wgpu: shader {} translation failed: {e}", id.0);
                self.failed_shader_cache.insert(key);
                self.shaders.remove(&id.0);
            }
        }
        Ok(())
    }

    fn destroy_shader(&mut self, id: ShaderId) -> Result<()> {
        self.shaders.remove(&id.0);
        // Forget the installed-content marker so a later create under this id re-installs (the shared
        // content cache keeps the compiled module for reuse; only the id→hash association is cleared).
        self.shader_id_hash.remove(&id.0);
        Ok(())
    }

    fn create_render_pipeline(&mut self, id: PipelineId, desc: &RenderPipelineDesc) -> Result<()> {
        // L3 content-key: the shim re-emits an identical pipeline descriptor under the same id every frame.
        // Skip entirely when this id already has this descriptor installed; otherwise reuse a compiled
        // pipeline from the content cache before paying for `create_render_pipeline` (shader compile/link).
        let key = self.hash_render_pipeline_key(desc);
        if self.pipeline_id_hash.get(&id.0) == Some(&key) {
            return Ok(());
        }
        if let Some(rp) = self.render_pipeline_cache.get(&key) {
            self.render_pipelines.insert(id.0, rp.clone());
            self.pipeline_id_hash.insert(id.0, key);
            return Ok(());
        }
        self.pipeline_compiles += 1;

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
        let rp = RenderPipe { pipeline, kind };
        self.render_pipeline_cache.insert(key, rp.clone());
        self.render_pipelines.insert(id.0, rp);
        self.pipeline_id_hash.insert(id.0, key);
        Ok(())
    }

    fn create_compute_pipeline(&mut self, id: PipelineId, desc: &ComputePipelineDesc) -> Result<()> {
        // L3 content-key (compute): fold the module id + its installed shader-content hash + entry point.
        let mut h = std::collections::hash_map::DefaultHasher::new();
        desc.compute.module.hash(&mut h);
        desc.compute.entry.hash(&mut h);
        self.shader_id_hash.get(&desc.compute.module).copied().unwrap_or(0).hash(&mut h);
        let key = h.finish();
        if self.pipeline_id_hash.get(&id.0) == Some(&key) {
            return Ok(());
        }
        if let Some(p) = self.compute_pipeline_cache.get(&key) {
            self.compute_pipelines.insert(id.0, p.clone());
            self.pipeline_id_hash.insert(id.0, key);
            return Ok(());
        }
        self.pipeline_compiles += 1;
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
        self.compute_pipeline_cache.insert(key, pipeline.clone());
        self.compute_pipelines.insert(id.0, pipeline);
        self.pipeline_id_hash.insert(id.0, key);
        Ok(())
    }

    fn destroy_pipeline(&mut self, id: PipelineId) -> Result<()> {
        self.render_pipelines.remove(&id.0);
        self.compute_pipelines.remove(&id.0);
        // Clear the installed-content marker so a later create under this id re-installs from the cache.
        self.pipeline_id_hash.remove(&id.0);
        Ok(())
    }

    fn create_bind_group(&mut self, id: BindGroupId, desc: &BindGroupDesc) -> Result<()> {
        // Deferred: a wgpu bind group needs a concrete layout, which comes from the pipeline it is used
        // with. Store the descriptor and materialize it at draw time against the bound pipeline's layout.
        self.bind_groups.insert(id.0, desc.clone());
        // A new descriptor under this id invalidates any materialized group keyed by the old epoch.
        self.bump_epoch();
        Ok(())
    }

    fn destroy_bind_group(&mut self, id: BindGroupId) -> Result<()> {
        self.bind_groups.remove(&id.0);
        self.bump_epoch();
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

        // --- pass 0 (mut): materialize any per-format clear/flip pipelines and offscreen flip scratch
        // textures this command buffer needs, so the borrow-only recording passes below never mutate self.
        let mut scratch_needed: Vec<(u32, u32, u32, TextureFormat)> = Vec::new();
        let mut clear_fmts: Vec<TextureFormat> = Vec::new();
        for op in ops.iter() {
            match op {
                Enc::BeginRenderPass { color, .. } => {
                    for c in color {
                        let t = self.resolve_tex(c.texture)?;
                        let (fmt, w, h) = (t.format, t.width, t.height);
                        clear_fmts.push(fmt);
                        // Offscreen (not the presented surface) → needs a Y-flip scratch.
                        if !self.surface_ids.contains(&c.texture) {
                            scratch_needed.push((c.texture, w, h, fmt));
                        }
                    }
                }
                Enc::ClearRect { texture, .. } => {
                    clear_fmts.push(self.resolve_tex(*texture)?.format);
                }
                _ => {}
            }
        }
        for f in clear_fmts {
            self.ensure_pipelines_for(f);
        }
        for (id, w, h, f) in scratch_needed {
            self.ensure_flip_scratch(id, w, h, f);
        }

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
                    // Resolve everything into owned locals so the immutable borrows of `self` end before the
                    // cache mutation below.
                    let (kind, layout, desc, cacheable, key) = {
                        let rp = self
                            .render_pipelines
                            .get(&pid)
                            .ok_or(GpuError::UnknownId { kind: "pipeline", id: pid })?;
                        let desc = self
                            .bind_groups
                            .get(group)
                            .ok_or(GpuError::UnknownId { kind: "bind_group", id: *group })?;
                        let layout = rp.pipeline.get_bind_group_layout(desc.set);
                        // Never cache a group that binds the presented surface: the executor re-registers the
                        // present target (a fresh view) every frame, so a cached group would hold a stale one.
                        let cacheable = !desc.entries.iter().any(|e| {
                            matches!(&e.resource, BindResource::Texture { id } if self.surface_ids.contains(id))
                        });
                        let key = BindGroupKey {
                            id: *group,
                            set: desc.set,
                            kind: rp.kind.tag(),
                            epoch: self.res_epoch,
                        };
                        (rp.kind, layout, desc.clone(), cacheable, key)
                    };
                    let bg = if cacheable {
                        if let Some(cached) = self.bind_group_cache.get(&key) {
                            cached.clone()
                        } else {
                            let bg = self.build_bind_group(&layout, kind, &desc)?;
                            self.bind_group_cache.insert(key, bg.clone());
                            self.bind_group_builds += 1;
                            bg
                        }
                    } else {
                        self.bind_group_builds += 1;
                        self.build_bind_group(&layout, kind, &desc)?
                    };
                    built.insert(k, bg);
                }
                Enc::ClearRect { color, texture, .. } => {
                    // Uniform + bind group for the scissored clear draw. The bind-group layout is identical
                    // across clear-pipeline format variants (same shader), so any pipeline's layout works;
                    // use the target's own so the same format is exercised end to end.
                    let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("dd-clear-color"),
                        contents: bytemuck_color(color),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                    let fmt = self.resolve_tex(*texture)?.format;
                    let layout = self.clear_pipeline_for(fmt).get_bind_group_layout(0);
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
                    // Resolve attachment views up front. Offscreen targets (not the presented surface) are
                    // rendered into their flip scratch and Y-mirrored into the real target after the pass
                    // (`post_flips`); a LoadOp::Load offscreen pass first mirrors the target INTO the scratch
                    // so the load sees the existing content in scratch (upright) space.
                    let mut color_views: Vec<&TexEntry> = Vec::with_capacity(color.len());
                    let mut post_flips: Vec<u32> = Vec::new();
                    for c in color.iter() {
                        let target = self.resolve_tex(c.texture)?;
                        if self.surface_ids.contains(&c.texture) {
                            color_views.push(target);
                        } else {
                            let scratch = self
                                .flip_scratch
                                .get(&c.texture)
                                .ok_or(GpuError::UnknownId { kind: "flip-scratch", id: c.texture })?;
                            if matches!(c.load, LoadOp::Load) {
                                self.encode_flip(&mut enc, target, scratch);
                            }
                            color_views.push(scratch);
                            post_flips.push(c.texture);
                        }
                    }
                    let color_attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = color_views
                        .iter()
                        .zip(color.iter())
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
                    // Y-mirror each offscreen scratch into its real target (GL-convention storage), so a
                    // later `texture()` sample of it lands upright — matching the Metal replay's flip.
                    for id in post_flips {
                        let scratch = self
                            .flip_scratch
                            .get(&id)
                            .ok_or(GpuError::UnknownId { kind: "flip-scratch", id })?;
                        let target = self.resolve_tex(id)?;
                        self.encode_flip(&mut enc, scratch, target);
                    }
                }
                Enc::ClearRect { texture, x, y, w, h, .. } => {
                    let t = self.resolve_tex(*texture)?;
                    let clear_pipe = self.clear_pipeline_for(t.format);
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
                        rp.set_pipeline(clear_pipe);
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

/// Reinterpret a SPIR-V `&[u32]` word slice as its underlying bytes for content hashing.
fn bytemuck_u32_bytes(w: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(w.as_ptr() as *const u8, std::mem::size_of_val(w)) }
}
