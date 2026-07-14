//! Vulkan-style graphics on the REAL Metal GPU through wgpu: a SPIR-V colored triangle.
//!
//! The milestone this proves: a **SPIR-V** vertex+fragment pipeline (the shader format a Vulkan app
//! hands the driver) rasterizes a colored triangle on a live Metal device via the wgpu backend, and the
//! rendered pixels contain the triangle. It drives the backend's raw-SPIR-V *App* pipeline path:
//!   `SPIR-V words → CreateShader{spirv} → naga::front::spv → WGSL → naga → MSL → wgpu::RenderPipeline
//!    → BeginRenderPass/Draw → render target → read pixels`.
//! This mirrors `examples/verify_ir.rs` (which exercises the builtin FLAT/TEX fallbacks) but with real,
//! guest-supplied SPIR-V modules — the exact host seam the guest `hl-shim-vk` will target. Vulkan hands
//! one SPIR-V module per stage, so the vertex and fragment shaders are two distinct `CreateShader`s.
//!
//! SPIR-V is minted host-side from GLSL 450 (Vulkan dialect) via naga's GLSL front + SPIR-V back — the
//! same GLSL→SPIR-V step glslang performs — with a WGSL fallback. Needs a Metal device, so macOS only.
//! Run: `cargo test -p hl-gpu-wgpu --test spirv_triangle`.
//!
//! Semantics grounded in MoltenVK (the canonical Vulkan-over-Metal driver), not guessed:
//!   * SPIR-V→MSL — `MoltenVKShaderConverter/SPIRVToMSLConverter.cpp`: entry point picked by (name, stage)
//!     via SPIRV-Cross `set_entry_point` (l.282), MTLFunction looked up by that name (l.543) — hence one
//!     SPIR-V module per stage, each entry "main", as a Vulkan app supplies them.
//!   * VkPipeline → MTLRenderPipelineState — `GPUObjects/MVKPipeline.mm::newMTLRenderPipelineDescriptor`
//!     (l.1170) builds an `MTLRenderPipelineDescriptor` with a `vertexDescriptor` from the vertex-input
//!     state (`addVertexInputToPipeline`, l.1193), vertex+fragment functions (l.1493/1651), color
//!     attachments (l.1961) and `inputPrimitiveTopology` (l.1946). Our `create_render_pipeline` builds the
//!     wgpu `RenderPipelineDescriptor` with the same pieces: vertex-buffer layouts, vertex/fragment
//!     entries, color targets and primitive topology.
//!   * VkRenderPass → MTLRenderPassDescriptor — `GPUObjects/MVKRenderPass.mm`: loadAction
//!     Clear/Load (l.808-817), storeAction Store/DontCare (l.891-919), clearColor (l.204). Our
//!     `Enc::BeginRenderPass` `ColorAttachment{load: Clear, clear, store: true}` → wgpu load `Clear(color)`
//!     / store `Store`.
//!   * vkCmdDraw → drawPrimitives — `Commands/MVKCmdDraw.mm` (l.332): `drawPrimitives: primitiveType
//!     vertexStart: firstVertex vertexCount: vertexCount`. Our `Enc::Draw{vertex_count:3, first_vertex:0}` →
//!     wgpu `render_pass.draw`, topology → Metal primitive type.

#![cfg(target_os = "macos")]

use hl_gpu::ir::*;
use hl_gpu_wgpu::WgpuBackend;

fn module_to_spirv(module: &naga::Module) -> Result<Vec<u32>, String> {
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("validate: {e:?}"))?;
    naga::back::spv::write_vec(module, &info, &naga::back::spv::Options::default(), None)
        .map_err(|e| format!("spv-out: {e:?}"))
}

fn glsl_to_spirv(src: &str, stage: naga::ShaderStage) -> Result<Vec<u32>, String> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(&naga::front::glsl::Options::from(stage), src)
        .map_err(|e| format!("glsl-in: {e:?}"))?;
    module_to_spirv(&module)
}

fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("wgsl parse");
    module_to_spirv(&module).expect("wgsl→spv")
}

// One SPIR-V module per stage (the Vulkan shape). Vertex reads clip-space pos + color from the vertex
// buffer (locations 0/1) and passes color through; fragment writes it. Clip-space coords keep the test
// self-contained (no rt_adjust uniform), so the only moving part is the SPIR-V pipeline itself.
const VS_GLSL: &str = "\
#version 450
layout(location = 0) in vec2 pos;
layout(location = 1) in vec4 col;
layout(location = 0) out vec4 vcol;
void main() {
    gl_Position = vec4(pos, 0.0, 1.0);
    vcol = col;
}
";
const FS_GLSL: &str = "\
#version 450
layout(location = 0) in vec4 vcol;
layout(location = 0) out vec4 frag;
void main() { frag = vcol; }
";
const VS_WGSL: &str = "\
struct VOut { @builtin(position) pos: vec4<f32>, @location(0) col: vec4<f32> };
@vertex fn main(@location(0) pos: vec2<f32>, @location(1) col: vec4<f32>) -> VOut {
    var o: VOut; o.pos = vec4<f32>(pos, 0.0, 1.0); o.col = col; return o;
}
";
const FS_WGSL: &str = "\
@fragment fn main(@location(0) col: vec4<f32>) -> @location(0) vec4<f32> { return col; }
";

