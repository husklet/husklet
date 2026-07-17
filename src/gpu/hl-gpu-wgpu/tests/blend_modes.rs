//! TECHNIQUE 8 — Porter-Duff SrcOver plus the advanced separable blend modes MULTIPLY and SCREEN, asserted
//! by their exact algebraic identity. These are the compositing modes Skia's `SkBlendMode` and GskGpu emit.
//!
//! Each mode is expressed with the protocol's fixed-function [`BlendState`] (factors + op) — every mode here
//! IS representable as `src·srcFac  ADD  dst·dstFac`:
//!   * SrcOver (premultiplied): `src·ONE + dst·(1-srcA)`      → factors ONE / ONE_MINUS_SRC_ALPHA
//!   * Multiply:                `src·dst + dst·ZERO = src·dst` → factors DST_COLOR / ZERO
//!   * Screen:  `src·ONE + dst·(1-src) = src + dst - src·dst`  → factors ONE / ONE_MINUS_SRC_COLOR
//!
//! INDEPENDENT REFERENCE — the closed-form identity, computed in Rust. The target is cleared to `dst` (so the
//! STORED destination is the unorm8-rounded `dst`, which the reference uses), a full-screen triangle emits a
//! known `src`, and the read-back center pixel is compared to the analytic result of each mode's identity —
//! `src+dst·(1-srcA)`, `src·dst`, `src+dst-src·dst` — never to the executor's own output. The three modes
//! produce three DISTINCT results for the chosen inputs, and the test also asserts they differ from each
//! other, so a stub that ignored the blend factors could not pass all three.
//!
//! TOLERANCE ±2 per channel: the hardware blends normalized values then rounds to unorm8; ±2 bounds that
//! rounding (and the one unorm8 rounding already baked into the stored `dst`). Skips if no adapter is
//! reachable.
//!
//! COVERAGE-GAP NOTE (reported, not a failure): the neutral [`BlendState`] models exactly the fixed-function
//! factor/op set. NON-separable / advanced modes that are NOT expressible that way (Overlay, Darken,
//! ColorDodge, HSL Hue/Saturation/Color/Luminosity, …) have NO representation in the protocol at all — they
//! cannot even be requested — so they are an honest protocol-level gap, not a wrong pixel.

mod common;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment,
    ColorTargetState, RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

use common::{glsl, new_session};

const W: u32 = 4;
const H: u32 = 4;

// Blend-factor wire codes (the neutral GL-driver numbering the executor decodes).
const ZERO: u32 = 0;
const ONE: u32 = 1;
const ONE_MINUS_SRC_COLOR: u32 = 3;
const ONE_MINUS_SRC_ALPHA: u32 = 5;
const DST_COLOR: u32 = 6;
const OP_ADD: u32 = 0;

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 color; } u;
layout(location = 0) out vec4 o;
void main() { o = u.color; }
"#;

fn tex(usage: u32) -> TextureDesc {
    TextureDesc {
        width: W,
        height: H,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

/// Draw `src` over a `dst`-cleared target with `blend`; return the center pixel.
fn run(exec: &mut WgpuExecutor, src: [f32; 4], dst: [f32; 4], blend: BlendState) -> [u8; 4] {
    let mut s = new_session(exec);
    let target = ColorTargetState {
        format: TextureFormat::Rgba8Unorm,
        blend: Some(blend),
        write_mask: 0xF,
    };
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src.iter().flat_map(|f| f.to_le_bytes()).collect(),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "main", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "main", FS),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "main".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "main".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![target],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 16,
                        },
                    }],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: dst,
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("blend-mode draw must run cleanly");
    let px = exec.read_texture(&s.resources, 1).unwrap();
    let c = ((H / 2 * W + W / 2) * 4) as usize;
    [px[c], px[c + 1], px[c + 2], px[c + 3]]
}

