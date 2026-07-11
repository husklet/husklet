//! GPU rung 3 host executor: a Metal implementation of `dd_gpu::backend::GpuBackend`. The guest streams
//! the dd-gpu command IR (buffers, a render pipeline, a render pass, draws) over a socket; the executor
//! decodes it (`dd_gpu::replay::replay_stream`) and drives THIS backend, rendering into the rung-2
//! IOSurface it resolved via the mach bridge. So *arbitrary* guest-described geometry renders on the host
//! GPU — the path a real ICD/app needs — not a hardcoded triangle.
//!
//! Shaders: the IR carries SPIR-V, which we'd need SPIRV-Cross to turn into MSL (a heavy dep, deferred).
//! For this first slice the backend ships ONE builtin **vertex-color** MSL pipeline: `create_shader`
//! ignores the SPIR-V and `create_render_pipeline` binds the builtin (configuring the vertex layout from
//! the guest's descriptor). The guest therefore controls the *geometry* (vertex buffer + draw count);
//! real per-app shaders are the next step (SPIRV-Cross or naga).
#![cfg(target_os = "macos")]

use crate::metal::MetalCtx;
use dd_gpu::backend::{Capabilities, GpuBackend};

/// Per-frame executor logging is off the hot path unless `DD_DISPLAY_DEBUG` is set. At native frame
/// rates (~9k fps) a per-frame `eprintln!` (format + `write()` syscall) sits on the guest's critical
/// path — the ack that unblocks the guest follows it — so gating it is a real steady-state win.
fn exec_debug() -> bool {
    use std::sync::OnceLock;
    static D: OnceLock<bool> = OnceLock::new();
    *D.get_or_init(|| std::env::var_os("DD_DISPLAY_DEBUG").is_some())
}
use dd_gpu::id::{BindGroupId, BufferId, PipelineId, SamplerId, ShaderId, TextureId};
use dd_gpu::ir::{
    BindGroupDesc, BindResource, BufferDesc, CommandBuffer, Enc, IndexFormat, LoadOp,
    RenderPipelineDesc, SamplerDesc, TextureDesc, Topology,
};
use dd_gpu::{GpuError, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLBlitCommandEncoder, MTLBuffer, MTLClearColor,
    MTLColorWriteMask, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLCompareFunction,
    MTLDepthStencilDescriptor, MTLDepthStencilState, MTLDevice, MTLIndexType, MTLLibrary,
    MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder,
    MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState,
    MTLResourceOptions, MTLSamplerAddressMode, MTLSamplerDescriptor, MTLSamplerMinMagFilter,
    MTLSamplerMipFilter,
    MTLSamplerState, MTLScissorRect, MTLSize, MTLStorageMode, MTLStoreAction, MTLTexture,
    MTLTextureDescriptor, MTLTextureUsage, MTLVertexDescriptor, MTLVertexFormat, MTLViewport,
};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

/// FNV-ish content hash of a byte slice via the std default hasher (used to content-key shader/PSO caches).
fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

fn metal_blend_factor(v: u32) -> MTLBlendFactor {
    match v {
        0 => MTLBlendFactor::Zero,
        1 => MTLBlendFactor::One,
        2 => MTLBlendFactor::SourceColor,
        3 => MTLBlendFactor::OneMinusSourceColor,
        4 => MTLBlendFactor::SourceAlpha,
        5 => MTLBlendFactor::OneMinusSourceAlpha,
        6 => MTLBlendFactor::DestinationColor,
        7 => MTLBlendFactor::OneMinusDestinationColor,
        8 => MTLBlendFactor::DestinationAlpha,
        9 => MTLBlendFactor::OneMinusDestinationAlpha,
        10 => MTLBlendFactor::SourceAlphaSaturated,
        11 => MTLBlendFactor::BlendColor,
        12 => MTLBlendFactor::OneMinusBlendColor,
        13 => MTLBlendFactor::BlendAlpha,
        14 => MTLBlendFactor::OneMinusBlendAlpha,
        _ => MTLBlendFactor::One,
    }
}

fn metal_blend_op(v: u32) -> MTLBlendOperation {
    match v {
        1 => MTLBlendOperation::Subtract,
        2 => MTLBlendOperation::ReverseSubtract,
        3 => MTLBlendOperation::Min,
        4 => MTLBlendOperation::Max,
        _ => MTLBlendOperation::Add,
    }
}

fn metal_write_mask(mask: u32) -> MTLColorWriteMask {
    let mut out = MTLColorWriteMask::None;
    if mask & 0x1 != 0 {
        out |= MTLColorWriteMask::Red;
    }
    if mask & 0x2 != 0 {
        out |= MTLColorWriteMask::Green;
    }
    if mask & 0x4 != 0 {
        out |= MTLColorWriteMask::Blue;
    }
    if mask & 0x8 != 0 {
        out |= MTLColorWriteMask::Alpha;
    }
    out
}

fn metal_vertex_format(format: u32) -> MTLVertexFormat {
    let comps = (format & 0xff).clamp(1, 4);
    let kind = (format >> 8) & 0xff;
    let normalized = (format & (1 << 16)) != 0;
    match kind {
        1 => match (comps, normalized) {
            (1, true) => MTLVertexFormat::UCharNormalized,
            (2, true) => MTLVertexFormat::UChar2Normalized,
            (3, true) => MTLVertexFormat::UChar3Normalized,
            (4, true) => MTLVertexFormat::UChar4Normalized,
            (1, false) => MTLVertexFormat::UChar,
            (2, false) => MTLVertexFormat::UChar2,
            (3, false) => MTLVertexFormat::UChar3,
            _ => MTLVertexFormat::UChar4,
        },
        2 => match (comps, normalized) {
            (1, true) => MTLVertexFormat::CharNormalized,
            (2, true) => MTLVertexFormat::Char2Normalized,
            (3, true) => MTLVertexFormat::Char3Normalized,
            (4, true) => MTLVertexFormat::Char4Normalized,
            (1, false) => MTLVertexFormat::Char,
            (2, false) => MTLVertexFormat::Char2,
            (3, false) => MTLVertexFormat::Char3,
            _ => MTLVertexFormat::Char4,
        },
        3 => match (comps, normalized) {
            (1, true) => MTLVertexFormat::UShortNormalized,
            (2, true) => MTLVertexFormat::UShort2Normalized,
            (3, true) => MTLVertexFormat::UShort3Normalized,
            (4, true) => MTLVertexFormat::UShort4Normalized,
            (1, false) => MTLVertexFormat::UShort,
            (2, false) => MTLVertexFormat::UShort2,
            (3, false) => MTLVertexFormat::UShort3,
            _ => MTLVertexFormat::UShort4,
        },
        4 => match (comps, normalized) {
            (1, true) => MTLVertexFormat::ShortNormalized,
            (2, true) => MTLVertexFormat::Short2Normalized,
            (3, true) => MTLVertexFormat::Short3Normalized,
            (4, true) => MTLVertexFormat::Short4Normalized,
            (1, false) => MTLVertexFormat::Short,
            (2, false) => MTLVertexFormat::Short2,
            (3, false) => MTLVertexFormat::Short3,
            _ => MTLVertexFormat::Short4,
        },
        5 => match comps {
            1 => MTLVertexFormat::UInt,
            2 => MTLVertexFormat::UInt2,
            3 => MTLVertexFormat::UInt3,
            _ => MTLVertexFormat::UInt4,
        },
        6 => match comps {
            1 => MTLVertexFormat::Int,
            2 => MTLVertexFormat::Int2,
            3 => MTLVertexFormat::Int3,
            _ => MTLVertexFormat::Int4,
        },
        _ => match comps {
            1 => MTLVertexFormat::Float,
            2 => MTLVertexFormat::Float2,
            3 => MTLVertexFormat::Float3,
            _ => MTLVertexFormat::Float4,
        },
    }
}

/// Metal buffer-argument index base for guest vertex buffers. Multi-VBO apps (glmark2/ANGLE bind a
/// separate tightly-packed VBO per attribute) map guest vertex-buffer slot `i` → Metal buffer index
/// `VBUF_BASE + i`. Placing them high keeps them clear of the uniform block at `[[buffer(1)]]` (bind-group
/// binding 1) and other low-index resources. `create_render_pipeline` and `SetVertexBuffer` apply it
/// identically so the `[[stage_in]]` layout and the bound buffers agree. (Metal allows indices 0..30.)
const VBUF_BASE: usize = 16;

/// One resolved binding inside a bind group: a uniform/storage buffer, a sampled texture, or a sampler,
/// each destined for a specific MSL `[[buffer(i)]]`/`[[texture(i)]]`/`[[sampler(i)]]` index in both stages.
enum Bind {
    Buffer {
        index: u32,
        buffer: u32,
        offset: u64,
    },
    Texture {
        index: u32,
        texture: u32,
    },
    Sampler {
        index: u32,
        sampler: u32,
    },
}

/// Map a dd-gpu `TextureFormat` to the Metal pixel format the backend can materialize as a sampled/render
/// texture (color formats only; depth is handled separately).
fn tex_pixel_format(f: dd_gpu::ir::TextureFormat) -> MTLPixelFormat {
    use dd_gpu::ir::TextureFormat as F;
    match f {
        F::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        F::Rgba8Srgb => MTLPixelFormat::RGBA8Unorm_sRGB,
        F::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        F::Bgra8Srgb => MTLPixelFormat::BGRA8Unorm_sRGB,
        F::R8Unorm => MTLPixelFormat::R8Unorm,
        F::Rg8Unorm => MTLPixelFormat::RG8Unorm,
        // Map the float color formats to their real Metal formats instead of silently reinterpreting
        // them as BGRA8 (which would change channel width/layout and hide guest bugs).
        F::Rgba16Float => MTLPixelFormat::RGBA16Float,
        F::Rgba32Float => MTLPixelFormat::RGBA32Float,
        F::R32Float => MTLPixelFormat::R32Float,
        // Depth formats are not color textures (handled via the depth attachment path); default the
        // color slot to BGRA8.
        F::Depth32Float | F::Depth24PlusStencil8 => MTLPixelFormat::BGRA8Unorm,
    }
}

fn texture_desc_key(desc: &TextureDesc) -> (u32, u32, u32, u32, u32, u32, u32) {
    (
        desc.width.max(1),
        desc.height.max(1),
        desc.depth.max(1),
        desc.mip_levels.max(1),
        desc.sample_count.max(1),
        desc.format.to_u32(),
        desc.usage,
    )
}

fn write_texture_png(tex: &ProtocolObject<dyn MTLTexture>, out: &str) -> bool {
    let w = tex.width() as u32;
    let h = tex.height() as u32;
    if w == 0 || h == 0 {
        return false;
    }
    let bpr = w as usize * 4;
    let mut raw = vec![0u8; bpr * h as usize];
    unsafe {
        tex.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            std::ptr::NonNull::new(raw.as_mut_ptr() as *mut _).unwrap(),
            bpr,
            MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize {
                    width: w as usize,
                    height: h as usize,
                    depth: 1,
                },
            },
            0,
        );
    }
    let is_bgra = tex.pixelFormat() == MTLPixelFormat::BGRA8Unorm
        || tex.pixelFormat() == MTLPixelFormat::BGRA8Unorm_sRGB;
    let mut rgba = raw;
    if is_bgra {
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 0xff;
        }
    }
    std::fs::write(out, dd_term_core::png::encode_rgba(w, h, &rgba)).is_ok()
}

