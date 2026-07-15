//! INTEGRATION DEMO — feature_combo_frame: depth test + stencil gating + blend + MRT combined into ONE
//! render frame, exact-pixel asserted in BOTH color attachments.
//!
//! The isolated per-feature demos each proved one thing alone: stencil gates a draw (#164/wgpu stencil),
//! depth occludes near-over-far (#168/vk_depth), blend composites alpha-over (#161/gl_blend), MRT fans a
//! fragment to two attachments (#170/gl_mrt). This demo mints ONE frame that makes all four INTERACT, so a
//! bug that only appears when the features combine (a stencil mask that leaks past a blended draw, a depth
//! test that drops the near fragment before it can composite, an MRT second attachment that doesn't get its
//! own blend/depth/stencil state) is caught here where the isolated demos pass.
//!
//! The frame, on two `Rgba8Unorm` color targets (MRT) + one `Depth24PlusStencil8` depth/stencil target, in a
//! SINGLE render pass:
//!   1. MARK  — a centered quad writes stencil=1 under the middle 16x16 of the 32x32 frame (compare ALWAYS,
//!              pass REPLACE, ref 1); color writes masked off. This is the stencil gate.
//!   2. BG    — a fullscreen OPAQUE draw (blend off), depth-tested LESS at z=0.5, stencil-tested EQUAL 1.
//!              Only the marked rect passes the stencil test → the rect gets the opaque background, writing
//!              depth 0.5 there; everything outside the rect stays the black clear (stencil gated it out).
//!   3. FG    — a fullscreen TRANSLUCENT draw (src-alpha-over, a=0.5), depth LESS at z=0.3 (NEARER than BG),
//!              stencil EQUAL 1. In the rect it passes depth (0.3<0.5) and COMPOSITES over BG; the two MRT
//!              outputs carry DIFFERENT colors so each attachment composites its own pair.
//!   4. FAR   — a fullscreen TRANSLUCENT red draw at z=0.9 (FARTHER than FG's 0.3), stencil EQUAL 1. The
//!              depth test (0.9<0.3 false) OCCLUDES it, so red never composites — proving depth gates the
//!              blended draw. If depth were dropped, red would tint the rect and the exact assert would fail.
//!
//! Exact result: the marked rect holds, in attachment 0, the exact alpha composite of BG_a and FG_a, and in
//! attachment 1 the DIFFERENT exact composite of BG_b and FG_b; everywhere outside the rect is the black
//! clear in both attachments; and no red survives anywhere. Two PNGs are written to /tmp/hl-demo/ for a human
//! confrontation of the combined frame.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment, ColorTargetState,
    DepthAttachment, DepthState, RenderPipelineDesc, ShaderRef, StencilFaceState, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, stencil_op, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

mod common;
use common::{demo_png_dir, write_png};

const N: u32 = 32;

// GL/protocol blend-factor + op wire numbering (matches hl_wip-gl `blend_factor_wire`/`blend_op_wire`,
// the same constants the wgpu executor maps — see tests/gl_blend.rs and the wgpu blend demo).
const SRC_ALPHA: u32 = 4;
const ONE_MINUS_SRC_ALPHA: u32 = 5;
const OP_ADD: u32 = 0;

// The two attachments' background + foreground colors. Channels chosen so the a=0.5 straight-alpha
// composite (bg+fg)/2 is an EXACT 8-bit integer, and so the two attachments' composites DIFFER (MRT proof).
const BG_A: [u8; 3] = [200, 100, 40];
const FG_A: [u8; 3] = [40, 180, 220];
const BG_B: [u8; 3] = [60, 220, 120];
const FG_B: [u8; 3] = [180, 60, 200];

/// Straight-alpha "source over" on all four channels: `src*srcAlpha + dst*(1-srcAlpha)`.
fn src_alpha_over() -> BlendState {
    BlendState {
        src_color: SRC_ALPHA,
        dst_color: ONE_MINUS_SRC_ALPHA,
        op_color: OP_ADD,
        src_alpha: SRC_ALPHA,
        dst_alpha: ONE_MINUS_SRC_ALPHA,
        op_alpha: OP_ADD,
    }
}

