//! ANGLE-style GLSL-ES corpus battery — the EXECUTOR half of the Chrome shader wall.
//!
//! Chrome (via ANGLE) and GTK4 GskGpu forward *GLSL-ES* vertex/fragment shaders that naga-24's `glsl-in`
//! rejects wholesale. `crate::glsl_es::normalize` (ES->460, `gl_VertexID`->`gl_VertexIndex`, combined-sampler
//! split, matrix/array IO reshaping, switch lowering) makes them naga-acceptable before parse; this battery
//! proves it against a LARGE, diverse corpus of the constructs ANGLE actually emits.
//!
//! Each shader is pushed through the REAL executor path -- `Cmd::CreateShader { kind: Glsl }` ->
//! `glsl_to_wgsl_reflect` (ES-normalize -> naga glsl-in -> validate -> wgsl-out) -> `create_shader_module`
//! on the device. A shader that survives that reaches a VALID wgpu shader module (the DoD).
//!
//! Every corpus entry declares its `Expect`: `Pass` (must reach a module) or `NagaLimit` (a construct
//! naga-24 genuinely cannot model with any reasonable textual normalization -- asserted to STILL fail, with
//! the exact reason logged, so the limit is on the record and never faked into a green).

mod common;
use common::*;

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

static EXEC: OnceLock<Option<Mutex<WgpuExecutor>>> = OnceLock::new();

fn exec() -> Option<MutexGuard<'static, WgpuExecutor>> {
    EXEC.get_or_init(|| WgpuExecutor::new(DeviceConfig::default()).ok().map(Mutex::new))
        .as_ref()
        .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()))
}

fn new_sess(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)))
}

/// Compile ONE GLSL-ES stage through the full executor shader path. `Ok` => it reached a valid wgpu shader
/// module. `Err(msg)` carries the exact glsl-in/validate/wgsl-out diagnostic.
fn compile(exec: &mut WgpuExecutor, stage: u32, entry: &str, src: &str) -> Result<(), String> {
    let mut s = new_sess(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(stage, entry, src) }],
    )
    .map(|_| ())
    .map_err(|e| format!("{e:?}"))
}

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Pass,
    NagaLimit(&'static str),
}
use Expect::*;

struct Case {
    name: &'static str,
    stage: u32,
    entry: &'static str,
    src: &'static str,
    expect: Expect,
}

const fn vs(name: &'static str, src: &'static str, expect: Expect) -> Case {
    Case { name, stage: glsl_stage::VERTEX, entry: "vmain", src, expect }
}
const fn fs(name: &'static str, src: &'static str, expect: Expect) -> Case {
    Case { name, stage: glsl_stage::FRAGMENT, entry: "fmain", src, expect }
}

include!("common/glsl_es_corpus_data.rs");

#[test]
fn angle_glsl_es_corpus_reaches_valid_modules() {
    let mut guard = match exec() {
        Some(g) => g,
        None => {
            eprintln!("glsl_es_corpus: no wgpu adapter -- skipping (headless without lavapipe)");
            return;
        }
    };
    let exec = &mut *guard;

    let mut passed = 0usize;
    let mut limits: Vec<(&str, &str, String)> = Vec::new();
    let mut unexpected_fail: Vec<(&str, String)> = Vec::new();
    let mut unexpected_pass: Vec<&str> = Vec::new();

    for c in CORPUS {
        match (c.expect, compile(exec, c.stage, c.entry, c.src)) {
            (Pass, Ok(())) => passed += 1,
            (Pass, Err(e)) => unexpected_fail.push((c.name, e)),
            (NagaLimit(reason), Err(e)) => limits.push((c.name, reason, e)),
            (NagaLimit(_), Ok(())) => unexpected_pass.push(c.name),
        }
    }

    let n = CORPUS.len();
    eprintln!("\n=== ANGLE GLSL-ES corpus: {passed}/{n} reached a valid wgpu shader module ===");
    if !limits.is_empty() {
        eprintln!("\n--- documented naga-24 limits ({}) -- skipped (still-fail, on the record) ---", limits.len());
        for (name, reason, err) in &limits {
            eprintln!("  [naga-limit] {name}: {reason}\n               error: {err}");
        }
    }
    if !unexpected_pass.is_empty() {
        eprintln!("\n--- entries marked NagaLimit that now PASS (reclassify to Pass) ---");
        for name in &unexpected_pass { eprintln!("  {name}"); }
    }
    if !unexpected_fail.is_empty() {
        eprintln!("\n--- UNEXPECTED failures (normalization gaps to fix) ---");
        for (name, err) in &unexpected_fail { eprintln!("  {name}: {err}"); }
    }

    assert!(unexpected_fail.is_empty(), "{} corpus shader(s) failed to reach a valid module (see log)", unexpected_fail.len());
    assert!(unexpected_pass.is_empty(), "{} shader(s) marked NagaLimit unexpectedly PASSED -- reclassify to Pass", unexpected_pass.len());
}