fn dump_texture_png(id: u32, tex: &ProtocolObject<dyn MTLTexture>, dir: &str, seq: u64) {
    let w = tex.width() as u32;
    let h = tex.height() as u32;
    let _ = std::fs::create_dir_all(dir);
    let path = format!("{dir}/tex-{id}-{seq:04}-{w}x{h}.png");
    if write_texture_png(tex, &path) {
        eprintln!("dd-metal: dumped texture {id} -> {path}");
    }
}

const BUILTIN_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;
struct VIn { float2 pos [[attribute(0)]]; float4 color [[attribute(1)]]; };
struct VOut { float4 pos [[position]]; float4 color; };
vertex VOut vcmain(VIn in [[stage_in]]) { VOut o; o.pos = float4(in.pos, 0.0, 1.0); o.color = in.color; return o; }
fragment float4 fcmain(VOut in [[stage_in]]) { return in.color; }
"#;

struct Pipeline {
    state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    primitive: MTLPrimitiveType,
    depth: bool, // pipeline was built with a depth attachment → bind the depth-stencil state
}

pub struct MetalBackend {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    lib: Retained<ProtocolObject<dyn MTLLibrary>>,
    buffers: HashMap<u32, Retained<ProtocolObject<dyn MTLBuffer>>>,
    textures: HashMap<u32, Retained<ProtocolObject<dyn MTLTexture>>>,
    texture_descs: HashMap<u32, (u32, u32, u32, u32, u32, u32, u32)>,
    /// Present render targets injected by the executor via `set_render_target` (the IOSurface-backed
    /// swapchain image). These live in a SEPARATE namespace from guest-created `textures` so the
    /// executor↔guest protocol's reserved present id (conventionally 1) can never be aliased or clobbered
    /// by a guest `create_texture` for the same id: a guest texture wins its own id (resolved first), and
    /// the present target survives untouched in its own map. See `resolve_texture`.
    present_targets: HashMap<u32, Retained<ProtocolObject<dyn MTLTexture>>>,
    pipelines: HashMap<u32, Pipeline>,
    /// Per-app shader modules: the guest's GLSL, translated to MSL by the shim and shipped as bytes in
    /// the IR `CreateShader`, compiled here to an `MTLLibrary`. Absent → the builtin vertex-color pipeline.
    shaders: HashMap<u32, Retained<ProtocolObject<dyn MTLLibrary>>>,
    /// Sampler states (for `sampler2D`/`texture()` — glmark2's texture scene, Chrome's UI atlas).
    samplers: HashMap<u32, Retained<ProtocolObject<dyn MTLSamplerState>>>,
    /// Bind groups: group id → resolved bindings (uniform buffers `[[buffer(i)]]`, sampled textures
    /// `[[texture(i)]]`, samplers `[[sampler(i)]]`), bound into both the vertex and fragment stages.
    bind_groups: HashMap<u32, Vec<Bind>>,
    /// Depth-test state (compare Less, depth-write on) + a lazily-created depth texture, for apps that
    /// `glEnable(GL_DEPTH_TEST)` (correct 3D occlusion, e.g. es2gears).
    depth_state: Retained<ProtocolObject<dyn MTLDepthStencilState>>,
    depth_tex: Option<(u32, u32, Retained<ProtocolObject<dyn MTLTexture>>)>,
    // L3 content-keyed caches: the guest re-emits CreateShader(20)+CreateRenderPipeline(30) EVERY frame
    // with identical content, so without these the two heaviest Metal calls (MSL→AIR compile, PSO link)
    // recompile every frame even once the backend persists (L2). Keying by content hash makes them map
    // hits after warmup → 0 compiles/frame; a genuine change (e.g. glmark2 build→texture scene switch,
    // which reuses id 20/30 with new MSL/layout) misses the cache and correctly recompiles.
    shader_lib_cache: HashMap<u64, Retained<ProtocolObject<dyn MTLLibrary>>>,
    failed_shader_cache: HashSet<u64>,
    pipeline_cache: HashMap<
        u64,
        (
            Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
            MTLPrimitiveType,
            bool,
        ),
    >,
    shader_id_hash: HashMap<u32, u64>, // shader id → MSL hash currently installed
    pipeline_id_hash: HashMap<u32, u64>, // pipeline id → desc hash currently installed
    clear_pipeline: Option<Retained<ProtocolObject<dyn MTLRenderPipelineState>>>,
    // Prof counters (read + reset per frame by the executor when DD_RENDER_PROF is on). `*_compiles`
    // count only ACTUAL Metal compiles (cache misses) — the key steady-state regression guard is that
    // they read 0 after warmup.
    pub shader_compiles: u32,
    pub pipeline_compiles: u32,
    pub lib_compiles: u32,
    pub gpu_wait_ns: u64,
    /// L4: the guest IOSurface id this frame renders into (set by the executor before `replay_stream`).
    /// `submit` uses it to look up the cross-queue tearing fence for async submit. 0 = none.
    pub cur_surface_id: u32,
    /// Texture ids that are the PRESENTED surface (injected via `set_render_target` — the IOSurface /
    /// default framebuffer). Everything else a render pass draws into is an OFFSCREEN render target
    /// (e.g. Chrome's content tiles 512/514, which Chrome GPU-rasterizes page body text into, then
    /// composites). GL's framebuffer origin is bottom-left; Metal's is top-left, so rendering the same
    /// guest geometry into an offscreen texture stores it Y-mirrored versus what a later GL-convention
    /// `texture()` sample expects — the composite then shows page content upside-down while the
    /// directly-drawn UI (CPU-uploaded atlases, no render-to-texture bounce) stays upright. We compensate
    /// by Y-flipping the viewport (negative height) for offscreen passes only, so the tile is stored
    /// bottom-left like GL and samples upright. The presented surface is read back / scanned out top-left
    /// and is left untouched.
    surface_ids: HashSet<u32>,
}