fn stage_spirv(glsl: &str, wgsl: &str, stage: naga::ShaderStage, name: &str) -> Vec<u32> {
    match glsl_to_spirv(glsl, stage) {
        Ok(w) => {
            eprintln!("spirv_triangle: {name} SPIR-V minted from GLSL 450 (Vulkan path)");
            w
        }
        Err(e) => {
            eprintln!("spirv_triangle: GLSL front end unavailable for {name} ({e}); using WGSL-sourced SPIR-V");
            wgsl_to_spirv(wgsl)
        }
    }
}

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn spirv_triangle_renders_on_real_metal() {
    let vs = stage_spirv(VS_GLSL, VS_WGSL, naga::ShaderStage::Vertex, "vertex");
    let fs = stage_spirv(FS_GLSL, FS_WGSL, naga::ShaderStage::Fragment, "fragment");
    assert_eq!(vs.first().copied(), Some(0x0723_0203), "vertex payload is real SPIR-V");
    assert_eq!(fs.first().copied(), Some(0x0723_0203), "fragment payload is real SPIR-V");

    let mut be = WgpuBackend::new().expect("wgpu Metal backend");
    let tex = be.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    be.set_render_target(1, tex, TextureFormat::Rgba8Unorm);

    // A green triangle centered in clip space over a dark-gray clear. Big enough that the center pixel
    // is solidly inside and a healthy count of pixels turn green.
    let mut verts = Vec::new();
    let tri: [([f32; 2], [f32; 4]); 3] = [
        ([0.0, 0.6], [0.0, 1.0, 0.0, 1.0]),
        ([-0.6, -0.6], [0.0, 1.0, 0.0, 1.0]),
        ([0.6, -0.6], [0.0, 1.0, 0.0, 1.0]),
    ];
    for (p, c) in tri {
        for v in p {
            verts.extend_from_slice(&v.to_le_bytes());
        }
        for v in c {
            verts.extend_from_slice(&v.to_le_bytes());
        }
    }

    let cmds = vec![
        Cmd::CreateBuffer(10, BufferDesc { size: verts.len() as u64, usage: buffer_usage::VERTEX, label: "v".into() }),
        Cmd::WriteBuffer { id: 10, offset: 0, data: verts },
        Cmd::CreateShader { id: 20, kind: hl_gpu::ir::ShaderPayloadKind::SpirV, spirv: vs },
        Cmd::CreateShader { id: 21, kind: hl_gpu::ir::ShaderPayloadKind::SpirV, spirv: fs },
        Cmd::CreateRenderPipeline(30, RenderPipelineDesc {
            vertex: ShaderRef { module: 20, entry: "main".into() },
            fragment: Some(ShaderRef { module: 21, entry: "main".into() }),
            vertex_buffers: vec![VertexLayout {
                stride: 24,
                step_mode: 0,
                attrs: vec![
                    VertexAttr { location: 0, format: 2, offset: 0 }, // float2 pos
                    VertexAttr { location: 1, format: 4, offset: 8 }, // float4 color
                ],
            }],
            color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xf }],
            depth: None,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            label: "spirv-tri".into(),
        }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.1, 0.1, 0.1, 1.0], store: true }],
                    depth: None,
                },
                Enc::SetPipeline(30),
                Enc::SetViewport { x: 0.0, y: 0.0, w: W as f32, h: H as f32, min_depth: 0.0, max_depth: 1.0 },
                Enc::SetScissor { x: 0, y: 0, w: W, h: H },
                Enc::SetVertexBuffer { slot: 0, buffer: 10, offset: 0 },
                Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ];
    let ir = encode_stream(&cmds);
    hl_gpu::replay::replay_stream(&mut be, &ir).expect("replay spirv triangle");

    let data = be.read_target(1).expect("readback");
    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    };
    let center = px(32, 32);
    let corner = px(2, 2);
    // Count green (triangle) vs gray (clear) pixels to prove the triangle actually rasterized.
    let mut green = 0u32;
    let mut gray = 0u32;
    for i in (0..data.len()).step_by(4) {
        let p = [data[i], data[i + 1], data[i + 2]];
        if p[0] < 40 && p[1] > 200 && p[2] < 40 {
            green += 1;
        }
        if (20..=40).contains(&p[0]) && (20..=40).contains(&p[1]) && (20..=40).contains(&p[2]) {
            gray += 1;
        }
    }
    eprintln!("[spirv-tri] center(32,32)={center:?} corner(2,2)={corner:?} green_px={green} gray_px={gray}");
    let center_green = center[0] < 40 && center[1] > 200 && center[2] < 40;
    let corner_gray = (20..=40).contains(&corner[0]) && (20..=40).contains(&corner[1]);
    assert!(center_green, "center pixel should be the green triangle, got {center:?}");
    assert!(corner_gray, "corner pixel should be the gray clear, got {corner:?}");
    assert!(green > 200, "expected a substantial green triangle, got {green} px");
    assert!(gray > 200, "expected the clear background to remain, got {gray} px");
    eprintln!("spirv_triangle_renders_on_real_metal: OK (green_px={green}, gray_px={gray})");
}