/// The a=0.5 composite of an opaque bg under a 50%-alpha fg: per-channel average; alpha = 0.5*0.5+1.0*0.5.
fn composite(bg: [u8; 3], fg: [u8; 3]) -> [u8; 4] {
    let avg = |b: u8, f: u8| ((b as u16 + f as u16) / 2) as u8;
    [avg(bg[0], fg[0]), avg(bg[1], fg[1]), avg(bg[2], fg[2]), 191]
}

/// The centered rect a `[-0.5,0.5]^2` NDC quad rasterizes to on an NxN target: pixel-center column `c` at
/// NDC `(c+0.5)/N*2-1` lies in `(-0.5,0.5)` exactly for `c` in `[N/4, 3N/4)`. For N=32 that is `[8,24)`.
fn inside_rect(x: u32, y: u32) -> bool {
    (N / 4..3 * N / 4).contains(&x) && (N / 4..3 * N / 4).contains(&y)
}

fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("seed wgsl parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed wgsl validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit spir-v")
}

// MARK: a centered quad ([-0.5,0.5]^2, two triangles) that writes ONLY the stencil buffer — its two color
// outputs are masked off on the pipeline, so its fragment value is irrelevant.
const MARK_WGSL: &str = r#"
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 6>(
        vec2<f32>(-0.5, -0.5), vec2<f32>( 0.5, -0.5), vec2<f32>(-0.5,  0.5),
        vec2<f32>( 0.5, -0.5), vec2<f32>( 0.5,  0.5), vec2<f32>(-0.5,  0.5));
    return vec4<f32>(p[vi], 0.0, 1.0);
}
struct FOut { @location(0) o0: vec4<f32>, @location(1) o1: vec4<f32> };
@fragment
fn fs_main() -> FOut { return FOut(vec4<f32>(1.0), vec4<f32>(1.0)); }
"#;

// DRAW: a fullscreen triangle whose clip-space z comes from the uniform (so ONE shader draws at any depth),
// with two color outputs routed from two distinct uniform members — the MRT fan-out is data-driven, and the
// depth/stencil/blend that gate it are pure pipeline/pass state.
const DRAW_WGSL: &str = r#"
struct U { c0: vec4<f32>, c1: vec4<f32>, params: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(p[vi], u.params.x, 1.0);
}
struct FOut { @location(0) o0: vec4<f32>, @location(1) o1: vec4<f32> };
@fragment
fn fs_main() -> FOut { return FOut(u.c0, u.c1); }
"#;

const DS_FMT: TextureFormat = TextureFormat::Depth24PlusStencil8;

fn color_tex() -> TextureDesc {
    TextureDesc {
        width: N,
        height: N,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
    }
}

fn ds_tex() -> TextureDesc {
    TextureDesc {
        width: N,
        height: N,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: DS_FMT,
        usage: texture_usage::RENDER_TARGET,
        label: String::new(),
    }
}

/// A uniform payload `[c0(4), c1(4), params(4)]` (48 bytes) — the two MRT colors + z in `params.x`.
fn uniform(c0: [u8; 3], a0: f32, c1: [u8; 3], a1: f32, z: f32) -> Vec<u8> {
    let f = |c: [u8; 3], a: f32| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, a];
    let mut v = Vec::new();
    for x in f(c0, a0) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for x in f(c1, a1) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for x in [z, 0.0, 0.0, 0.0] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v
}

fn stencil_face(cmp: u32, pass: u32) -> StencilFaceState {
    StencilFaceState { compare: cmp, fail_op: stencil_op::KEEP, depth_fail_op: stencil_op::KEEP, pass_op: pass }
}

/// A pipeline with TWO color targets (MRT), a depth/stencil state, and an optional blend on both targets.
fn pipeline(shader: u32, blend: Option<BlendState>, write_mask: u32, depth: DepthState) -> RenderPipelineDesc {
    let target = ColorTargetState { format: TextureFormat::Rgba8Unorm, blend, write_mask };
    RenderPipelineDesc {
        vertex: ShaderRef { module: shader, entry: "vs_main".into() },
        fragment: Some(ShaderRef { module: shader, entry: "fs_main".into() }),
        vertex_buffers: vec![],
        color_targets: vec![target.clone(), target],
        depth: Some(depth),
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        sample_count: 1,
        label: String::new(),
    }
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * N + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= 1)
}