/// Decode an MSL source string from an IR shader payload: word[0] = byte length, the rest packs the UTF-8
/// bytes 4-per-word (little-endian). Empty (or a real SPIR-V blob we can't consume) → None (use builtin).
fn msl_from_words(words: &[u32]) -> Option<String> {
    let len = *words.first()? as usize;
    if len == 0 || len > (words.len() - 1) * 4 {
        return None;
    }
    let mut bytes = Vec::with_capacity(len);
    for w in &words[1..] {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes.truncate(len);
    String::from_utf8(bytes).ok()
}

impl MetalBackend {
    pub fn new(ctx: &MetalCtx) -> Self {
        let lib = ctx
            .device
            .newLibraryWithSource_options_error(&NSString::from_str(BUILTIN_MSL), None)
            .expect("builtin MSL compile");
        let dsd = unsafe { MTLDepthStencilDescriptor::new() };
        dsd.setDepthCompareFunction(MTLCompareFunction::Less);
        dsd.setDepthWriteEnabled(true);
        let depth_state = ctx
            .device
            .newDepthStencilStateWithDescriptor(&dsd)
            .expect("depth-stencil state");
        Self {
            device: ctx.device.clone(),
            queue: ctx.queue.clone(),
            lib,
            buffers: HashMap::new(),
            textures: HashMap::new(),
            texture_descs: HashMap::new(),
            present_targets: HashMap::new(),
            pipelines: HashMap::new(),
            shaders: HashMap::new(),
            samplers: HashMap::new(),
            bind_groups: HashMap::new(),
            depth_state,
            depth_tex: None,
            shader_lib_cache: HashMap::new(),
            failed_shader_cache: HashSet::new(),
            pipeline_cache: HashMap::new(),
            shader_id_hash: HashMap::new(),
            pipeline_id_hash: HashMap::new(),
            clear_pipeline: None,
            shader_compiles: 0,
            pipeline_compiles: 0,
            lib_compiles: 1, // the builtin BUILTIN_MSL compile just above
            gpu_wait_ns: 0,
            cur_surface_id: 0,
            surface_ids: HashSet::new(),
        }
    }

    fn clear_pipeline(&mut self) -> Result<Retained<ProtocolObject<dyn MTLRenderPipelineState>>> {
        if let Some(p) = &self.clear_pipeline {
            return Ok(p.clone());
        }
        let vfn = self
            .lib
            .newFunctionWithName(&NSString::from_str("vcmain"))
            .ok_or(GpuError::Unsupported("clear vertex fn"))?;
        let ffn = self
            .lib
            .newFunctionWithName(&NSString::from_str("fcmain"))
            .ok_or(GpuError::Unsupported("clear fragment fn"))?;
        let pdesc = MTLRenderPipelineDescriptor::new();
        pdesc.setVertexFunction(Some(&vfn));
        pdesc.setFragmentFunction(Some(&ffn));
        unsafe {
            let ca = pdesc.colorAttachments().objectAtIndexedSubscript(0);
            ca.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            let vd = MTLVertexDescriptor::vertexDescriptor();
            let a0 = vd.attributes().objectAtIndexedSubscript(0);
            a0.setFormat(MTLVertexFormat::Float2);
            a0.setOffset(0);
            a0.setBufferIndex(VBUF_BASE);
            let a1 = vd.attributes().objectAtIndexedSubscript(1);
            a1.setFormat(MTLVertexFormat::Float4);
            a1.setOffset(8);
            a1.setBufferIndex(VBUF_BASE);
            vd.layouts()
                .objectAtIndexedSubscript(VBUF_BASE)
                .setStride(24);
            pdesc.setVertexDescriptor(Some(&vd));
        }
        let state = self
            .device
            .newRenderPipelineStateWithDescriptor_error(&pdesc)
            .map_err(|_| GpuError::Unsupported("clear pipeline compile"))?;
        self.pipeline_compiles += 1;
        self.clear_pipeline = Some(state.clone());
        Ok(state)
    }

    /// Content hash of a render-pipeline descriptor for the L3 PSO cache. Folds in the *installed*
    /// shader-source hash of each referenced module so a recompiled shader (same ids, new MSL) forces a
    /// PSO rebuild even when the descriptor bytes are unchanged.
    fn hash_pipeline_key(&self, desc: &RenderPipelineDesc) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        desc.vertex.module.hash(&mut h);
        desc.vertex.entry.hash(&mut h);
        self.shader_id_hash
            .get(&desc.vertex.module)
            .copied()
            .unwrap_or(0)
            .hash(&mut h);
        match &desc.fragment {
            Some(f) => {
                1u8.hash(&mut h);
                f.module.hash(&mut h);
                f.entry.hash(&mut h);
                self.shader_id_hash
                    .get(&f.module)
                    .copied()
                    .unwrap_or(0)
                    .hash(&mut h);
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

    /// A depth texture matching (w,h), created once and reused (Depth32Float, private, render-target).
    fn depth_texture(&mut self, w: u32, h: u32) -> Retained<ProtocolObject<dyn MTLTexture>> {
        if let Some((dw, dh, t)) = &self.depth_tex {
            if *dw == w && *dh == h {
                return t.clone();
            }
        }
        let d = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::Depth32Float,
                w as usize,
                h as usize,
                false,
            )
        };
        d.setUsage(MTLTextureUsage::RenderTarget);
        d.setStorageMode(MTLStorageMode::Private);
        let t = self
            .device
            .newTextureWithDescriptor(&d)
            .expect("depth texture");
        self.depth_tex = Some((w, h, t.clone()));
        t
    }

    /// Pre-register a texture id as the executor's render target (the rung-2 IOSurface wrapped as an
    /// MTLTexture). The guest's `BeginRenderPass` color attachment references this id.
    ///
    /// The target is stored in the dedicated `present_targets` namespace, NOT in the guest-visible
    /// `textures` map: a guest that (per protocol should not, but could) `create_texture`s this same id
    /// then gets its own distinct texture in `textures`, which `resolve_texture` prefers — so the guest's
    /// stray geometry lands in its own texture and can never clobber or alias the presented surface.
    pub fn set_render_target(&mut self, id: u32, tex: Retained<ProtocolObject<dyn MTLTexture>>) {
        self.present_targets.insert(id, tex);
        // A prior guest texture under this id would shadow the present target (see resolve_texture); drop
        // it so the freshly-injected present surface is the one that resolves for the executor's own IR.
        self.textures.remove(&id);
        self.texture_descs.remove(&id);
        // This id is the presented surface (default framebuffer). Passes that target it are scanned out
        // top-left and must NOT be Y-flipped; only OFFSCREEN render targets are (see `surface_ids`).
        self.surface_ids.insert(id);
    }

    /// Resolve a texture id to its materialized MTLTexture. Guest-created `textures` win over the reserved
    /// `present_targets` namespace, so a guest that creates a texture with the present id addresses its own
    /// texture (never the presented surface), while the unmodified present id still resolves to the
    /// injected render target. This is the single lookup every render/sample/copy site funnels through so
    /// the present-vs-guest namespacing is enforced uniformly.
    fn resolve_texture(&self, id: u32) -> Option<&Retained<ProtocolObject<dyn MTLTexture>>> {
        self.textures.get(&id).or_else(|| self.present_targets.get(&id))
    }

    /// Whether `id` names any materialized texture (guest-created or a present target).
    fn has_texture(&self, id: u32) -> bool {
        self.textures.contains_key(&id) || self.present_targets.contains_key(&id)
    }

    /// Test/debug hook: fetch a materialized texture by id (e.g. to read back an offscreen render target
    /// such as a Chrome content tile, or a present target registered via `set_render_target`).
    pub fn texture(&self, id: u32) -> Option<&ProtocolObject<dyn MTLTexture>> {
        self.resolve_texture(id).map(|t| &**t)
    }

}

/// `dd-display selftest-shader <out.png>`: prove the per-app SHADER path — the same one the GL shim
/// drives after GLSL→MSL. Build an IR stream that ships a CUSTOM MSL shader via `CreateShader` (bytes),
/// a pipeline referencing it, and a quad; replay it; PNG. The fragment shader brightens the vertex colors
/// (out = c*0.5+0.5), so a washed-out quad proves the app's shader ran (not the builtin passthrough).
pub fn selftest_shader(out: &str) -> ! {
    use crate::metal::{cfrelease, create_iosurface};
    use dd_gpu::ir::*;
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("selftest-shader: no Metal device");
        std::process::exit(1);
    };
    const MSL: &str = "#include <metal_stdlib>\nusing namespace metal;\nstruct VIn { float2 p [[attribute(0)]]; float4 c [[attribute(1)]]; };\nstruct VOut { float4 position [[position]]; float4 c [[user(v0)]]; };\nvertex VOut vmain(VIn in [[stage_in]]) { VOut o; o.position = float4(in.p, 0.0, 1.0); o.c = in.c; return o; }\nfragment float4 fmain(VOut in [[stage_in]]) { return float4(in.c.rgb * 0.5 + 0.5, 1.0); }\n";
    // Pack MSL bytes into words: word[0]=len, rest = bytes 4/word (matches msl_from_words / the shim).
    let mut words = vec![MSL.len() as u32];
    let b = MSL.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let mut w = [0u8; 4];
        for k in 0..4 {
            if i + k < b.len() {
                w[k] = b[i + k];
            }
        }
        words.push(u32::from_le_bytes(w));
        i += 4;
    }
    let (w, h): (u32, u32) = (256, 256);
    unsafe {
        let surf = create_iosurface(w, h);
        let tex = ctx.texture_from_iosurface(surf, w, h);
        let mut be = MetalBackend::new(&ctx);
        be.set_render_target(1, tex.clone());
        let v: [[f32; 6]; 6] = [
            [-0.8, 0.8, 1.0, 0.0, 0.0, 1.0],
            [-0.8, -0.8, 0.0, 0.0, 1.0, 1.0],
            [0.8, 0.8, 0.0, 1.0, 0.0, 1.0],
            [0.8, 0.8, 0.0, 1.0, 0.0, 1.0],
            [-0.8, -0.8, 0.0, 0.0, 1.0, 1.0],
            [0.8, -0.8, 1.0, 1.0, 0.0, 1.0],
        ];
        let mut data = Vec::new();
        for vert in &v {
            for f in vert {
                data.extend_from_slice(&f.to_le_bytes());
            }
        }
        let cmds = vec![
            Cmd::CreateBuffer(
                10,
                BufferDesc {
                    size: data.len() as u64,
                    usage: buffer_usage::VERTEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 10,
                offset: 0,
                data,
            },
            Cmd::CreateShader {
                id: 20,
                spirv: words,
            },
            Cmd::CreateRenderPipeline(
                30,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 20,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 20,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![VertexLayout {
                        stride: 24,
                        step_mode: 0,
                        attrs: vec![
                            VertexAttr {
                                location: 0,
                                format: 0,
                                offset: 0,
                            },
                            VertexAttr {
                                location: 1,
                                format: 0,
                                offset: 8,
                            },
                        ],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Bgra8Unorm,
                        blend: None,
                        write_mask: 0xf,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 10,
                        offset: 0,
                    },
                    Enc::Draw {
                        vertex_count: 6,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        let bytes = encode_stream(&cmds);
        dd_gpu::replay::replay_stream(&mut be, &bytes).expect("replay");
        let dst = ctx.new_bgra_texture(w, h);
        ctx.blit(&tex, &dst);
        let bgra = ctx.readback_bgra(&dst, w, h);
        cfrelease(surf);
        let mut rgba = vec![0u8; bgra.len()];
        for i in (0..bgra.len()).step_by(4) {
            rgba[i] = bgra[i + 2];
            rgba[i + 1] = bgra[i + 1];
            rgba[i + 2] = bgra[i];
            rgba[i + 3] = 0xff;
        }
        let _ = std::fs::write(out, dd_term_core::png::encode_rgba(w, h, &rgba));
    }
    println!("selftest-shader: replayed a custom-MSL-shader quad -> {out}");
    std::process::exit(0);
}

/// `dd-display selftest-shim-ir <ir.bin> <out.png>`: replay a raw dd-gpu IR byte-stream (exactly what the
/// GL shim emits for one frame, captured via `DD_IR_DUMP`) through the real `MetalBackend` into an
/// IOSurface → PNG. Closes the loop on the shim's RUNTIME IR (CreateTexture/CreateSampler/
/// CopyBufferToTexture/SetIndexBuffer/DrawIndexed/bind-group) without needing the container executor socket.
pub fn selftest_shim_ir(irfile: &str, out: &str, w: u32, h: u32, target_id: u32) -> ! {
    use crate::metal::{cfrelease, create_iosurface};
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("selftest-shim-ir: no Metal device");
        std::process::exit(1);
    };
    let bytes = match std::fs::read(irfile) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("selftest-shim-ir: read {irfile}: {e}");
            std::process::exit(1);
        }
    };
    unsafe {
        let surf = create_iosurface(w, h);
        let rt = ctx.texture_from_iosurface(surf, w, h);
        let mut be = MetalBackend::new(&ctx);
        be.set_render_target(1, rt.clone());
        match dd_gpu::replay::replay_stream(&mut be, &bytes) {
            Ok(_) => eprintln!("selftest-shim-ir: replayed {} IR bytes OK", bytes.len()),
            Err(e) => {
                eprintln!("selftest-shim-ir: replay error: {e}");
                std::process::exit(1);
            }
        }
        if target_id == 1 {
            readback_png(&ctx, &rt, w, h, out);
        } else if let Some(tex) = be.texture(target_id) {
            if !write_texture_png(tex, out) {
                eprintln!("selftest-shim-ir: failed to write texture target {target_id}");
                std::process::exit(1);
            }
        } else {
            eprintln!("selftest-shim-ir: texture target {target_id} was not created");
            std::process::exit(1);
        }
        cfrelease(surf);
    }
    println!("selftest-shim-ir: {irfile} -> {out}");
    std::process::exit(0);
}

/// `dd-display selftest-msl <file.metal>`: compile a Metal source file via `newLibraryWithSource` and
/// report OK / the compiler diagnostics. Used to prove the GL shim's GLSL-ES→MSL translator emits valid
/// MSL for real app shaders (e.g. glmark2's `light-basic`/`light-basic-tex`) without running the app.
pub fn selftest_msl(path: &str) -> ! {
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("selftest-msl: no Metal device");
        std::process::exit(1);
    };
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("selftest-msl: read {path}: {e}");
            std::process::exit(1);
        }
    };
    match ctx
        .device
        .newLibraryWithSource_options_error(&NSString::from_str(&src), None)
    {
        Ok(lib) => {
            let vfn = lib
                .newFunctionWithName(&NSString::from_str("vmain"))
                .is_some();
            let ffn = lib
                .newFunctionWithName(&NSString::from_str("fmain"))
                .is_some();
            println!("selftest-msl: {path} COMPILED OK (vmain={vfn} fmain={ffn})");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("selftest-msl: {path} FAILED:\n{}", e.localizedDescription());
            std::process::exit(1);
        }
    }
}

/// Pack an MSL string into IR shader words (word[0]=byte len, rest = bytes 4/word LE) — the encoding both
/// `msl_from_words` and the GL shim use.
fn pack_msl(src: &str) -> Vec<u32> {
    let mut words = vec![src.len() as u32];
    let b = src.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let mut w = [0u8; 4];
        for k in 0..4 {
            if i + k < b.len() {
                w[k] = b[i + k];
            }
        }
        words.push(u32::from_le_bytes(w));
        i += 4;
    }
    words
}

fn readback_png(ctx: &MetalCtx, tex: &ProtocolObject<dyn MTLTexture>, w: u32, h: u32, out: &str) {
    let dst = ctx.new_bgra_texture(w, h);
    ctx.blit(tex, &dst);
    let bgra = ctx.readback_bgra(&dst, w, h);
    let mut rgba = vec![0u8; bgra.len()];
    for i in (0..bgra.len()).step_by(4) {
        rgba[i] = bgra[i + 2];
        rgba[i + 1] = bgra[i + 1];
        rgba[i + 2] = bgra[i];
        rgba[i + 3] = 0xff;
    }
    let _ = std::fs::write(out, dd_term_core::png::encode_rgba(w, h, &rgba));
}