// ===================================================================================================
// Real renders: a subset of the corpus is not just compiled but DRAWN, and an EXACT pixel is read back
// off the device -- proving the ES-normalized shaders execute, not merely translate. Each uses an ES
// `gl_VertexID` fullscreen-triangle vertex shader (itself exercising the ES vertex-index builtin path).
// ===================================================================================================

const W: u32 = 8;
const H: u32 = 8;

// A fullscreen triangle from `gl_VertexID` (no attributes) -- ANGLE's standard clear/blit vertex shape and
// an exercise of the ES vertex-index lowering.
const DRAW_VS: &str = r#"#version 300 es
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexID], 0.0, 1.0);
}
"#;

fn rt(w: u32, h: u32) -> TextureDesc {
    TextureDesc {
        width: w, height: h, depth: 1, mip_levels: 1, sample_count: 1,
        dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC, label: String::new(),
    }
}
fn ct() -> ColorTargetState {
    ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }
}

/// Draw `DRAW_VS` + `fs` over an 8x8 target (no bindings) and return the readback.
fn draw_plain(exec: &mut WgpuExecutor, fs_src: &str) -> Vec<u8> {
    let mut s = new_sess(exec);
    hl_gpu::runtime::submit(&mut s, exec, 0, &[
        Cmd::CreateTexture(1, rt(W, H)),
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", DRAW_VS) },
        Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs_src) },
        Cmd::CreateRenderPipeline(1, RenderPipelineDesc {
            vertex: ShaderRef { module: 1, entry: "vmain".into() },
            fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
            vertex_buffers: vec![], color_targets: vec![ct()], depth: None,
            topology: Topology::TriangleList, cull: 0, front_face: 0, sample_count: 1, label: String::new(),
        }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ]).expect("plain ES draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

fn approx(a: [u8; 4], b: [u8; 4], tol: i16) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= tol)
}

#[test]
fn es_vertexid_plus_const_fragment_renders_exact_pixel() {
    let mut guard = match exec() { Some(g) => g, None => return };
    let exec = &mut *guard;
    // 0.2,0.4,0.6 -> 51,102,153 (round(x*255)).
    let fs = r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() { o = vec4(0.2, 0.4, 0.6, 1.0); }
"#;
    let px = draw_plain(exec, fs);
    for chunk in px.chunks_exact(4) {
        let p = [chunk[0], chunk[1], chunk[2], chunk[3]];
        assert!(approx(p, [51, 102, 153, 255], 1), "ES gl_VertexID triangle + const fragment must fill {:?}, got {p:?}", [51, 102, 153, 255]);
    }
}

#[test]
fn es_math_builtins_fragment_renders_exact_pixel() {
    let mut guard = match exec() { Some(g) => g, None => return };
    let exec = &mut *guard;
    // clamp(mix(0,1,0.5)) = 0.5 -> 128 ; smoothstep(0,1,0.5) = 0.5 -> 128 ; abs(-0.25) = 0.25 -> 64.
    let fs = r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() {
    float a = clamp(mix(0.0, 1.0, 0.5), 0.0, 1.0);
    float c = smoothstep(0.0, 1.0, 0.5);
    float d = abs(-0.25);
    o = vec4(a, c, d, 1.0);
}
"#;
    let px = draw_plain(exec, fs);
    for chunk in px.chunks_exact(4) {
        let p = [chunk[0], chunk[1], chunk[2], chunk[3]];
        assert!(approx(p, [128, 128, 64, 255], 2), "ES math-builtin fragment must fill ~[128,128,64,255], got {p:?}");
    }
}

