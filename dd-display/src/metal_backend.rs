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
    BindGroupDesc, BindResource, BufferDesc, CommandBuffer, Enc, IndexFormat, LoadOp, RenderPipelineDesc,
    SamplerDesc, TextureDesc, Topology,
};
use dd_gpu::{GpuError, Result};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBuffer, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLCompareFunction, MTLDepthStencilDescriptor, MTLDepthStencilState, MTLDevice, MTLIndexType, MTLLibrary,
    MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType, MTLRenderCommandEncoder,
    MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLResourceOptions,
    MTLSamplerAddressMode, MTLSamplerDescriptor, MTLSamplerMinMagFilter, MTLSamplerState, MTLSize,
    MTLStorageMode, MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureUsage, MTLVertexDescriptor,
    MTLVertexFormat,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// FNV-ish content hash of a byte slice via the std default hasher (used to content-key shader/PSO caches).
fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
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
    Buffer { index: u32, buffer: u32, offset: u64 },
    Texture { index: u32, texture: u32 },
    Sampler { index: u32, sampler: u32 },
}

/// Map a dd-gpu `TextureFormat` to the Metal pixel format the backend can materialize as a sampled/render
/// texture (color formats only; depth is handled separately).
fn tex_pixel_format(f: dd_gpu::ir::TextureFormat) -> MTLPixelFormat {
    use dd_gpu::ir::TextureFormat as F;
    match f {
        F::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        F::Rgba8Srgb => MTLPixelFormat::RGBA8Unorm_sRGB,
        F::Bgra8Srgb => MTLPixelFormat::BGRA8Unorm_sRGB,
        F::R8Unorm => MTLPixelFormat::R8Unorm,
        F::Rg8Unorm => MTLPixelFormat::RG8Unorm,
        _ => MTLPixelFormat::BGRA8Unorm,
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
    pipeline_cache: HashMap<u64, (Retained<ProtocolObject<dyn MTLRenderPipelineState>>, MTLPrimitiveType, bool)>,
    shader_id_hash: HashMap<u32, u64>, // shader id → MSL hash currently installed
    pipeline_id_hash: HashMap<u32, u64>, // pipeline id → desc hash currently installed
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
        let depth_state = ctx.device.newDepthStencilStateWithDescriptor(&dsd).expect("depth-stencil state");
        Self {
            device: ctx.device.clone(),
            queue: ctx.queue.clone(),
            lib,
            buffers: HashMap::new(),
            textures: HashMap::new(),
            pipelines: HashMap::new(),
            shaders: HashMap::new(),
            samplers: HashMap::new(),
            bind_groups: HashMap::new(),
            depth_state,
            depth_tex: None,
            shader_lib_cache: HashMap::new(),
            pipeline_cache: HashMap::new(),
            shader_id_hash: HashMap::new(),
            pipeline_id_hash: HashMap::new(),
            shader_compiles: 0,
            pipeline_compiles: 0,
            lib_compiles: 1, // the builtin BUILTIN_MSL compile just above
            gpu_wait_ns: 0,
            cur_surface_id: 0,
        }
    }

    /// Content hash of a render-pipeline descriptor for the L3 PSO cache. Folds in the *installed*
    /// shader-source hash of each referenced module so a recompiled shader (same ids, new MSL) forces a
    /// PSO rebuild even when the descriptor bytes are unchanged.
    fn hash_pipeline_key(&self, desc: &RenderPipelineDesc) -> u64 {
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
            c.blend.is_some().hash(&mut h);
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
        let t = self.device.newTextureWithDescriptor(&d).expect("depth texture");
        self.depth_tex = Some((w, h, t.clone()));
        t
    }

    /// Pre-register a texture id as the executor's render target (the rung-2 IOSurface wrapped as an
    /// MTLTexture). The guest's `BeginRenderPass` color attachment references this id.
    pub fn set_render_target(&mut self, id: u32, tex: Retained<ProtocolObject<dyn MTLTexture>>) {
        self.textures.insert(id, tex);
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
            [-0.8, 0.8, 1.0, 0.0, 0.0, 1.0], [-0.8, -0.8, 0.0, 0.0, 1.0, 1.0], [0.8, 0.8, 0.0, 1.0, 0.0, 1.0],
            [0.8, 0.8, 0.0, 1.0, 0.0, 1.0], [-0.8, -0.8, 0.0, 0.0, 1.0, 1.0], [0.8, -0.8, 1.0, 1.0, 0.0, 1.0],
        ];
        let mut data = Vec::new();
        for vert in &v {
            for f in vert {
                data.extend_from_slice(&f.to_le_bytes());
            }
        }
        let cmds = vec![
            Cmd::CreateBuffer(10, BufferDesc { size: data.len() as u64, usage: buffer_usage::VERTEX, label: String::new() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data },
            Cmd::CreateShader { id: 20, spirv: words },
            Cmd::CreateRenderPipeline(30, RenderPipelineDesc {
                vertex: ShaderRef { module: 20, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: 20, entry: "fmain".into() }),
                vertex_buffers: vec![VertexLayout { stride: 24, step_mode: 0, attrs: vec![VertexAttr { location: 0, format: 0, offset: 0 }, VertexAttr { location: 1, format: 0, offset: 8 }] }],
                color_targets: vec![ColorTargetState { format: TextureFormat::Bgra8Unorm, blend: None, write_mask: 0xf }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                label: String::new(),
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass { color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }], depth: None },
                    Enc::SetPipeline(30),
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::Draw { vertex_count: 6, instance_count: 1, first_vertex: 0, first_instance: 0 },
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
pub fn selftest_shim_ir(irfile: &str, out: &str) -> ! {
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
    let (w, h): (u32, u32) = (256, 256);
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
        readback_png(&ctx, &rt, w, h, out);
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
    match ctx.device.newLibraryWithSource_options_error(&NSString::from_str(&src), None) {
        Ok(lib) => {
            let vfn = lib.newFunctionWithName(&NSString::from_str("vmain")).is_some();
            let ffn = lib.newFunctionWithName(&NSString::from_str("fmain")).is_some();
            println!("selftest-msl: {path} COMPILED OK (vmain={vfn} fmain={ffn})");
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("selftest-msl: {path} FAILED:\n{e:?}");
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
        [-0.9, 0.9, 0.0, 0.0], [-0.9, -0.9, 0.0, 1.0], [0.9, 0.9, 1.0, 0.0],
        [0.9, 0.9, 1.0, 0.0], [-0.9, -0.9, 0.0, 1.0], [0.9, -0.9, 1.0, 1.0],
    ];
    let mut vbytes = Vec::new();
    for vert in &v {
        for f in vert {
            vbytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    // 2×2 RGBA texels: red, green, blue, white
    let tex_px: [u8; 16] = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
    unsafe {
        let surf = create_iosurface(w, h);
        let rt = ctx.texture_from_iosurface(surf, w, h);
        let mut be = MetalBackend::new(&ctx);
        be.set_render_target(1, rt.clone());
        let cmds = vec![
            Cmd::CreateBuffer(10, BufferDesc { size: vbytes.len() as u64, usage: buffer_usage::VERTEX, label: String::new() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data: vbytes },
            Cmd::CreateBuffer(12, BufferDesc { size: 16, usage: buffer_usage::COPY_SRC, label: String::new() }),
            Cmd::WriteBuffer { id: 12, offset: 0, data: tex_px.to_vec() },
            Cmd::CreateTexture(2, TextureDesc { width: 2, height: 2, depth: 1, mip_levels: 1, sample_count: 1, dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm, usage: texture_usage::SAMPLED | texture_usage::COPY_DST, label: String::new() }),
            Cmd::CreateSampler(3, SamplerDesc { min_filter: Filter::Linear, mag_filter: Filter::Linear, mip_filter: Filter::Nearest, address_u: AddressMode::ClampToEdge, address_v: AddressMode::ClampToEdge, address_w: AddressMode::ClampToEdge }),
            Cmd::CreateShader { id: 20, spirv: pack_msl(MSL) },
            Cmd::CreateRenderPipeline(30, RenderPipelineDesc {
                vertex: ShaderRef { module: 20, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: 20, entry: "fmain".into() }),
                vertex_buffers: vec![VertexLayout { stride: 16, step_mode: 0, attrs: vec![VertexAttr { location: 0, format: 2, offset: 0 }, VertexAttr { location: 1, format: 2, offset: 8 }] }],
                color_targets: vec![ColorTargetState { format: TextureFormat::Bgra8Unorm, blend: None, write_mask: 0xf }],
                depth: None, topology: Topology::TriangleList, cull: 0, front_face: 0, label: String::new(),
            }),
            Cmd::CreateBindGroup(40, BindGroupDesc { set: 0, entries: vec![
                BindEntry { binding: 0, resource: BindResource::Texture { id: 2 } },
                BindEntry { binding: 0, resource: BindResource::Sampler { id: 3 } },
            ] }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture { src: 12, src_offset: 0, bytes_per_row: 8, dst: 2, mip: 0, width: 2, height: 2 },
                    Enc::BeginRenderPass { color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }], depth: None },
                    Enc::SetPipeline(30),
                    Enc::SetBindGroup { index: 0, group: 40 },
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::Draw { vertex_count: 6, instance_count: 1, first_vertex: 0, first_instance: 0 },
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
            Cmd::CreateBuffer(10, BufferDesc { size: vbytes.len() as u64, usage: buffer_usage::VERTEX, label: String::new() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data: vbytes },
            Cmd::CreateBuffer(11, BufferDesc { size: ibytes.len() as u64, usage: buffer_usage::INDEX, label: String::new() }),
            Cmd::WriteBuffer { id: 11, offset: 0, data: ibytes },
            Cmd::CreateShader { id: 20, spirv: pack_msl(MSL) },
            Cmd::CreateRenderPipeline(30, RenderPipelineDesc {
                vertex: ShaderRef { module: 20, entry: "vmain".into() },
                fragment: Some(ShaderRef { module: 20, entry: "fmain".into() }),
                vertex_buffers: vec![VertexLayout { stride: 24, step_mode: 0, attrs: vec![VertexAttr { location: 0, format: 2, offset: 0 }, VertexAttr { location: 1, format: 4, offset: 8 }] }],
                color_targets: vec![ColorTargetState { format: TextureFormat::Bgra8Unorm, blend: None, write_mask: 0xf }],
                depth: None, topology: Topology::TriangleList, cull: 0, front_face: 0, label: String::new(),
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass { color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.05, 0.05, 0.1, 1.0], store: true }], depth: None },
                    Enc::SetPipeline(30),
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::SetIndexBuffer { buffer: 11, offset: 0, format: IndexFormat::U16 },
                    Enc::DrawIndexed { index_count: 6, instance_count: 1, first_index: 0, base_vertex: 0, first_instance: 0 },
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
            match dd_gpu::replay::replay_stream(&mut be, &bytes) {
                Ok(_) => {
                    if exec_debug() {
                        eprintln!("dd-gpu executor: replayed {len} IR bytes into IOSurface {id}");
                    }
                }
                Err(e) => eprintln!("dd-gpu executor: replay error: {e}"),
            }
            s.write_all(&[1])?; // ack: frame rendered
            if let Some(p) = prof.as_mut() {
                let replay_us = t_rx.elapsed().as_micros() as u64;
                let gpu_us = be.gpu_wait_ns / 1000;
                let (dsh, dpso, dlib) = (be.shader_compiles - cum_sh, be.pipeline_compiles - cum_pso, be.lib_compiles - cum_lib);
                cum_sh = be.shader_compiles;
                cum_pso = be.pipeline_compiles;
                cum_lib = be.lib_compiles;
                p.line(&format!("{seq},{replay_us},{gpu_us},{dsh},{dpso},{dlib},{len}"));
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
            "exec" => "seq,replay_us,gpu_us,shader_compiles,pipeline_compiles,lib_compiles,ir_bytes",
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
            Cmd::CreateBuffer(10, BufferDesc { size: data.len() as u64, usage: buffer_usage::VERTEX, label: String::new() }),
            Cmd::WriteBuffer { id: 10, offset: 0, data },
            Cmd::CreateShader { id: 20, spirv: vec![] },
            Cmd::CreateRenderPipeline(30, RenderPipelineDesc {
                vertex: ShaderRef { module: 20, entry: "vcmain".into() },
                fragment: Some(ShaderRef { module: 20, entry: "fcmain".into() }),
                vertex_buffers: vec![VertexLayout {
                    stride: 24,
                    step_mode: 0,
                    attrs: vec![VertexAttr { location: 0, format: 0, offset: 0 }, VertexAttr { location: 1, format: 0, offset: 8 }],
                }],
                color_targets: vec![ColorTargetState { format: TextureFormat::Bgra8Unorm, blend: None, write_mask: 0xf }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                label: String::new(),
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.09, 0.09, 0.14, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(30),
                    Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                    Enc::Draw { vertex_count: 6, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ];
        let bytes = encode_stream(&cmds);
        eprintln!("selftest-replay: streaming {} IR cmds ({} bytes)", cmds.len(), bytes.len());
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
        let buf = self.buffers.get(&id.0).ok_or(GpuError::UnknownId { kind: "buffer", id: id.0 })?;
        // The guest streams WriteBuffer over the socket (untrusted IR); reject any range that would write
        // past the buffer rather than corrupting adjacent heap.
        let cap = buf.length();
        let end = (offset as usize).checked_add(data.len());
        if end.map_or(true, |e| e > cap) {
            eprintln!("[dd-display/metal_backend] write_buffer OOB: id={} offset={} len={} cap={} — skipped",
                id.0, offset, data.len(), cap);
            return Ok(());
        }
        unsafe {
            let base = buf.contents().as_ptr() as *mut u8;
            std::ptr::copy_nonoverlapping(data.as_ptr(), base.add(offset as usize), data.len());
        }
        Ok(())
    }

    fn create_texture(&mut self, id: TextureId, desc: &TextureDesc) -> Result<()> {
        // The render target is injected via set_render_target (the IOSurface). A create for that same id
        // is a no-op. Otherwise materialize a real texture honoring the guest's format/usage: a SAMPLED
        // texture (glmark2 texture scene, Chrome UI) is Shared-storage + ShaderRead so we can blit pixels
        // into it from a staging buffer (CopyBufferToTexture); anything else falls back to a BGRA target.
        if self.textures.contains_key(&id.0) {
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
        let tex = self.device.newTextureWithDescriptor(&d).ok_or(GpuError::Unsupported("newTexture"))?;
        self.textures.insert(id.0, tex);
        Ok(())
    }
    fn destroy_texture(&mut self, id: TextureId) -> Result<()> {
        self.textures.remove(&id.0);
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
        sd.setMinFilter(filt(desc.min_filter));
        sd.setMagFilter(filt(desc.mag_filter));
        sd.setSAddressMode(addr(desc.address_u));
        sd.setTAddressMode(addr(desc.address_v));
        let s = self.device.newSamplerStateWithDescriptor(&sd).ok_or(GpuError::Unsupported("newSampler"))?;
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
            if self.shader_id_hash.get(&id.0) == Some(&key) && self.shaders.contains_key(&id.0) {
                return Ok(());
            }
            // Content-cache hit (same MSL compiled earlier under any id) → reuse the library, no compile.
            let lib = if let Some(cached) = self.shader_lib_cache.get(&key) {
                cached.clone()
            } else {
                match self.device.newLibraryWithSource_options_error(&NSString::from_str(&src), None) {
                    Ok(lib) => {
                        self.shader_compiles += 1;
                        self.shader_lib_cache.insert(key, lib.clone());
                        lib
                    }
                    Err(e) => {
                        eprintln!("dd-metal: shader {} MSL compile failed: {e:?}", id.0);
                        return Err(GpuError::Unsupported("shader compile"));
                    }
                }
            };
            self.shaders.insert(id.0, lib);
            self.shader_id_hash.insert(id.0, key);
        }
        Ok(())
    }
    fn destroy_shader(&mut self, id: ShaderId) -> Result<()> {
        self.shaders.remove(&id.0);
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
            self.pipelines.insert(id.0, Pipeline { state, primitive, depth });
            self.pipeline_id_hash.insert(id.0, key);
            return Ok(());
        }
        // Prefer the guest's own translated shaders (per module); fall back to the builtin vertex-color
        // library + its vcmain/fcmain entry points when the app didn't ship MSL.
        let vlib = self.shaders.get(&desc.vertex.module).unwrap_or(&self.lib);
        let ventry = if self.shaders.contains_key(&desc.vertex.module) { desc.vertex.entry.as_str() } else { "vcmain" };
        let vfn = vlib.newFunctionWithName(&NSString::from_str(ventry)).ok_or(GpuError::Unsupported("vertex fn"))?;
        let (flib, fentry) = match &desc.fragment {
            Some(f) if self.shaders.contains_key(&f.module) => (self.shaders.get(&f.module).unwrap(), f.entry.as_str()),
            _ => (&self.lib, "fcmain"),
        };
        let ffn = flib.newFunctionWithName(&NSString::from_str(fentry)).ok_or(GpuError::Unsupported("fragment fn"))?;
        let pdesc = MTLRenderPipelineDescriptor::new();
        pdesc.setVertexFunction(Some(&vfn));
        pdesc.setFragmentFunction(Some(&ffn));
        unsafe {
            pdesc.colorAttachments().objectAtIndexedSubscript(0).setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        }
        // Vertex layout: derive the Metal vertex descriptor from the guest's attributes. The IR carries
        // one `VertexLayout` per SOURCE VBO — glmark2/ANGLE bind a separate tightly-packed buffer per
        // attribute (position in one, normal in another) — where layout index i == the guest's vertex-
        // buffer slot i. Each attribute references its layout's Metal buffer via setBufferIndex, so a
        // secondary attribute (normal/texcoord) reads its own stream instead of aliasing slot 0. `format`
        // carries the float-component count (1/2/3/4). Vertex buffers live at VBUF_BASE.. (see const) so
        // they never collide with the uniform block at `[[buffer(1)]]`; `SetVertexBuffer` applies the same
        // base. Falls back to the builtin float2+float4 layout at VBUF_BASE.
        let vd = MTLVertexDescriptor::vertexDescriptor();
        let any_attrs = desc.vertex_buffers.iter().any(|l| !l.attrs.is_empty());
        unsafe {
            if any_attrs {
                for (li, layout) in desc.vertex_buffers.iter().enumerate() {
                    let slot = VBUF_BASE + li;
                    for a in &layout.attrs {
                        let mf = match a.format {
                            1 => MTLVertexFormat::Float,
                            2 => MTLVertexFormat::Float2,
                            3 => MTLVertexFormat::Float3,
                            4 => MTLVertexFormat::Float4,
                            _ => MTLVertexFormat::Float4,
                        };
                        let ad = vd.attributes().objectAtIndexedSubscript(a.location as usize);
                        ad.setFormat(mf);
                        ad.setOffset(a.offset as usize);
                        ad.setBufferIndex(slot);
                    }
                    vd.layouts().objectAtIndexedSubscript(slot).setStride(layout.stride.max(4) as usize);
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
                vd.layouts().objectAtIndexedSubscript(VBUF_BASE).setStride(24);
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
        self.pipeline_cache.insert(key, (state.clone(), primitive, depth));
        self.pipeline_id_hash.insert(id.0, key);
        self.pipelines.insert(id.0, Pipeline { state, primitive, depth });
        Ok(())
    }

    fn create_bind_group(&mut self, id: BindGroupId, desc: &BindGroupDesc) -> Result<()> {
        // `binding` is the MSL resource index used in both stages: a uniform block at `[[buffer(1)]]`, a
        // sampled texture at `[[texture(n)]]`, a sampler at `[[sampler(n)]]`. Record all three kinds.
        let mut binds = Vec::new();
        for e in &desc.entries {
            match e.resource {
                BindResource::Buffer { id: bid, offset, .. } => {
                    binds.push(Bind::Buffer { index: e.binding, buffer: bid, offset })
                }
                BindResource::Texture { id: tid } => binds.push(Bind::Texture { index: e.binding, texture: tid }),
                BindResource::Sampler { id: sid } => binds.push(Bind::Sampler { index: e.binding, sampler: sid }),
            }
        }
        self.bind_groups.insert(id.0, binds);
        Ok(())
    }

    fn submit(&mut self, cb: &CommandBuffer) -> Result<()> {
        let cmd = self.queue.commandBuffer().ok_or(GpuError::Unsupported("commandBuffer"))?;
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
        for op in &cb.encoder {
            match op {
                Enc::BeginRenderPass { color, depth } => {
                    let pass = unsafe { MTLRenderPassDescriptor::renderPassDescriptor() };
                    let mut cw = 0u32;
                    let mut chh = 0u32;
                    for (i, ca) in color.iter().enumerate() {
                        let tex = self.textures.get(&ca.texture).ok_or(GpuError::UnknownId { kind: "texture", id: ca.texture })?;
                        cw = tex.width() as u32;
                        chh = tex.height() as u32;
                        let att = unsafe { pass.colorAttachments().objectAtIndexedSubscript(i) };
                        att.setTexture(Some(tex));
                        att.setLoadAction(if ca.load == LoadOp::Clear { MTLLoadAction::Clear } else { MTLLoadAction::Load });
                        let c = ca.clear;
                        att.setClearColor(MTLClearColor { red: c[0] as f64, green: c[1] as f64, blue: c[2] as f64, alpha: c[3] as f64 });
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
                    enc = Some(cmd.renderCommandEncoderWithDescriptor(&pass).ok_or(GpuError::Unsupported("encoder"))?);
                }
                Enc::SetPipeline(p) => {
                    let pl = self.pipelines.get(p).ok_or(GpuError::UnknownId { kind: "pipeline", id: *p })?;
                    cur_prim = pl.primitive;
                    let pd = pl.depth;
                    if let Some(e) = &enc {
                        e.setRenderPipelineState(&pl.state);
                        if pd {
                            e.setDepthStencilState(Some(&self.depth_state));
                        }
                    }
                }
                Enc::SetVertexBuffer { slot, buffer, offset } => {
                    let buf = self.buffers.get(buffer).ok_or(GpuError::UnknownId { kind: "buffer", id: *buffer })?;
                    if let Some(e) = &enc {
                        // Bind at VBUF_BASE + slot to match the vertex descriptor's buffer indices (keeps
                        // guest vertex buffers clear of the uniform block at [[buffer(1)]]).
                        unsafe { e.setVertexBuffer_offset_atIndex(Some(buf), *offset as usize, VBUF_BASE + *slot as usize) };
                    }
                }
                Enc::Draw { vertex_count, instance_count, first_vertex, .. } => {
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
                                    Bind::Buffer { index, buffer, offset } => (0u8, *index, *buffer, *offset),
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
                                        e.setVertexBuffer_offset_atIndex(Some(buf), off as usize, idx as usize);
                                        e.setFragmentBuffer_offset_atIndex(Some(buf), off as usize, idx as usize);
                                    }
                                }
                                1 => {
                                    if let Some(tex) = self.textures.get(&id) {
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
                Enc::SetIndexBuffer { buffer, offset, format } => {
                    let it = match format {
                        IndexFormat::U16 => MTLIndexType::UInt16,
                        IndexFormat::U32 => MTLIndexType::UInt32,
                    };
                    index_buffer = Some((*buffer, *offset, it));
                }
                Enc::DrawIndexed { index_count, instance_count, first_index, .. } => {
                    if let (Some(e), Some((bid, base_off, it))) = (&enc, index_buffer) {
                        if let Some(ibuf) = self.buffers.get(&bid) {
                            let esz = if it == MTLIndexType::UInt16 { 2u64 } else { 4 };
                            let off = base_off + *first_index as u64 * esz;
                            unsafe {
                                e.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset_instanceCount(
                                    cur_prim,
                                    *index_count as usize,
                                    it,
                                    ibuf,
                                    off as usize,
                                    (*instance_count).max(1) as usize,
                                )
                            };
                        }
                    }
                }
                Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, width, height, .. } => {
                    // Upload a staging buffer's pixels into a sampled texture (glTexImage2D path). Runs on a
                    // standalone blit encoder between passes; a render encoder must not be open here.
                    if let (Some(buf), Some(tex)) = (self.buffers.get(src), self.textures.get(dst)) {
                        // Untrusted IR: the staging buffer must hold src_offset + bytes_per_row*height, else
                        // Metal reads OOB. Verify before encoding the copy (skip on overflow/short buffer).
                        let need = (*bytes_per_row as usize)
                            .checked_mul(*height as usize)
                            .and_then(|n| n.checked_add(*src_offset as usize));
                        if need.map_or(true, |n| n > buf.length()) {
                            eprintln!("[dd-display/metal_backend] CopyBufferToTexture OOB: src_off={} bpr={} h={} buf_len={} — skipped",
                                src_offset, bytes_per_row, height, buf.length());
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
                Enc::EndRenderPass => {
                    if let Some(e) = enc.take() {
                        e.endEncoding();
                    }
                }
                _ => {} // SetViewport/compute: not in this slice
            }
        }
        if let Some(e) = enc.take() {
            e.endEncoding();
        }
        if let Some((render_ev, _present_ev, gen, _wait)) = &fence {
            // fence a: signal render-complete for this gen so the compositor's blit (which WAITs on this
            // event) never samples the surface mid-render.
            cmd.encodeSignalEvent_value(render_ev, *gen);
            cmd.commit();
            // Async: DO NOT wait for GPU completion — ack immediately (in run_executor) so the guest builds
            // the next frame while this one renders. The event fence, not a CPU stall, guarantees ordering.
            self.gpu_wait_ns = 0;
        } else {
            cmd.commit();
            // Baseline (DD_RENDER_NOASYNC): CPU stalls on GPU completion — the only hop that overlaps nothing.
            let t = std::time::Instant::now();
            unsafe { cmd.waitUntilCompleted() };
            self.gpu_wait_ns = t.elapsed().as_nanos() as u64;
        }
        Ok(())
    }
}