/// `dd-display selftest-texture <out.png>`: prove the TEXTURE path — a `sampler2D`/`texture()` fragment
/// shader sampling a 2×2 RGBA texture uploaded via a staging buffer (`CreateTexture` + `CreateBuffer` +
/// `WriteBuffer` + `CopyBufferToTexture`) and bound with a sampler through a bind group. The PNG shows the
/// four texel colors stretched across the quad (bilinear) — the exact surface glmark2's texture scene and
/// Chrome's UI atlas need.
pub fn selftest_texture(out: &str) -> ! {
    use crate::metal::{cfrelease, create_iosurface};
    use dd_gpu::ir::*;
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("selftest-texture: no Metal device");
        std::process::exit(1);
    };
    const MSL: &str = "#include <metal_stdlib>\nusing namespace metal;\nstruct VIn { float2 p [[attribute(0)]]; float2 uv [[attribute(1)]]; };\nstruct VOut { float4 position [[position]]; float2 uv [[user(v0)]]; };\nvertex VOut vmain(VIn in [[stage_in]]) { VOut o; o.position = float4(in.p, 0.0, 1.0); o.uv = in.uv; return o; }\nfragment float4 fmain(VOut in [[stage_in]], texture2d<float> uTex [[texture(0)]], sampler uTexSmplr [[sampler(0)]]) { return uTex.sample(uTexSmplr, in.uv); }\n";
    let (w, h): (u32, u32) = (256, 256);
    // quad: pos.xy + uv (16 bytes/vertex), 2 triangles
    let v: [[f32; 4]; 6] = [
        [-0.9, 0.9, 0.0, 0.0],
        [-0.9, -0.9, 0.0, 1.0],
        [0.9, 0.9, 1.0, 0.0],
        [0.9, 0.9, 1.0, 0.0],
        [-0.9, -0.9, 0.0, 1.0],
        [0.9, -0.9, 1.0, 1.0],
    ];
    let mut vbytes = Vec::new();
    for vert in &v {
        for f in vert {
            vbytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    // 2×2 RGBA texels: red, green, blue, white
    let tex_px: [u8; 16] = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];
    unsafe {
        let surf = create_iosurface(w, h);
        let rt = ctx.texture_from_iosurface(surf, w, h);
        let mut be = MetalBackend::new(&ctx);
        be.set_render_target(1, rt.clone());
        let cmds = vec![
            Cmd::CreateBuffer(
                10,
                BufferDesc {
                    size: vbytes.len() as u64,
                    usage: buffer_usage::VERTEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 10,
                offset: 0,
                data: vbytes,
            },
            Cmd::CreateBuffer(
                12,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 12,
                offset: 0,
                data: tex_px.to_vec(),
            },
            Cmd::CreateTexture(
                2,
                TextureDesc {
                    width: 2,
                    height: 2,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim: TextureDim::D2,
                    format: TextureFormat::Rgba8Unorm,
                    usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::CreateSampler(
                3,
                SamplerDesc {
                    min_filter: Filter::Linear,
                    mag_filter: Filter::Linear,
                    mip_filter: Filter::Nearest,
                    address_u: AddressMode::ClampToEdge,
                    address_v: AddressMode::ClampToEdge,
                    address_w: AddressMode::ClampToEdge,
                },
            ),
            Cmd::CreateShader {
                id: 20,
                spirv: pack_msl(MSL),
            },
            Cmd::CreateRenderPipeline(
                30,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 20,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 20,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![VertexLayout {
                        stride: 16,
                        step_mode: 0,
                        attrs: vec![
                            VertexAttr {
                                location: 0,
                                format: 2,
                                offset: 0,
                            },
                            VertexAttr {
                                location: 1,
                                format: 2,
                                offset: 8,
                            },
                        ],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Bgra8Unorm,
                        blend: None,
                        write_mask: 0xf,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::CreateBindGroup(
                40,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Sampler { id: 3 },
                        },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 12,
                        src_offset: 0,
                        bytes_per_row: 8,
                        dst: 2,
                        mip: 0,
                        width: 2,
                        height: 2,
                    },
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetBindGroup {
                        index: 0,
                        group: 40,
                    },
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 10,
                        offset: 0,
                    },
                    Enc::Draw {
                        vertex_count: 6,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        let bytes = encode_stream(&cmds);
        dd_gpu::replay::replay_stream(&mut be, &bytes).expect("replay");
        readback_png(&ctx, &rt, w, h, out);
        cfrelease(surf);
    }
    println!("selftest-texture: sampled a 2x2 texture across a quad -> {out}");
    std::process::exit(0);
}

/// `dd-display selftest-indexed <out.png>`: prove the INDEXED-DRAW path — a 4-vertex quad rendered with a
/// 6-entry U16 index buffer (`SetIndexBuffer` + `DrawIndexed`), the `glDrawElements` mechanism glmark2's
/// mesh scenes and Chrome's compositor quads use. The PNG shows the vertex-colored quad from 4 (not 6)
/// vertices, so the index buffer really drove assembly.
pub fn selftest_indexed(out: &str) -> ! {
    use crate::metal::{cfrelease, create_iosurface};
    use dd_gpu::ir::*;
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("selftest-indexed: no Metal device");
        std::process::exit(1);
    };
    const MSL: &str = "#include <metal_stdlib>\nusing namespace metal;\nstruct VIn { float2 p [[attribute(0)]]; float4 c [[attribute(1)]]; };\nstruct VOut { float4 position [[position]]; float4 c [[user(v0)]]; };\nvertex VOut vmain(VIn in [[stage_in]]) { VOut o; o.position = float4(in.p, 0.0, 1.0); o.c = in.c; return o; }\nfragment float4 fmain(VOut in [[stage_in]]) { return in.c; }\n";
    let (w, h): (u32, u32) = (256, 256);
    // 4 unique corners (pos.xy + rgba), quad via 6 indices
    let v: [[f32; 6]; 4] = [
        [-0.9, 0.9, 1.0, 0.0, 0.0, 1.0],
        [-0.9, -0.9, 0.0, 1.0, 0.0, 1.0],
        [0.9, 0.9, 0.0, 0.0, 1.0, 1.0],
        [0.9, -0.9, 1.0, 1.0, 0.0, 1.0],
    ];
    let mut vbytes = Vec::new();
    for vert in &v {
        for f in vert {
            vbytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    let idx: [u16; 6] = [0, 1, 2, 2, 1, 3];
    let mut ibytes = Vec::new();
    for i in &idx {
        ibytes.extend_from_slice(&i.to_le_bytes());
    }
    unsafe {
        let surf = create_iosurface(w, h);
        let rt = ctx.texture_from_iosurface(surf, w, h);
        let mut be = MetalBackend::new(&ctx);
        be.set_render_target(1, rt.clone());
        let cmds = vec![
            Cmd::CreateBuffer(
                10,
                BufferDesc {
                    size: vbytes.len() as u64,
                    usage: buffer_usage::VERTEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 10,
                offset: 0,
                data: vbytes,
            },
            Cmd::CreateBuffer(
                11,
                BufferDesc {
                    size: ibytes.len() as u64,
                    usage: buffer_usage::INDEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 11,
                offset: 0,
                data: ibytes,
            },
            Cmd::CreateShader {
                id: 20,
                spirv: pack_msl(MSL),
            },
            Cmd::CreateRenderPipeline(
                30,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 20,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 20,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![VertexLayout {
                        stride: 24,
                        step_mode: 0,
                        attrs: vec![
                            VertexAttr {
                                location: 0,
                                format: 2,
                                offset: 0,
                            },
                            VertexAttr {
                                location: 1,
                                format: 4,
                                offset: 8,
                            },
                        ],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Bgra8Unorm,
                        blend: None,
                        write_mask: 0xf,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.05, 0.05, 0.1, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 10,
                        offset: 0,
                    },
                    Enc::SetIndexBuffer {
                        buffer: 11,
                        offset: 0,
                        format: IndexFormat::U16,
                    },
                    Enc::DrawIndexed {
                        index_count: 6,
                        instance_count: 1,
                        first_index: 0,
                        base_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        let bytes = encode_stream(&cmds);
        dd_gpu::replay::replay_stream(&mut be, &bytes).expect("replay");
        readback_png(&ctx, &rt, w, h, out);
        cfrelease(surf);
    }
    println!("selftest-indexed: drew a 4-vertex quad via a 6-index buffer -> {out}");
    std::process::exit(0);
}

/// GPU rung 3 executor transport: listen on a unix socket for framed dd-gpu IR streams from the guest,
/// resolve the target IOSurface (via the mach bridge), and `replay_stream` the guest's commands into it
/// on the host GPU. Frame = `[u32 iosurface_id][u32 w][u32 h][u32 stream_len][stream bytes]`; we reply one
/// ack byte when the replay completes (so the guest commits only after the frame is rendered).
pub fn run_executor(sock_path: String) {
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    let _ = std::fs::remove_file(&sock_path);
    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("dd-gpu executor: bind {sock_path}: {e}");
            return;
        }
    };
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("dd-gpu executor: no Metal device");
        return;
    };
    eprintln!("dd-gpu executor: listening on {sock_path}");
    // L2 + L7.1: one connection now carries the surface's WHOLE lifetime (frame = header+stream, ack as
    // before). `handle` builds ONE `MetalBackend` per connection and drives every frame through it, so the
    // builtin-MSL compile, the depth-stencil state, and — crucially — all resource + shader + PSO caches
    // (L3) survive across frames instead of being thrown away each frame. Re-resolving the guest IOSurface
    // is cached by id (the guest reuses one surface), so a steady frame does zero Metal object creation.
    fn handle(ctx: &MetalCtx, s: &mut UnixStream) -> std::io::Result<()> {
        let mut be = MetalBackend::new(ctx);
        // rt cache: guest IOSurface id → (retained surface, wrapped MTLTexture). The IOSurface-backed
        // texture is a live view of the surface's pages, so it stays valid across frames — wrap once.
        let mut rt_cache: HashMap<u32, Retained<ProtocolObject<dyn MTLTexture>>> = HashMap::new();
        let mut prof = RenderProf::open("exec");
        let mut seq: u64 = 0;
        let (mut cum_sh, mut cum_pso, mut cum_lib) = (0u32, 0u32, 0u32);
        loop {
            let mut hdr = [0u8; 16];
            match s.read_exact(&mut hdr) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()), // client done
                Err(e) => return Err(e),
            }
            let id = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            let w = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
            let h = u32::from_le_bytes(hdr[8..12].try_into().unwrap());
            let len = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
            if len > 64 * 1024 * 1024 {
                return Ok(());
            }
            let mut bytes = vec![0u8; len];
            s.read_exact(&mut bytes)?;
            let t_rx = std::time::Instant::now();
            unsafe {
                if !rt_cache.contains_key(&id) {
                    let surf = crate::metal::resolve_iosurface(id);
                    if surf.is_null() {
                        eprintln!("dd-gpu executor: IOSurface id {id} not found");
                        let _ = s.write_all(&[0]);
                        continue;
                    }
                    let tex = ctx.texture_from_iosurface(surf, w, h);
                    crate::metal::cfrelease(surf); // the MTLTexture retains the surface's backing
                    rt_cache.insert(id, tex);
                }
                be.set_render_target(1, rt_cache.get(&id).unwrap().clone());
            }
            be.cur_surface_id = id; // L4: submit() fences the async render on this IOSurface id
            // The ack tells the guest whether the frame actually replayed. A replay error must ack
            // failure (0), not success — otherwise the guest commits a stale/partly-rendered frame
            // believing it rendered.
            let rendered = match dd_gpu::replay::replay_stream(&mut be, &bytes) {
                Ok(_) => {
                    if exec_debug() {
                        eprintln!("dd-gpu executor: replayed {len} IR bytes into IOSurface {id}");
                    }
                    true
                }
                Err(e) => {
                    eprintln!("dd-gpu executor: replay error: {e}");
                    false
                }
            };
            s.write_all(&[rendered as u8])?; // ack: 1 = rendered, 0 = replay failed
            if let Some(p) = prof.as_mut() {
                let replay_us = t_rx.elapsed().as_micros() as u64;
                let gpu_us = be.gpu_wait_ns / 1000;
                let (dsh, dpso, dlib) = (
                    be.shader_compiles - cum_sh,
                    be.pipeline_compiles - cum_pso,
                    be.lib_compiles - cum_lib,
                );
                cum_sh = be.shader_compiles;
                cum_pso = be.pipeline_compiles;
                cum_lib = be.lib_compiles;
                p.line(&format!(
                    "{seq},{replay_us},{gpu_us},{dsh},{dpso},{dlib},{len}"
                ));
            }
            seq += 1;
        }
    }
    for conn in listener.incoming() {
        match conn {
            Ok(mut s) => {
                if let Err(e) = handle(&ctx, &mut s) {
                    eprintln!("dd-gpu executor: conn error: {e}");
                }
            }
            Err(_) => continue,
        }
    }
}

/// `DD_RENDER_PROF` per-hop ledger writer (executor + compositor). Zero-cost when the env is unset
/// (mirrors the `DD_DISPLAY_DEBUG`/`DD_SHIM_DEBUG` getenv-once pattern): `open` returns `None` and no
/// file is created. When set, appends one CSV line per frame to `$DD_RENDER_PROF_DIR/<who>-<pid>.csv`.
pub struct RenderProf {
    f: std::fs::File,
}
impl RenderProf {
    pub fn open(who: &str) -> Option<RenderProf> {
        if std::env::var_os("DD_RENDER_PROF").is_none() {
            return None;
        }
        let dir = std::env::var("DD_RENDER_PROF_DIR").unwrap_or_else(|_| "/tmp".into());
        let _ = std::fs::create_dir_all(&dir);
        let path = format!("{dir}/{who}-{}.csv", std::process::id());
        let mut f = std::fs::File::create(&path).ok()?;
        use std::io::Write;
        let hdr = match who {
            "exec" => {
                "seq,replay_us,gpu_us,shader_compiles,pipeline_compiles,lib_compiles,ir_bytes"
            }
            _ => "seq,fields",
        };
        let _ = writeln!(f, "{hdr}");
        Some(RenderProf { f })
    }
    pub fn line(&mut self, s: &str) {
        use std::io::Write;
        let _ = writeln!(self.f, "{s}");
    }
}

/// `dd-display selftest-replay <out.png>`: GPU rung 3 proof — build a dd-gpu **IR stream** describing a
/// vertex-colored QUAD (2 triangles, 4 corner colors), encode it to bytes, `replay_stream` it through the
/// `MetalBackend` into a host IOSurface, read back → PNG. The rendered quad reflects the STREAMED geometry
/// (buffers + draw), not any hardcoded shape — proving arbitrary guest IR renders on the host GPU.
pub fn selftest_replay(out: &str) -> ! {
    use crate::metal::{cfrelease, create_iosurface};
    use dd_gpu::ir::*;
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("selftest-replay: no Metal device");
        std::process::exit(1);
    };
    let (w, h): (u32, u32) = (256, 256);
    unsafe {
        let surf = create_iosurface(w, h);
        if surf.is_null() {
            eprintln!("selftest-replay: IOSurfaceCreate failed");
            std::process::exit(1);
        }
        let tex = ctx.texture_from_iosurface(surf, w, h);
        let mut be = MetalBackend::new(&ctx);
        be.set_render_target(1, tex.clone()); // texture id 1 = the IOSurface render target

        // Quad: 4 corners (TL red, TR green, BL blue, BR yellow), 2 triangles. Each vertex = pos.xy +
        // color.rgba (24 bytes). The guest describes THIS geometry entirely via the buffer + draw.
        let v: [[f32; 6]; 6] = [
            [-0.8, 0.8, 1.0, 0.2, 0.2, 1.0],
            [-0.8, -0.8, 0.2, 0.2, 1.0, 1.0],
            [0.8, 0.8, 0.2, 1.0, 0.2, 1.0],
            [0.8, 0.8, 0.2, 1.0, 0.2, 1.0],
            [-0.8, -0.8, 0.2, 0.2, 1.0, 1.0],
            [0.8, -0.8, 1.0, 1.0, 0.2, 1.0],
        ];
        let mut data = Vec::new();
        for vert in &v {
            for f in vert {
                data.extend_from_slice(&f.to_le_bytes());
            }
        }
        let cmds = vec![
            Cmd::CreateBuffer(
                10,
                BufferDesc {
                    size: data.len() as u64,
                    usage: buffer_usage::VERTEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 10,
                offset: 0,
                data,
            },
            Cmd::CreateShader {
                id: 20,
                spirv: vec![],
            },
            Cmd::CreateRenderPipeline(
                30,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 20,
                        entry: "vcmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 20,
                        entry: "fcmain".into(),
                    }),
                    vertex_buffers: vec![VertexLayout {
                        stride: 24,
                        step_mode: 0,
                        attrs: vec![
                            VertexAttr {
                                location: 0,
                                format: 0,
                                offset: 0,
                            },
                            VertexAttr {
                                location: 1,
                                format: 0,
                                offset: 8,
                            },
                        ],
                    }],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Bgra8Unorm,
                        blend: None,
                        write_mask: 0xf,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.09, 0.09, 0.14, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 10,
                        offset: 0,
                    },
                    Enc::Draw {
                        vertex_count: 6,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        let bytes = encode_stream(&cmds);
        eprintln!(
            "selftest-replay: streaming {} IR cmds ({} bytes)",
            cmds.len(),
            bytes.len()
        );
        dd_gpu::replay::replay_stream(&mut be, &bytes).expect("replay_stream");

        let dst = ctx.new_bgra_texture(w, h);
        ctx.blit(&tex, &dst);
        let bgra = ctx.readback_bgra(&dst, w, h);
        cfrelease(surf);
        let mut rgba = vec![0u8; bgra.len()];
        for i in (0..bgra.len()).step_by(4) {
            rgba[i] = bgra[i + 2];
            rgba[i + 1] = bgra[i + 1];
            rgba[i + 2] = bgra[i];
            rgba[i + 3] = 0xff;
        }
        let png = dd_term_core::png::encode_rgba(w, h, &rgba);
        let _ = std::fs::write(out, png);
    }
    println!("selftest-replay: replayed a dd-gpu IR quad onto Metal -> {out}");
    std::process::exit(0);
}

fn prim(t: Topology) -> MTLPrimitiveType {
    match t {
        Topology::PointList => MTLPrimitiveType::Point,
        Topology::LineList => MTLPrimitiveType::Line,
        Topology::LineStrip => MTLPrimitiveType::LineStrip,
        Topology::TriangleStrip => MTLPrimitiveType::TriangleStrip,
        Topology::TriangleList => MTLPrimitiveType::Triangle,
    }
}

impl GpuBackend for MetalBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "dd-metal".into(),
            unified_memory: true,
            supports_compute: false,
            supports_graphics: true,
            max_texture_2d: 16384,
            present_kinds: vec![],
        }
    }

    fn create_buffer(&mut self, id: BufferId, desc: &BufferDesc) -> Result<()> {
        let len = desc.size.max(4) as usize;
        let buf = self
            .device
            .newBufferWithLength_options(len, MTLResourceOptions::MTLResourceStorageModeShared)
            .ok_or(GpuError::Unsupported("newBuffer"))?;
        self.buffers.insert(id.0, buf);
        Ok(())
    }
    fn destroy_buffer(&mut self, id: BufferId) -> Result<()> {
        self.buffers.remove(&id.0);
        Ok(())
    }
    fn write_buffer(&mut self, id: BufferId, offset: u64, data: &[u8]) -> Result<()> {
        let buf = self.buffers.get(&id.0).ok_or(GpuError::UnknownId {
            kind: "buffer",
            id: id.0,
        })?;
        // The guest streams WriteBuffer over the socket (untrusted IR); reject any range that would write
        // past the buffer rather than corrupting adjacent heap.
        let cap = buf.length();
        let end = (offset as usize).checked_add(data.len());
        if end.map_or(true, |e| e > cap) {
            // Untrusted IR: an out-of-range write must fail the replay (so the executor acks failure),
            // not be silently skipped while the guest believes the upload landed.
            eprintln!("[dd-display/metal_backend] write_buffer OOB: id={} offset={} len={} cap={}",
                id.0, offset, data.len(), cap);
            return Err(GpuError::OutOfBounds);
        }
        unsafe {
            let base = buf.contents().as_ptr() as *mut u8;
            std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(offset as usize), data.len());
        }
        Ok(())
    }

    fn create_texture(&mut self, id: TextureId, desc: &TextureDesc) -> Result<()> {
        // Materialize a real texture honoring the guest's format/usage: a SAMPLED texture (glmark2 texture
        // scene, Chrome UI) is Shared-storage + ShaderRead so we can blit pixels into it from a staging
        // buffer (CopyBufferToTexture); anything else falls back to a BGRA target.
        // NB: unlike the checked software/mock oracle (which rejects a duplicate id), the streaming
        // executor is intentionally *idempotent* on re-create — the guest re-issues the same create for
        // the same id every frame, and the resource + PSO caches key off the descriptor so a same-desc
        // re-create is a no-op rather than a DuplicateId error. Format handling no longer silently
        // reinterprets unknown formats as BGRA (see tex_pixel_format).
        //
        // A guest create for the executor's reserved present id is NO LONGER swallowed: present targets
        // live in their own `present_targets` namespace (see set_render_target), so this materializes a
        // distinct guest texture that shadows — never aliases — the presented surface.
        let key = texture_desc_key(desc);
        if self.texture_descs.get(&id.0) == Some(&key) && self.textures.contains_key(&id.0) {
            return Ok(());
        }
        use dd_gpu::ir::texture_usage;
        let sampled = desc.usage & texture_usage::SAMPLED != 0;
        let d = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                tex_pixel_format(desc.format),
                desc.width.max(1) as usize,
                desc.height.max(1) as usize,
                false,
            )
        };
        let mut usage = MTLTextureUsage::ShaderRead;
        if desc.usage & texture_usage::RENDER_TARGET != 0 || !sampled {
            usage |= MTLTextureUsage::RenderTarget;
        }
        d.setUsage(usage);
        d.setStorageMode(MTLStorageMode::Shared);
        let tex = self
            .device
            .newTextureWithDescriptor(&d)
            .ok_or(GpuError::Unsupported("newTexture"))?;
        self.textures.insert(id.0, tex);
        self.texture_descs.insert(id.0, key);
        Ok(())
    }
    fn destroy_texture(&mut self, id: TextureId) -> Result<()> {
        self.textures.remove(&id.0);
        self.texture_descs.remove(&id.0);
        Ok(())
    }

    fn create_sampler(&mut self, id: SamplerId, desc: &SamplerDesc) -> Result<()> {
        use dd_gpu::ir::{AddressMode, Filter};
        let sd = MTLSamplerDescriptor::new();
        let filt = |f: Filter| match f {
            Filter::Nearest => MTLSamplerMinMagFilter::Nearest,
            Filter::Linear => MTLSamplerMinMagFilter::Linear,
        };
        let addr = |a: AddressMode| match a {
            AddressMode::ClampToEdge => MTLSamplerAddressMode::ClampToEdge,
            AddressMode::Repeat => MTLSamplerAddressMode::Repeat,
            AddressMode::MirrorRepeat => MTLSamplerAddressMode::MirrorRepeat,
        };
        let mip = |f: Filter| match f {
            Filter::Nearest => MTLSamplerMipFilter::Nearest,
            Filter::Linear => MTLSamplerMipFilter::Linear,
        };
        sd.setMinFilter(filt(desc.min_filter));
        sd.setMagFilter(filt(desc.mag_filter));
        // Apply ALL descriptor fields — dropping mip_filter / address_w silently changes sampling.
        sd.setMipFilter(mip(desc.mip_filter));
        sd.setSAddressMode(addr(desc.address_u));
        sd.setTAddressMode(addr(desc.address_v));
        sd.setRAddressMode(addr(desc.address_w));
        let s = self
            .device
            .newSamplerStateWithDescriptor(&sd)
            .ok_or(GpuError::Unsupported("newSampler"))?;
        self.samplers.insert(id.0, s);
        Ok(())
    }
    fn destroy_sampler(&mut self, id: SamplerId) -> Result<()> {
        self.samplers.remove(&id.0);
        Ok(())
    }

    fn create_shader(&mut self, id: ShaderId, spirv: &[u32]) -> Result<()> {
        // The guest's GLSL, translated to MSL by the shim, arrives packed as bytes. Compile it to a
        // library; if the payload isn't MSL (empty/real SPIR-V), leave it unset → builtin pipeline.
        if let Some(src) = msl_from_words(spirv) {
            let key = hash_bytes(src.as_bytes());
            // L3: identical MSL already installed for this id → nothing to compile (steady state).
            if self.shader_id_hash.get(&id.0) == Some(&key)
                && (self.shaders.contains_key(&id.0) || self.failed_shader_cache.contains(&key))
            {
                return Ok(());
            }
            if self.failed_shader_cache.contains(&key) {
                self.shaders.remove(&id.0);
                self.shader_id_hash.insert(id.0, key);
                return Ok(());
            }
            // Content-cache hit (same MSL compiled earlier under any id) → reuse the library, no compile.
            let lib = if let Some(cached) = self.shader_lib_cache.get(&key) {
                cached.clone()
            } else {
                match self
                    .device
                    .newLibraryWithSource_options_error(&NSString::from_str(&src), None)
                {
                    Ok(lib) => {
                        self.shader_compiles += 1;
                        self.shader_lib_cache.insert(key, lib.clone());
                        lib
                    }
                    Err(e) => {
                        eprintln!("dd-metal: shader {} MSL compile failed: {e:?}", id.0);
                        let dir = std::env::var("DD_SHADER_DUMP_DIR")
                            .unwrap_or_else(|_| "/tmp/dd-metal-shaders".into());
                        if let Err(err) = std::fs::create_dir_all(&dir) {
                            eprintln!("dd-metal: shader dump mkdir {dir}: {err}");
                        } else {
                            let path = format!("{dir}/shader-{}-{key:016x}.metal", id.0);
                            match std::fs::write(&path, &src) {
                                Ok(()) => {
                                    eprintln!("dd-metal: dumped failed shader {} to {path}", id.0)
                                }
                                Err(err) => eprintln!("dd-metal: shader dump write {path}: {err}"),
                            }
                        }
                        self.failed_shader_cache.insert(key);
                        self.shaders.remove(&id.0);
                        self.shader_id_hash.insert(id.0, key);
                        return Ok(());
                    }
                }
            };
            self.shaders.insert(id.0, lib);
            self.shader_id_hash.insert(id.0, key);
        } else {
            // The payload is not MSL (empty / real SPIR-V). Drop any library previously installed under
            // this id so a recreate can't leave a *stale* shader that later pipelines would compile
            // against — the id now has no usable MSL and falls back to the builtin.
            self.shaders.remove(&id.0);
            self.shader_id_hash.remove(&id.0);
        }
        Ok(())
    }
    fn destroy_shader(&mut self, id: ShaderId) -> Result<()> {
        self.shaders.remove(&id.0);
        self.shader_id_hash.remove(&id.0);
        Ok(())
    }

    fn create_render_pipeline(&mut self, id: PipelineId, desc: &RenderPipelineDesc) -> Result<()> {
        // L3: content-key the PSO. Identical descriptor already installed for this id → done; otherwise a
        // cache hit clones the compiled state (no PSO link). Only a genuine miss reaches the compile below.
        let key = self.hash_pipeline_key(desc);
        if self.pipeline_id_hash.get(&id.0) == Some(&key) && self.pipelines.contains_key(&id.0) {
            return Ok(());
        }
        if let Some((state, primitive, depth)) = self.pipeline_cache.get(&key) {
            let (state, primitive, depth) = (state.clone(), *primitive, *depth);
            self.pipelines.insert(
                id.0,
                Pipeline {
                    state,
                    primitive,
                    depth,
                },
            );
            self.pipeline_id_hash.insert(id.0, key);
            return Ok(());
        }
        // Prefer the guest's own translated shaders (per module); fall back to the builtin vertex-color
        // library + its vcmain/fcmain entry points when the app didn't ship MSL.
        let vlib = self.shaders.get(&desc.vertex.module).unwrap_or(&self.lib);
        let ventry = if self.shaders.contains_key(&desc.vertex.module) {
            desc.vertex.entry.as_str()
        } else {
            "vcmain"
        };
        let vfn = vlib
            .newFunctionWithName(&NSString::from_str(ventry))
            .ok_or(GpuError::Unsupported("vertex fn"))?;
        let (flib, fentry) = match &desc.fragment {
            Some(f) if self.shaders.contains_key(&f.module) => {
                (self.shaders.get(&f.module).unwrap(), f.entry.as_str())
            }
            _ => (&self.lib, "fcmain"),
        };
        let ffn = flib
            .newFunctionWithName(&NSString::from_str(fentry))
            .ok_or(GpuError::Unsupported("fragment fn"))?;
        let pdesc = MTLRenderPipelineDescriptor::new();
        pdesc.setVertexFunction(Some(&vfn));
        pdesc.setFragmentFunction(Some(&ffn));
        unsafe {
            let ca = pdesc.colorAttachments().objectAtIndexedSubscript(0);
            let color_format = desc
                .color_targets
                .first()
                .map(|target| tex_pixel_format(target.format))
                .unwrap_or(MTLPixelFormat::BGRA8Unorm);
            ca.setPixelFormat(color_format);
            if let Some(target) = desc.color_targets.first() {
                ca.setWriteMask(metal_write_mask(target.write_mask));
            }
            if let Some(blend) = desc.color_targets.first().and_then(|c| c.blend.as_ref()) {
                ca.setBlendingEnabled(true);
                ca.setSourceRGBBlendFactor(metal_blend_factor(blend.src_color));
                ca.setDestinationRGBBlendFactor(metal_blend_factor(blend.dst_color));
                ca.setRgbBlendOperation(metal_blend_op(blend.op_color));
                ca.setSourceAlphaBlendFactor(metal_blend_factor(blend.src_alpha));
                ca.setDestinationAlphaBlendFactor(metal_blend_factor(blend.dst_alpha));
                ca.setAlphaBlendOperation(metal_blend_op(blend.op_alpha));
            }
        }
        // Vertex layout: derive the Metal vertex descriptor from the guest's attributes. The IR carries
        // one `VertexLayout` per SOURCE VBO — glmark2/ANGLE bind a separate tightly-packed buffer per
        // attribute (position in one, normal in another) — where layout index i == the guest's vertex-
        // buffer slot i. Each attribute references its layout's Metal buffer via setBufferIndex, so a
        // secondary attribute (normal/texcoord) reads its own stream instead of aliasing slot 0. `format`
        // carries the guest vertex attribute type/normalization/component count in a compact shim-private
        // encoding. Vertex buffers live at VBUF_BASE.. (see const) so they never collide with the uniform
        // block at `[[buffer(1)]]`; `SetVertexBuffer` applies the same base. Falls back to the builtin
        // float2+float4 layout at VBUF_BASE.
        let vd = MTLVertexDescriptor::vertexDescriptor();
        let any_attrs = desc.vertex_buffers.iter().any(|l| !l.attrs.is_empty());
        unsafe {
            if any_attrs {
                for (li, layout) in desc.vertex_buffers.iter().enumerate() {
                    let slot = VBUF_BASE + li;
                    for a in &layout.attrs {
                        let mf = metal_vertex_format(a.format);
                        let ad = vd
                            .attributes()
                            .objectAtIndexedSubscript(a.location as usize);
                        ad.setFormat(mf);
                        ad.setOffset(a.offset as usize);
                        ad.setBufferIndex(slot);
                    }
                    vd.layouts()
                        .objectAtIndexedSubscript(slot)
                        .setStride(layout.stride.max(4) as usize);
                }
            } else {
                let a0 = vd.attributes().objectAtIndexedSubscript(0);
                a0.setFormat(MTLVertexFormat::Float2);
                a0.setOffset(0);
                a0.setBufferIndex(VBUF_BASE);
                let a1 = vd.attributes().objectAtIndexedSubscript(1);
                a1.setFormat(MTLVertexFormat::Float4);
                a1.setOffset(8);
                a1.setBufferIndex(VBUF_BASE);
                vd.layouts()
                    .objectAtIndexedSubscript(VBUF_BASE)
                    .setStride(24);
            }
        }
        pdesc.setVertexDescriptor(Some(&vd));
        let depth = desc.depth.is_some();
        if depth {
            pdesc.setDepthAttachmentPixelFormat(MTLPixelFormat::Depth32Float);
        }
        let state = self
            .device
            .newRenderPipelineStateWithDescriptor_error(&pdesc)
            .map_err(|_| GpuError::Unsupported("pipeline compile"))?;
        self.pipeline_compiles += 1;
        let primitive = prim(desc.topology);
        self.pipeline_cache
            .insert(key, (state.clone(), primitive, depth));
        self.pipeline_id_hash.insert(id.0, key);
        self.pipelines.insert(
            id.0,
            Pipeline {
                state,
                primitive,
                depth,
            },
        );
        Ok(())
    }

    fn create_bind_group(&mut self, id: BindGroupId, desc: &BindGroupDesc) -> Result<()> {
        // `binding` is the MSL resource index used in both stages: a uniform block at `[[buffer(1)]]`, a
        // sampled texture at `[[texture(n)]]`, a sampler at `[[sampler(n)]]`. Record all three kinds.
        let mut binds = Vec::new();
        for e in &desc.entries {
            match e.resource {
                BindResource::Buffer {
                    id: bid, offset, ..
                } => {
                    // Validate the referenced resource exists up front (parity with the software/mock
                    // oracle) so a missing id is a typed error, not a nil binding / black frame later.
                    if !self.buffers.contains_key(&bid) {
                        return Err(GpuError::UnknownId { kind: "buffer", id: bid });
                    }
                    binds.push(Bind::Buffer {
                        index: e.binding,
                        buffer: bid,
                        offset,
                    });
                }
                BindResource::Texture { id: tid } => {
                    if !self.has_texture(tid) {
                        return Err(GpuError::UnknownId { kind: "texture", id: tid });
                    }
                    binds.push(Bind::Texture {
                        index: e.binding,
                        texture: tid,
                    });
                }
                BindResource::Sampler { id: sid } => {
                    if !self.samplers.contains_key(&sid) {
                        return Err(GpuError::UnknownId { kind: "sampler", id: sid });
                    }
                    binds.push(Bind::Sampler {
                        index: e.binding,
                        sampler: sid,
                    });
                }
            }
        }
        self.bind_groups.insert(id.0, binds);
        Ok(())
    }
    fn destroy_bind_group(&mut self, id: BindGroupId) -> Result<()> {
        self.bind_groups.remove(&id.0);
        Ok(())
    }

    fn destroy_pipeline(&mut self, id: PipelineId) -> Result<()> {
        // Match the checked backends' cleanup: releasing a pipeline drops the id's installed state
        // (the cached compiled PSO is keyed by content hash and shared, so it is left intact).
        self.pipelines.remove(&id.0);
        self.pipeline_id_hash.remove(&id.0);
        Ok(())
    }

    fn submit(&mut self, cb: &CommandBuffer) -> Result<()> {
        let cmd = self
            .queue
            .commandBuffer()
            .ok_or(GpuError::Unsupported("commandBuffer"))?;
        // L4: async submit + cross-queue tearing fence. Reserve this frame's render generation for the
        // target IOSurface and make the render pass WAIT for the compositor's previous blit of that surface
        // (fence b) before it begins — encoded at the very start of the command buffer, before any encoder.
        // The matching SIGNAL (render-complete → fence a) is encoded after all encoders, just before commit.
        let fence = if crate::metal::async_on() && self.cur_surface_id != 0 {
            crate::metal::fence_begin_render(self.cur_surface_id)
        } else {
            None
        };
        if let Some((_render_ev, present_ev, _gen, wait_present)) = &fence {
            cmd.encodeWaitForEvent_value(present_ev, *wait_present);
        }
        let mut enc: Option<Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>> = None;
        let mut cur_prim = MTLPrimitiveType::Triangle;
        // Bound index buffer for glDrawElements → drawIndexedPrimitives (buffer id, byte offset, U16/U32).
        let mut index_buffer: Option<(u32, u64, MTLIndexType)> = None;
        // Whether the CURRENT render pass draws into an offscreen render target (not the presented
        // surface): if so, its viewport is Y-flipped (negative height) so the texture is stored
        // bottom-left (GL convention) and samples upright in the later composite.
        let mut flip_pass = false;
        // Guard against the guest shim's clear-vs-load heuristic false-clearing a REUSED offscreen tile.
        // The shim decides a pass's load-op from a *global* clear serial (bumped by every glClear on any
        // FBO); when Chrome renders content into tile T, clears an unrelated tile, then draws MORE into T
        // in the same frame (e.g. compositing a byline child render pass onto a gradient-header tile), the
        // serial has advanced, so the shim emits load=Clear on T's second pass — wiping T's real content
        // before the surface pass ever samples it (the dropped-top-tile bug on gradient/complex pages). An
        // ACTUAL glClear is carried separately as a ClearRect op, so we can tell the two apart: if an
        // offscreen target was already rendered earlier in THIS command buffer and has NOT been ClearRect'd
        // since, a load=Clear is the shim's false positive → force Load to preserve the prior content.
        let mut rendered_offscreen: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut cleared_since: std::collections::HashSet<u32> = std::collections::HashSet::new();
        // Height of the CURRENT render pass's color target — the offscreen Y-flip mirrors a viewport/scissor
        // across the FULL tile height, not just within a sub-band, so a partial sub-viewport (e.g. a CSS
        // opacity/clip child render pass) lands and clips consistently with full-tile passes.
        let mut cur_target_h: u32 = 0;
        for op in &cb.encoder {
            match op {
                Enc::BeginRenderPass { color, depth } => {
                    let pass = unsafe { MTLRenderPassDescriptor::renderPassDescriptor() };
                    let mut cw = 0u32;
                    let mut chh = 0u32;
                    for (i, ca) in color.iter().enumerate() {
                        let tex = self.resolve_texture(ca.texture).ok_or(GpuError::UnknownId {
                            kind: "texture",
                            id: ca.texture,
                        })?;
                        cw = tex.width() as u32;
                        chh = tex.height() as u32;
                        // Preserve a reused offscreen tile's content when the shim's global-serial heuristic
                        // spuriously asked to clear it (see rendered_offscreen note above). Only the first
                        // color attachment names the tile; a real clear (ClearRect) removes it from the set.
                        let is_offscreen = !self.surface_ids.contains(&ca.texture);
                        let force_load = i == 0
                            && is_offscreen
                            && ca.load == LoadOp::Clear
                            && rendered_offscreen.contains(&ca.texture)
                            && !cleared_since.contains(&ca.texture);
                        let att = unsafe { pass.colorAttachments().objectAtIndexedSubscript(i) };
                        att.setTexture(Some(tex));
                        att.setLoadAction(if ca.load == LoadOp::Clear && !force_load {
                            MTLLoadAction::Clear
                        } else {
                            MTLLoadAction::Load
                        });
                        let c = ca.clear;
                        att.setClearColor(MTLClearColor {
                            red: c[0] as f64,
                            green: c[1] as f64,
                            blue: c[2] as f64,
                            alpha: c[3] as f64,
                        });
                        att.setStoreAction(MTLStoreAction::Store);
                    }
                    if let Some(da) = depth {
                        let dtex = self.depth_texture(cw.max(1), chh.max(1));
                        let datt = unsafe { pass.depthAttachment() };
                        datt.setTexture(Some(&dtex));
                        datt.setLoadAction(MTLLoadAction::Clear);
                        datt.setClearDepth(da.clear_depth as f64);
                        datt.setStoreAction(MTLStoreAction::DontCare);
                    }
                    let e = cmd
                        .renderCommandEncoderWithDescriptor(&pass)
                        .ok_or(GpuError::Unsupported("encoder"))?;
                    // Y-flip this pass iff it targets an OFFSCREEN render texture (a content tile, not the
                    // presented surface). Prime a default full-target flipped viewport up front so passes
                    // that never emit their own SetViewport are still stored bottom-left; an explicit
                    // guest SetViewport below overrides it (also flipped).
                    flip_pass = color
                        .first()
                        .is_some_and(|ca| !self.surface_ids.contains(&ca.texture));
                    cur_target_h = chh;
                    // Record this offscreen tile as rendered-this-frame and consume any pending clear, so a
                    // later reuse pass in the same command buffer preserves (loads) its content unless it is
                    // explicitly ClearRect'd in between.
                    if let Some(ca) = color.first() {
                        if !self.surface_ids.contains(&ca.texture) {
                            rendered_offscreen.insert(ca.texture);
                            cleared_since.remove(&ca.texture);
                        }
                    }
                    if flip_pass {
                        e.setViewport(MTLViewport {
                            originX: 0.0,
                            originY: chh as f64,
                            width: cw as f64,
                            height: -(chh as f64),
                            znear: 0.0,
                            zfar: 1.0,
                        });
                    }
                    enc = Some(e);
                }
                Enc::SetPipeline(p) => {
                    let pl = self.pipelines.get(p).ok_or(GpuError::UnknownId {
                        kind: "pipeline",
                        id: *p,
                    })?;
                    cur_prim = pl.primitive;
                    let pd = pl.depth;
                    if let Some(e) = &enc {
                        e.setRenderPipelineState(&pl.state);
                        if pd {
                            e.setDepthStencilState(Some(&self.depth_state));
                        }
                    }
                }
                Enc::SetVertexBuffer {
                    slot,
                    buffer,
                    offset,
                } => {
                    let buf = self.buffers.get(buffer).ok_or(GpuError::UnknownId {
                        kind: "buffer",
                        id: *buffer,
                    })?;
                    if let Some(e) = &enc {
                        // Bind at VBUF_BASE + slot to match the vertex descriptor's buffer indices (keeps
                        // guest vertex buffers clear of the uniform block at [[buffer(1)]]).
                        unsafe {
                            e.setVertexBuffer_offset_atIndex(
                                Some(buf),
                                *offset as usize,
                                VBUF_BASE + *slot as usize,
                            )
                        };
                    }
                }
                Enc::Draw {
                    vertex_count,
                    instance_count,
                    first_vertex,
                    ..
                } => {
                    if let Some(e) = &enc {
                        unsafe {
                            e.drawPrimitives_vertexStart_vertexCount_instanceCount(
                                cur_prim,
                                *first_vertex as usize,
                                *vertex_count as usize,
                                (*instance_count).max(1) as usize,
                            )
                        };
                    }
                }
                Enc::SetBindGroup { group, .. } => {
                    // Bind each resource in the group to BOTH stages at its index: uniform buffers (read by
                    // vertex + fragment), sampled textures, and samplers. Snapshot the (kind,index,id) list
                    // first to avoid holding a borrow of self while also borrowing enc.
                    let plan: Vec<(u8, u32, u32, u64)> = self
                        .bind_groups
                        .get(group)
                        .map(|v| {
                            v.iter()
                                .map(|b| match b {
                                    Bind::Buffer {
                                        index,
                                        buffer,
                                        offset,
                                    } => (0u8, *index, *buffer, *offset),
                                    Bind::Texture { index, texture } => (1, *index, *texture, 0),
                                    Bind::Sampler { index, sampler } => (2, *index, *sampler, 0),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    for (kind, idx, id, off) in plan {
                        let Some(e) = &enc else { continue };
                        unsafe {
                            match kind {
                                0 => {
                                    if let Some(buf) = self.buffers.get(&id) {
                                        e.setVertexBuffer_offset_atIndex(
                                            Some(buf),
                                            off as usize,
                                            idx as usize,
                                        );
                                        e.setFragmentBuffer_offset_atIndex(
                                            Some(buf),
                                            off as usize,
                                            idx as usize,
                                        );
                                    }
                                }
                                1 => {
                                    if let Some(tex) = self.resolve_texture(id) {
                                        e.setVertexTexture_atIndex(Some(tex), idx as usize);
                                        e.setFragmentTexture_atIndex(Some(tex), idx as usize);
                                    }
                                }
                                _ => {
                                    if let Some(s) = self.samplers.get(&id) {
                                        e.setVertexSamplerState_atIndex(Some(s), idx as usize);
                                        e.setFragmentSamplerState_atIndex(Some(s), idx as usize);
                                    }
                                }
                            }
                        }
                    }
                }
                Enc::SetIndexBuffer {
                    buffer,
                    offset,
                    format,
                } => {
                    let it = match format {
                        IndexFormat::U16 => MTLIndexType::UInt16,
                        IndexFormat::U32 => MTLIndexType::UInt32,
                    };
                    index_buffer = Some((*buffer, *offset, it));
                }
                Enc::SetViewport {
                    x,
                    y,
                    w,
                    h,
                    min_depth,
                    max_depth,
                } => {
                    if let Some(e) = &enc {
                        let hh = (*h).max(0.0) as f64;
                        // Offscreen render target: negative-height viewport mirrors NDC-y so the texture is
                        // stored bottom-left like GL and samples upright. The mirror is across the FULL tile
                        // height (originY = H - y), not the sub-viewport band (y + h) — those coincide only
                        // for a full-tile viewport; a partial sub-viewport (a CSS opacity/clip child render
                        // pass) must flip about the tile so its content lands where the flipped scissor clips.
                        let (origin_y, height) = if flip_pass {
                            (cur_target_h as f64 - *y as f64, -hh)
                        } else {
                            (*y as f64, hh)
                        };
                        e.setViewport(MTLViewport {
                            originX: *x as f64,
                            originY: origin_y,
                            width: (*w).max(0.0) as f64,
                            height,
                            znear: *min_depth as f64,
                            zfar: *max_depth as f64,
                        });
                    }
                }
                Enc::SetScissor { x, y, w, h } => {
                    if let Some(e) = &enc {
                        // The offscreen Y-flip mirrors content across the full tile height (see SetViewport),
                        // so the scissor must mirror to match: a guest rect [y, y+h) clips the flipped rows
                        // [H-(y+h), H-y). For a full-tile scissor this is a no-op (H-(0+H)=0); for a partial
                        // scissor (opacity/clip child pass) it keeps the clip aligned with the flipped content.
                        let sy = if flip_pass {
                            cur_target_h.saturating_sub(*y + *h)
                        } else {
                            *y
                        };
                        e.setScissorRect(MTLScissorRect {
                            x: *x as _,
                            y: sy as _,
                            width: *w as _,
                            height: *h as _,
                        });
                    }
                }
                Enc::DrawIndexed {
                    index_count,
                    instance_count,
                    first_index,
                    base_vertex,
                    first_instance,
                } => {
                    if let (Some(e), Some((bid, base_off, it))) = (&enc, index_buffer) {
                        if let Some(ibuf) = self.buffers.get(&bid) {
                            let esz = if it == MTLIndexType::UInt16 { 2u64 } else { 4 };
                            let off = base_off + *first_index as u64 * esz;
                            // Honor base_vertex (added to every fetched index — glDrawElementsBaseVertex)
                            // and first_instance via the full drawIndexedPrimitives variant.
                            unsafe {
                                e.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset_instanceCount_baseVertex_baseInstance(
                                    cur_prim,
                                    *index_count as usize,
                                    it,
                                    ibuf,
                                    off as usize,
                                    (*instance_count).max(1) as usize,
                                    *base_vertex as isize,
                                    *first_instance as usize,
                                )
                            };
                        }
                    }
                }
                Enc::CopyBufferToTexture {
                    src,
                    src_offset,
                    bytes_per_row,
                    dst,
                    width,
                    height,
                    ..
                } => {
                    // Upload a staging buffer's pixels into a sampled texture (glTexImage2D path). Runs on a
                    // standalone blit encoder between passes; a render encoder must not be open here.
                    let (Some(buf), Some(tex)) = (self.buffers.get(src), self.resolve_texture(*dst))
                    else {
                        return Err(GpuError::UnknownId { kind: "buffer/texture", id: *src });
                    };
                    {
                        // Untrusted IR: the staging buffer must hold src_offset + bytes_per_row*height, else
                        // Metal reads OOB. Verify before encoding the copy; an out-of-range copy fails the
                        // replay (surfacing to the executor ack) rather than being silently dropped.
                        let need = (*bytes_per_row as usize)
                            .checked_mul(*height as usize)
                            .and_then(|n| n.checked_add(*src_offset as usize));
                        if need.map_or(true, |n| n > buf.length()) {
                            eprintln!("[dd-display/metal_backend] CopyBufferToTexture OOB: src_off={} bpr={} h={} buf_len={}",
                                src_offset, bytes_per_row, height, buf.length());
                            return Err(GpuError::OutOfBounds);
                        } else if let Some(blit) = cmd.blitCommandEncoder() {
                            unsafe {
                                blit.copyFromBuffer_sourceOffset_sourceBytesPerRow_sourceBytesPerImage_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                                    buf,
                                    *src_offset as usize,
                                    *bytes_per_row as usize,
                                    (*bytes_per_row as usize) * (*height as usize),
                                    MTLSize { width: *width as usize, height: *height as usize, depth: 1 },
                                    tex,
                                    0,
                                    0,
                                    MTLOrigin { x: 0, y: 0, z: 0 },
                                );
                            }
                            blit.endEncoding();
                        }
                    }
                }
                Enc::ClearRect {
                    texture,
                    x,
                    y,
                    w,
                    h,
                    color,
                } => {
                    if let Some(e) = enc.take() {
                        e.endEncoding();
                    }
                    // An explicit clear of this target: a following load=Clear pass on it is genuine (not the
                    // shim's global-serial false positive), so let it clear rather than being forced to Load.
                    cleared_since.insert(*texture);
                    let Some(tex) = self.resolve_texture(*texture).cloned() else {
                        continue;
                    };
                    let tw = tex.width() as u32;
                    let th = tex.height() as u32;
                    let x0 = (*x).min(tw);
                    let y0 = (*y).min(th);
                    let x1 = x.saturating_add(*w).min(tw);
                    let y1 = y.saturating_add(*h).min(th);
                    if x1 <= x0 || y1 <= y0 {
                        continue;
                    }
                    let state = self.clear_pipeline()?;
                    let pass = unsafe { MTLRenderPassDescriptor::renderPassDescriptor() };
                    unsafe {
                        let att = pass.colorAttachments().objectAtIndexedSubscript(0);
                        att.setTexture(Some(&tex));
                        att.setLoadAction(MTLLoadAction::Load);
                        att.setStoreAction(MTLStoreAction::Store);
                    }
                    let e = cmd
                        .renderCommandEncoderWithDescriptor(&pass)
                        .ok_or(GpuError::Unsupported("clear encoder"))?;
                    e.setRenderPipelineState(&state);
                    e.setScissorRect(MTLScissorRect {
                        x: x0 as _,
                        y: y0 as _,
                        width: (x1 - x0) as _,
                        height: (y1 - y0) as _,
                    });
                    let c = *color;
                    let verts: [f32; 36] = [
                        -1.0, -1.0, c[0], c[1], c[2], c[3], 1.0, -1.0, c[0], c[1], c[2], c[3],
                        -1.0, 1.0, c[0], c[1], c[2], c[3], 1.0, -1.0, c[0], c[1], c[2], c[3], 1.0,
                        1.0, c[0], c[1], c[2], c[3], -1.0, 1.0, c[0], c[1], c[2], c[3],
                    ];
                    let nbytes = std::mem::size_of_val(&verts);
                    let vbuf = self
                        .device
                        .newBufferWithLength_options(
                            nbytes,
                            MTLResourceOptions::MTLResourceStorageModeShared,
                        )
                        .ok_or(GpuError::Unsupported("clear buffer"))?;
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            verts.as_ptr() as *const u8,
                            vbuf.contents().as_ptr() as *mut u8,
                            nbytes,
                        );
                        e.setVertexBuffer_offset_atIndex(Some(&vbuf), 0, VBUF_BASE);
                        e.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 6);
                    }
                    e.endEncoding();
                }
                Enc::EndRenderPass => {
                    if let Some(e) = enc.take() {
                        e.endEncoding();
                    }
                }
                // Compute-pass markers carry no GPU work in this render-only slice — tolerated as no-ops.
                Enc::BeginComputePass | Enc::EndComputePass => {}
                // A supported IR command the render executor cannot perform (compute dispatch, buffer→buffer
                // or texture→buffer copy) must NOT be silently dropped — that would let the guest believe
                // work happened. Surface it as an error so the executor acks failure.
                Enc::Dispatch { .. }
                | Enc::CopyBufferToBuffer { .. }
                | Enc::CopyTextureToBuffer { .. } => {
                    if let Some(e) = enc.take() {
                        e.endEncoding();
                    }
                    return Err(GpuError::Unsupported("metal replay: command not implemented"));
                }
            }
        }
        if let Some(e) = enc.take() {
            e.endEncoding();
        }
        let dump_ids = std::env::var("DD_GPU_DUMP_TEXTURES").ok().map(|s| {
            s.split(',')
                .filter_map(|p| p.trim().parse::<u32>().ok())
                .collect::<Vec<_>>()
        });
        if let Some((render_ev, _present_ev, gen, _wait)) = &fence {
            // fence a: signal render-complete for this gen so the compositor's blit (which WAITs on this
            // event) never samples the surface mid-render.
            cmd.encodeSignalEvent_value(render_ev, *gen);
            cmd.commit();
            // Async: DO NOT wait for GPU completion — ack immediately (in run_executor) so the guest builds
            // the next frame while this one renders. The event fence, not a CPU stall, guarantees ordering.
            if dump_ids.as_ref().is_some_and(|ids| !ids.is_empty()) {
                let t = std::time::Instant::now();
                unsafe { cmd.waitUntilCompleted() };
                self.gpu_wait_ns = t.elapsed().as_nanos() as u64;
            } else {
                self.gpu_wait_ns = 0;
            }
        } else {
            cmd.commit();
            // Baseline (DD_RENDER_NOASYNC): CPU stalls on GPU completion — the only hop that overlaps nothing.
            let t = std::time::Instant::now();
            unsafe { cmd.waitUntilCompleted() };
            self.gpu_wait_ns = t.elapsed().as_nanos() as u64;
        }
        if let Some(ids) = dump_ids {
            if !ids.is_empty() {
                let dir = std::env::var("DD_GPU_DUMP_DIR")
                    .unwrap_or_else(|_| "/tmp/dd-gpu-textures".into());
                let seq = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                for id in ids {
                    if let Some(tex) = self.resolve_texture(id) {
                        dump_texture_png(id, tex, &dir, seq);
                    }
                }
            }
        }
        Ok(())
    }
}