#[test]
fn es_combined_sampler_fragment_samples_exact_texel() {
    let mut guard = match exec() { Some(g) => g, None => return };
    let exec = &mut *guard;
    // A GLSL-ES `uniform sampler2D` sampled at center -- exercises the combined->separate split END TO END:
    // the split lands the texture at binding 1 and the sampler at binding 2 (glsl_es scheme 1+2k / 2+2k).
    let texel: [u8; 4] = [40, 160, 210, 255];
    let fs = r#"#version 300 es
precision highp float;
uniform sampler2D uTex;
layout(location = 0) out vec4 o;
void main() { o = texture(uTex, vec2(0.5, 0.5)); }
"#;
    let mut s = new_sess(exec);
    hl_gpu::runtime::submit(&mut s, exec, 0, &[
        Cmd::CreateTexture(1, rt(W, H)),
        Cmd::CreateTexture(2, TextureDesc {
            width: 1, height: 1, depth: 1, mip_levels: 1, sample_count: 1, dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm, usage: texture_usage::SAMPLED | texture_usage::COPY_DST, label: String::new(),
        }),
        Cmd::CreateBuffer(1, BufferDesc { size: 4, usage: buffer_usage::COPY_SRC, label: String::new() }),
        Cmd::WriteBuffer { id: 1, offset: 0, data: texel.to_vec() },
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", DRAW_VS) },
        Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs) },
        Cmd::CreateSampler(1, SamplerDesc {
            min_filter: Filter::Nearest, mag_filter: Filter::Nearest, mip_filter: Filter::Nearest,
            address_u: AddressMode::ClampToEdge, address_v: AddressMode::ClampToEdge, address_w: AddressMode::ClampToEdge,
        }),
        Cmd::CreateRenderPipeline(1, RenderPipelineDesc {
            vertex: ShaderRef { module: 1, entry: "vmain".into() },
            fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
            vertex_buffers: vec![], color_targets: vec![ct()], depth: None,
            topology: Topology::TriangleList, cull: 0, front_face: 0, sample_count: 1, label: String::new(),
        }),
        // The ES combined-sampler split: texture -> binding 1, sampler -> binding 2.
        Cmd::CreateBindGroup(1, BindGroupDesc {
            set: 0,
            entries: vec![
                BindEntry { binding: 1, resource: BindResource::Texture { id: 2 } },
                BindEntry { binding: 2, resource: BindResource::Sampler { id: 1 } },
            ],
        }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::CopyBufferToTexture { src: 1, src_offset: 0, bytes_per_row: 4, dst: 2, mip: 0, width: 1, height: 1 },
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [1.0, 0.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ]).expect("ES combined-sampler draw must run cleanly");
    let px = exec.read_texture(&s.resources, 1).unwrap();
    for chunk in px.chunks_exact(4) {
        let p = [chunk[0], chunk[1], chunk[2], chunk[3]];
        assert!(approx(p, texel, 1), "ES combined-sampler fragment must sample {texel:?} end-to-end, got {p:?}");
    }
}

// ---------------------------------------------------------------------------------------------------
// mat2-in-std140-UBO: an EXACT-PIXEL geometry proof that the reconstructed matrix multiplies the REAL
// std140 bytes correctly (not just that it compiles). A unit quad synthesized from gl_VertexID is
// transformed by a `mat2` in a std140 uniform block; the covered pixels must land EXACTLY where the mat2
// maps the quad. A wrong std140 column offset (the padding bug the `vec4 col[N]` rewrite exists to avoid) or
// a transposed/collapsed reconstruction would move or reshape the block and fail the exact check.
// ---------------------------------------------------------------------------------------------------

