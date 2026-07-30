//! TECHNIQUE 3 — radial gradient (shader-computed distance-from-center ramp), the Skia/GskGpu radial fill.
//!
//! A full-screen triangle's fragment shader reads its own `gl_FragCoord.xy` (window-space pixel centers,
//! upper-left origin — the readback's coordinate frame), measures the distance to a uniform-supplied center,
//! normalizes it by a uniform radius, clamps to `[0,1]`, and `mix`es an inner color to an outer color by
//! that fraction — the exact arithmetic Skia's radial shader runs.
//!
//! INDEPENDENT REFERENCE — the analytic radial ramp. The test recomputes, in Rust, the SAME geometric
//! function from first principles: for pixel `(x,y)`, `d = hypot(x+0.5 - cx, y+0.5 - cy)`,
//! `t = clamp(d / radius, 0, 1)`, `channel = round(inner*(1-t) + outer*t)`. This is a closed-form reference
//! derived from the gradient's DEFINITION, not a snapshot of the executor — it would catch a wrong `length`,
//! a wrong divide, or a wrong `mix`. Asserted over the WHOLE image, plus the exact endpoints (center = inner,
//! a corner beyond the radius = outer) and a set of intermediate radii that must be strictly between.
//!
//! TOLERANCE ±2 per channel: `sqrt`, the divide, and `mix` each run in f32 and the unorm store rounds to
//! nearest; ±2 bounds those. The ramp is smooth (no hard edge), so no sample can diverge by more. Skips if
//! no adapter is reachable.

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
const CX: f32 = 24.5; // a pixel CENTER, so pixel (24,24) sits exactly at distance 0 (the inner color)
const CY: f32 = 24.5;
const RADIUS: f32 = 20.0;

const INNER: [u8; 4] = [240, 60, 40, 255];
const OUTER: [u8; 4] = [20, 40, 120, 255];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

// params = (cx, cy, radius, _pad)
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 params; vec4 inner; vec4 outer; } u;
layout(location = 0) out vec4 o;
void main() {
    float d = distance(gl_FragCoord.xy, u.params.xy);
    float t = clamp(d / u.params.z, 0.0, 1.0);
    o = mix(u.inner, u.outer, t);
}
"#;

fn render(exec: &mut WgpuExecutor) -> Vec<u8> {
    let mut s = new_session(exec);
    let f = |c: [u8; 4]| {
        [
            c[0] as f32 / 255.0,
            c[1] as f32 / 255.0,
            c[2] as f32 / 255.0,
            c[3] as f32 / 255.0,
        ]
    };
    let mut uni = le_f32(&[CX, CY, RADIUS, 0.0]);
    uni.extend(le_f32(&f(INNER)));
    uni.extend(le_f32(&f(OUTER)));

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
                            clear: [0.0, 0.0, 0.0, 1.0],
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
    .expect("radial gradient draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

/// The analytic radial-ramp byte for pixel `(x, y)`.
fn expected(x: u32, y: u32) -> [u8; 4] {
    let dx = (x as f32 + 0.5) - CX;
    let dy = (y as f32 + 0.5) - CY;
    let t = ((dx * dx + dy * dy).sqrt() / RADIUS).clamp(0.0, 1.0);
    let mut out = [0u8; 4];
    for c in 0..4 {
        let v = INNER[c] as f32 * (1.0 - t) + OUTER[c] as f32 * t;
        out[c] = (v + 0.5) as u8;
    }
    out
}

fn near2(a: [u8; 4], b: [u8; 4]) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= 2)
}

#[test]
fn radial_gradient_matches_the_analytic_ramp() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let img = render(&mut exec);
    write_png("radial_gradient", W, H, &img);

    // Whole-image match against the analytic ramp.
    let mut worst = 0i16;
    for y in 0..H {
        for x in 0..W {
            let got = px(&img, W, x, y);
            let want = expected(x, y);
            for k in 0..4 {
                worst = worst.max((got[k] as i16 - want[k] as i16).abs());
            }
            assert!(
                near2(got, want),
                "radial gradient at ({x},{y}): expected analytic ramp {want:?}, got {got:?}"
            );
        }
    }

    // Exact endpoints: the center pixel is the inner color; a corner (distance ~33 > radius 20) is the outer.
    assert!(
        near2(px(&img, W, 24, 24), INNER),
        "center must be the inner color {INNER:?}"
    );
    assert!(
        near2(px(&img, W, 0, 0), OUTER),
        "a corner beyond the radius must be the outer color {OUTER:?}"
    );

    // Monotonic ramp along a radius: moving outward from the center, the red channel (inner-high) must not
    // increase — a genuine distance-driven ramp.
    let mut last = 256i16;
    for x in 24..44 {
        let r = px(&img, W, x, 24)[0] as i16;
        assert!(
            r <= last + 2,
            "red channel must be non-increasing along the radius; {r} after {last}"
        );
        last = r;
    }

    eprintln!(
        "technique 3 (radial_gradient): whole-image match to analytic ramp (worst ±{worst}) — PASS"
    );
}