fn u8c(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// The unorm8-rounded destination actually stored by the `LoadOp::Clear` (what the hardware blends against).
fn stored(dst: [f32; 4]) -> [f32; 4] {
    let mut o = [0f32; 4];
    for k in 0..4 {
        o[k] = u8c(dst[k]) as f32 / 255.0;
    }
    o
}

fn blend(src_color: u32, dst_color: u32, src_alpha: u32, dst_alpha: u32) -> BlendState {
    BlendState {
        src_color,
        dst_color,
        op_color: OP_ADD,
        src_alpha,
        dst_alpha,
        op_alpha: OP_ADD,
    }
}

fn near2(a: [u8; 4], b: [u8; 4]) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= 2)
}

#[test]
fn porter_duff_src_over_multiply_and_screen_match_their_identities() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    let src = [0.6f32, 0.5, 0.8, 0.7];
    let dst = [0.4f32, 0.9, 0.3, 0.5];
    let d = stored(dst);

    // ---- SrcOver (premultiplied): out.rgb = src + dst*(1-srcA); out.a = srcA + dstA*(1-srcA). ----
    let src_over = run(
        &mut exec,
        src,
        dst,
        blend(ONE, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA),
    );
    let so_ref = [
        u8c(src[0] + d[0] * (1.0 - src[3])),
        u8c(src[1] + d[1] * (1.0 - src[3])),
        u8c(src[2] + d[2] * (1.0 - src[3])),
        u8c(src[3] + d[3] * (1.0 - src[3])),
    ];
    assert!(
        near2(src_over, so_ref),
        "SrcOver: expected src+dst*(1-srcA) = {so_ref:?}, got {src_over:?}"
    );

    // ---- Multiply: out.rgb = src*dst (alpha kept as src.a via ONE/ZERO). ----
    let multiply = run(&mut exec, src, dst, blend(DST_COLOR, ZERO, ONE, ZERO));
    let mul_ref = [
        u8c(src[0] * d[0]),
        u8c(src[1] * d[1]),
        u8c(src[2] * d[2]),
        u8c(src[3]),
    ];
    assert!(
        near2(multiply, mul_ref),
        "Multiply: expected src*dst = {mul_ref:?}, got {multiply:?}"
    );

    // ---- Screen: out.rgb = src + dst*(1-src) = src + dst - src*dst (alpha kept as src.a). ----
    let screen = run(
        &mut exec,
        src,
        dst,
        blend(ONE, ONE_MINUS_SRC_COLOR, ONE, ZERO),
    );
    let scr_ref = [
        u8c(src[0] + d[0] - src[0] * d[0]),
        u8c(src[1] + d[1] - src[1] * d[1]),
        u8c(src[2] + d[2] - src[2] * d[2]),
        u8c(src[3]),
    ];
    assert!(
        near2(screen, scr_ref),
        "Screen: expected src+dst-src*dst = {scr_ref:?}, got {screen:?}"
    );

    // The three modes must produce DISTINCT results for these inputs — a stub ignoring the blend factors
    // could not satisfy all three identities AND make them differ.
    assert!(
        !near2(multiply, screen),
        "Multiply {multiply:?} and Screen {screen:?} must differ"
    );
    assert!(
        !near2(multiply, src_over),
        "Multiply {multiply:?} and SrcOver {src_over:?} must differ"
    );
    assert!(
        !near2(screen, src_over),
        "Screen {screen:?} and SrcOver {src_over:?} must differ"
    );

    // Sanity of the modes' physical meaning: Multiply darkens (result <= dst on each channel), Screen
    // lightens (result >= dst on each channel) — an independent structural check.
    for k in 0..3 {
        assert!(
            multiply[k] <= (d[k] * 255.0 + 2.0) as u8,
            "Multiply must darken channel {k}"
        );
        assert!(
            screen[k] as f32 >= d[k] * 255.0 - 2.0,
            "Screen must lighten channel {k}"
        );
    }

    eprintln!(
        "technique 8 (blend_modes): SrcOver={src_over:?} Multiply={multiply:?} Screen={screen:?} all match their \
         exact identities; advanced non-separable modes are unrepresentable in the protocol (reported gap) — PASS"
    );
}