// std140 stores each 2-row matrix column in its own 16-byte (vec4) slot — only `.xy` carries data. This is
// EXACTLY the byte image `split_std140_mat2`'s `vec4 col[N]` rewrite reads back, so the uploaded bytes are
// the app's real UBO contents with no re-pack.
fn mat2_std140_bytes(col0: [f32; 2], col1: [f32; 2]) -> Vec<u8> {
    [col0[0], col0[1], 0.0, 0.0, col1[0], col1[1], 0.0, 0.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

// Unit quad in LOCAL space [0,1]² from gl_VertexID (a 4-vertex triangle strip), transformed by a `mat2` in a
// std140 UBO. GLSL-ES (`gl_VertexID`) so it takes the normalize path; the mat2 member is the construct under
// test. `x.m2 * local` = col0*local.x + col1*local.y, so a swapped/mis-padded column visibly mis-maps.
const MAT2_VS: &str = r#"#version 300 es
precision highp float;
layout(std140, binding = 0) uniform Xf { mat2 m2; } x;
void main() {
    vec2 local = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
    gl_Position = vec4(x.m2 * local, 0.0, 1.0);
}
"#;

const MAT2_FILL_FS: &str = r#"#version 300 es
precision highp float;
layout(location = 0) out vec4 o;
void main() { o = vec4(0.2, 0.8, 0.4, 1.0); }
"#;

/// Draw the unit quad transformed by `mat2(col0, col1)` (std140 UBO at binding 0) over an 8×8 target.
fn draw_mat2_quad(exec: &mut WgpuExecutor, col0: [f32; 2], col1: [f32; 2]) -> Vec<u8> {
    let mut s = new_sess(exec);
    hl_gpu::runtime::submit(&mut s, exec, 0, &[
        Cmd::CreateTexture(1, rt(W, H)),
        Cmd::CreateBuffer(1, BufferDesc { size: 32, usage: buffer_usage::UNIFORM | buffer_usage::COPY_DST, label: String::new() }),
        Cmd::WriteBuffer { id: 1, offset: 0, data: mat2_std140_bytes(col0, col1) },
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", MAT2_VS) },
        Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", MAT2_FILL_FS) },
        Cmd::CreateRenderPipeline(1, RenderPipelineDesc {
            vertex: ShaderRef { module: 1, entry: "vmain".into() },
            fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
            vertex_buffers: vec![], color_targets: vec![ct()], depth: None,
            topology: Topology::TriangleStrip, cull: 0, front_face: 0, sample_count: 1, label: String::new(),
        }),
        Cmd::CreateBindGroup(1, BindGroupDesc {
            set: 0,
            entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 32 } }],
        }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::Draw { vertex_count: 4, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ]).expect("mat2 std140 UBO quad draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

/// The EXACT covered-pixel mask (8×8, readback row order) for a quad occupying the wgpu NDC rect
/// `x∈[nx0,nx1]`, `y∈[ny0,ny1]` (y-up), by the standard center-sample rule. Edges are integer, centers are
/// half-integers, so coverage is unambiguous.
fn ndc_rect_mask(nx0: f32, nx1: f32, ny0: f32, ny1: f32) -> Vec<bool> {
    let fx = |n: f32| (n + 1.0) / 2.0 * W as f32;
    let fy = |n: f32| (1.0 - n) / 2.0 * H as f32; // y-up NDC → y-down framebuffer row
    let (x0, x1) = (fx(nx0.min(nx1)), fx(nx0.max(nx1)));
    let (y0, y1) = (fy(ny0.max(ny1)), fy(ny0.min(ny1)));
    let mut m = vec![false; (W * H) as usize];
    for py in 0..H {
        for px in 0..W {
            let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
            m[(py * W + px) as usize] = cx > x0 && cx < x1 && cy > y0 && cy < y1;
        }
    }
    m
}

fn assert_mat2_coverage(label: &str, px: &[u8], mask: &[bool]) {
    let fill = [51u8, 204, 102, 255]; // vec4(0.2,0.8,0.4,1.0) * 255
    let clear = [0u8, 0, 0, 255];
    for i in 0..(W * H) as usize {
        let p = [px[i * 4], px[i * 4 + 1], px[i * 4 + 2], px[i * 4 + 3]];
        let want = if mask[i] { fill } else { clear };
        assert!(
            approx(p, want, 2),
            "{label}: pixel ({},{}) got {p:?} want {want:?} (mat2 must map the quad EXACTLY)",
            i as u32 % W, i as u32 / W,
        );
    }
}

#[test]
fn es_mat2_std140_ubo_transforms_quad_to_exact_pixels() {
    let mut guard = match exec() { Some(g) => g, None => return };
    let exec = &mut *guard;

    // Case 1 — 90° CCW rotation about origin: (x,y) → (-y,x). Column-major mat2*v = col0*v.x + col1*v.y, so
    // col0=(0,1), col1=(-1,0). The unit quad [0,1]² maps to NDC x∈[-1,0], y∈[0,1] = the TOP-LEFT quadrant.
    // Off-diagonal terms only — a transpose or a mis-padded second column would land it elsewhere.
    let px = draw_mat2_quad(exec, [0.0, 1.0], [-1.0, 0.0]);
    assert_mat2_coverage("rotate90", &px, &ndc_rect_mask(-1.0, 0.0, 0.0, 1.0));

    // Case 2 — non-uniform scale diag(0.5, 0.75): col0=(0.5,0), col1=(0,0.75). The quad maps to NDC
    // x∈[0,0.5], y∈[0,0.75]. Diagonal terms — proves col0.x and col1.y are read at the right std140 offsets.
    let px = draw_mat2_quad(exec, [0.5, 0.0], [0.0, 0.75]);
    assert_mat2_coverage("scale", &px, &ndc_rect_mask(0.0, 0.5, 0.0, 0.75));
}