/// Upscale an NxN RGBA plane by `s` for a legible PNG.
fn upscale(rgba: &[u8], s: u32) -> Vec<u8> {
    let (w, h) = (N * s, N * s);
    let mut up = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let src = (((y / s) * N + (x / s)) * 4) as usize;
            let dst = ((y * w + x) * 4) as usize;
            up[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
        }
    }
    up
}

#[test]
fn depth_stencil_blend_mrt_combine_in_one_frame() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("SKIP: no wgpu adapter (lavapipe/Vulkan ICD unreachable): {e}");
            return;
        }
    };
    let adapter = exec.adapter_name().to_lowercase();
    assert!(
        adapter.contains("llvmpipe") || adapter.contains("lavapipe"),
        "must rasterize on the software Vulkan device, got {:?}",
        exec.adapter_name()
    );

    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));

    // Depth/stencil states -------------------------------------------------------------------------
    // MARK: no depth write, stencil ALWAYS→REPLACE (writes ref 1 into the marked rect).
    let mark_ds = DepthState {
        format: DS_FMT,
        depth_write: false,
        depth_compare: compare::ALWAYS,
        stencil_front: stencil_face(compare::ALWAYS, stencil_op::REPLACE),
        stencil_back: stencil_face(compare::ALWAYS, stencil_op::REPLACE),
        stencil_read_mask: 0xFF,
        stencil_write_mask: 0xFF,
    };
    // Draws: depth-write LESS + stencil EQUAL(ref) — gated to the marked rect, near occludes far.
    let draw_ds = DepthState {
        format: DS_FMT,
        depth_write: true,
        depth_compare: compare::LESS,
        stencil_front: stencil_face(compare::EQUAL, stencil_op::KEEP),
        stencil_back: stencil_face(compare::EQUAL, stencil_op::KEEP),
        stencil_read_mask: 0xFF,
        stencil_write_mask: 0x00,
    };

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, color_tex()), // attachment 0
            Cmd::CreateTexture(2, color_tex()), // attachment 1
            Cmd::CreateTexture(3, ds_tex()),    // depth/stencil
            Cmd::CreateBuffer(1, BufferDesc { size: 48, usage: buffer_usage::UNIFORM, label: String::new() }),
            Cmd::WriteBuffer { id: 1, offset: 0, data: uniform(BG_A, 1.0, BG_B, 1.0, 0.5) }, // BG (opaque, mid)
            Cmd::CreateBuffer(2, BufferDesc { size: 48, usage: buffer_usage::UNIFORM, label: String::new() }),
            Cmd::WriteBuffer { id: 2, offset: 0, data: uniform(FG_A, 0.5, FG_B, 0.5, 0.3) }, // FG (translucent, near)
            Cmd::CreateBuffer(3, BufferDesc { size: 48, usage: buffer_usage::UNIFORM, label: String::new() }),
            Cmd::WriteBuffer { id: 3, offset: 0, data: uniform([255, 0, 0], 0.5, [255, 0, 0], 0.5, 0.9) }, // FAR red (occluded)
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv: wgsl_to_spirv(MARK_WGSL) },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::SpirV, spirv: wgsl_to_spirv(DRAW_WGSL) },
            // MARK pipeline: color writes OFF (mask 0), stencil write only.
            Cmd::CreateRenderPipeline(1, pipeline(1, None, 0x0, mark_ds)),
            // BG opaque: blend None. FG + FAR: src-alpha-over on BOTH targets.
            Cmd::CreateRenderPipeline(2, pipeline(2, None, 0xF, draw_ds.clone())),
            Cmd::CreateRenderPipeline(3, pipeline(2, Some(src_alpha_over()), 0xF, draw_ds.clone())),
            Cmd::CreateRenderPipeline(4, pipeline(2, Some(src_alpha_over()), 0xF, draw_ds)),
            Cmd::CreateBindGroup(1, BindGroupDesc { set: 0, entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 48 } }] }),
            Cmd::CreateBindGroup(2, BindGroupDesc { set: 0, entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 2, offset: 0, size: 48 } }] }),
            Cmd::CreateBindGroup(3, BindGroupDesc { set: 0, entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 3, offset: 0, size: 48 } }] }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![
                            ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true },
                            ColorAttachment { texture: 2, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true },
                        ],
                        depth: Some(DepthAttachment { texture: 3, load: LoadOp::Clear, clear_depth: 1.0, clear_stencil: 0 }),
                    },
                    // The stencil reference (1) is dynamic pass state and applies to MARK's REPLACE write AND
                    // every draw's EQUAL test — set once at pass start.
                    Enc::SetStencilReference { reference: 1 },
                    // 1. MARK the centered rect (stencil = 1).
                    Enc::SetPipeline(1),
                    Enc::Draw { vertex_count: 6, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    // 2. BG opaque, depth 0.5, gated to the rect by stencil EQUAL 1.
                    Enc::SetPipeline(2),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    // 3. FG translucent, depth 0.3 (nearer) → composites over BG in the rect.
                    Enc::SetPipeline(3),
                    Enc::SetBindGroup { index: 0, group: 2 },
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    // 4. FAR translucent red, depth 0.9 (farther) → occluded by FG's depth, never composites.
                    Enc::SetPipeline(4),
                    Enc::SetBindGroup { index: 0, group: 3 },
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the combined depth+stencil+blend+MRT frame must run cleanly");

    let a0 = exec.read_texture(&s.resources, 1).expect("read attachment 0");
    let a1 = exec.read_texture(&s.resources, 2).expect("read attachment 1");
    write_png(&demo_png_dir().join("feature_combo_attach0.png"), N * 8, N * 8, &upscale(&a0, 8));
    write_png(&demo_png_dir().join("feature_combo_attach1.png"), N * 8, N * 8, &upscale(&a1, 8));

    let comp0 = composite(BG_A, FG_A); // [120,140,130,191]
    let comp1 = composite(BG_B, FG_B); // [120,140,160,191]
    let black = [0, 0, 0, 255];

    // MRT separation: the two attachments' composites genuinely differ.
    assert_ne!(comp0, comp1, "the two attachments must composite to DIFFERENT colors for MRT to prove anything");
    // Blend actually composited (not an opaque overwrite of FG, not a no-op leaving BG).
    assert!(comp0 != [FG_A[0], FG_A[1], FG_A[2], comp0[3]] && comp0 != [BG_A[0], BG_A[1], BG_A[2], comp0[3]],
        "attachment 0 composite {comp0:?} must differ from both pure FG and pure BG");

    // EXACT per-pixel over the whole frame, BOTH attachments: inside the marked rect = the composite,
    // outside = the black clear. This single loop enforces stencil gating (rect boundary), depth+blend
    // (the composite value, which excludes the occluded red), and MRT (each attachment's own composite).
    let mut comp0_px = 0usize;
    let mut comp1_px = 0usize;
    let mut red_px = 0usize;
    for y in 0..N {
        for x in 0..N {
            let (p0, p1) = (px(&a0, x, y), px(&a1, x, y));
            let want0 = if inside_rect(x, y) { comp0 } else { black };
            let want1 = if inside_rect(x, y) { comp1 } else { black };
            assert!(near(p0, want0), "attach0 ({x},{y}) inside_rect={}: got {p0:?} want {want0:?}", inside_rect(x, y));
            assert!(near(p1, want1), "attach1 ({x},{y}) inside_rect={}: got {p1:?} want {want1:?}", inside_rect(x, y));
            if near(p0, comp0) { comp0_px += 1; }
            if near(p1, comp1) { comp1_px += 1; }
            // Any pixel with the occluded red tint anywhere in either attachment = depth test failed to occlude.
            if p0[0] > 150 && p0[1] < 100 && p0[2] < 100 { red_px += 1; }
            if p1[0] > 150 && p1[1] < 100 && p1[2] < 100 { red_px += 1; }
        }
    }

    // The stencil gated the composited region to EXACTLY the 16x16 = 256 marked pixels in BOTH attachments.
    assert_eq!(comp0_px, 256, "attachment 0: exactly the 16x16 marked rect must composite (stencil gate)");
    assert_eq!(comp1_px, 256, "attachment 1: exactly the 16x16 marked rect must composite (stencil gate)");
    // The far red draw was occluded by the depth test — it must not survive anywhere.
    assert_eq!(red_px, 0, "the z=0.9 red draw must be occluded by the depth test (near FG at z=0.3 wins)");

    eprintln!(
        "feature_combo_frame OK — attach0 rect={comp0:?} attach1 rect={comp1:?} (256 px each), red occluded; \
         PNGs at {}/feature_combo_attach0.png + feature_combo_attach1.png",
        demo_png_dir().display()
    );
}
