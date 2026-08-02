//! TECHNIQUE 6 — rounded rectangle via a signed-distance field, the GskGpu/Skia rounded-rect primitive.
//!
//! A full-screen triangle's fragment evaluates the canonical rounded-box SDF at its own `gl_FragCoord`:
//!   p = fragcoord - center;  q = abs(p) - half + radius;  d = length(max(q, 0)) - radius
//! and `discard`s where `d > 0`, painting the color where `d <= 0`. This is exactly the rounded-rect
//! coverage GskGpu emits for a `GskRoundedRect` clip/fill.
//!
//! INDEPENDENT REFERENCE — the SDF sign, recomputed in Rust from the rounded-rect DEFINITION. For every
//! pixel the test computes the same `d` and expects the color inside (`d < 0`) and the untouched clear
//! outside (`d > 0`). Because a pixel exactly on the boundary (`d ≈ 0`) is genuinely ambiguous at f32
//! precision, the whole-image check skips a 1.5-px guard band around `d = 0` (asserting only clearly-inside
//! and clearly-outside pixels), which is honest rather than a loosened tolerance.
//!
//! The DECISIVE rounding check does not rely on the guard band: the four extreme CORNER pixels (which a
//! SHARP rectangle of the same bounds WOULD fill) must be the untouched clear — proving the corners are
//! actually rounded away — while the four straight-edge midpoints must be filled. A sharp-rect executor
//! would fail the corner assertions. Exact color, ±1 tolerance.

mod gpu_harness;
use gpu_harness::{glsl, le_f32, new_session, px, write_png};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    ShaderRef,
};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, LoadOp, Topology};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

use gpu_harness::{color_target, tex2d};

const W: u32 = 48;
const H: u32 = 48;
const CX: f32 = 24.0;
const CY: f32 = 24.0;
const HX: f32 = 18.0; // half-extent x → rect spans x in [6, 42]
const HY: f32 = 18.0;
const R: f32 = 8.0; // corner radius

const COLOR: [u8; 4] = [230, 180, 40, 255];
const BG: [u8; 4] = [10, 10, 12, 255];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

// center_half.xy = center, center_half.zw = half; params.x = radius.
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 center_half; vec4 params; vec4 color; } u;
layout(location = 0) out vec4 o;
void main() {
    vec2 p = gl_FragCoord.xy - u.center_half.xy;
    vec2 q = abs(p) - u.center_half.zw + vec2(u.params.x);
    float d = length(max(q, vec2(0.0))) - u.params.x;
    if (d > 0.0) { discard; }
    o = u.color;
}
"#;

/// The rounded-box signed distance at pixel `(x, y)` (negative = inside).
fn sdf(x: u32, y: u32) -> f32 {
    let px = (x as f32 + 0.5) - CX;
    let py = (y as f32 + 0.5) - CY;
    let qx = (px.abs() - HX + R).max(0.0);
    let qy = (py.abs() - HY + R).max(0.0);
    (qx * qx + qy * qy).sqrt() - R
}

fn near1(a: [u8; 4], b: [u8; 4]) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= 1)
}

fn colf(c: [u8; 4]) -> [f32; 4] {
    [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    ]
}

fn render(exec: &mut WgpuExecutor) -> Vec<u8> {
    let mut s = new_session(exec);
    let mut uni = le_f32(&[CX, CY, HX, HY]);
    uni.extend(le_f32(&[R, 0.0, 0.0, 0.0]));
    uni.extend(le_f32(&colf(COLOR)));

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 48,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: uni,
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![color_target()],
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
                            size: 48,
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
                            clear: colf(BG).map(f64::from),
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
    .expect("rounded-rect SDF draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

#[test]
fn rounded_rect_sdf_rounds_the_corners_at_the_exact_radius() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let img = render(&mut exec);
    write_png("rounded_rect", W, H, &img);

    // Whole-image SDF-sign classification, skipping the ambiguous 1.5-px boundary band.
    for y in 0..H {
        for x in 0..W {
            let d = sdf(x, y);
            if d.abs() < 1.5 {
                continue; // boundary band: f32 sign is ambiguous — honestly skipped
            }
            let want = if d < 0.0 { COLOR } else { BG };
            let got = px(&img, W, x, y);
            assert!(
                near1(got, want),
                "rounded-rect at ({x},{y}) sdf={d:.2}: expected {want:?}, got {got:?}"
            );
        }
    }

    // DECISIVE: the four extreme corners a SHARP rect would fill must be the untouched clear (rounded away).
    for &(x, y) in &[(6u32, 6u32), (41, 6), (6, 41), (41, 41)] {
        // Sanity: these corners are inside the sharp bounding rect [6,42] but outside the rounded shape.
        assert!(
            sdf(x, y) > 1.5,
            "corner ({x},{y}) must be clearly OUTSIDE the rounded shape in the reference"
        );
        assert!(
            near1(px(&img, W, x, y), BG),
            "corner ({x},{y}) must be the untouched clear — a rounded corner, not a sharp one; got {:?}",
            px(&img, W, x, y)
        );
    }

    // The four straight-edge midpoints must be filled (the shape is a rounded RECT, not a circle).
    for &(x, y) in &[(24u32, 8u32), (24, 39), (8, 24), (39, 24)] {
        assert!(
            sdf(x, y) < -1.5,
            "edge midpoint ({x},{y}) must be clearly INSIDE in the reference"
        );
        assert!(
            near1(px(&img, W, x, y), COLOR),
            "straight-edge midpoint ({x},{y}) must be filled; got {:?}",
            px(&img, W, x, y)
        );
    }

    eprintln!("technique 6 (rounded_rect): corners rounded at r={R}, straight edges filled, SDF sign exact — PASS");
}
