//! TECHNIQUE 2 — linear gradient (vertex-interpolated across a quad), the Skia/GskGpu gouraud-fill path.
//!
//! A full-screen triangle-strip quad carries color A on its two LEFT vertices and color B on its two RIGHT
//! vertices; the fragment stage emits the interpolated varying. The result is a purely HORIZONTAL linear
//! gradient. The colors arrive in a set-0 uniform (data, not baked constants).
//!
//! INDEPENDENT REFERENCE — the analytic lerp. Because the quad has constant w (= 1), the hardware
//! perspective-correct interpolation reduces to an affine (linear) interpolation of the vertex colors, and
//! the LINEAR `Rgba8Unorm` target applies no sRGB gamma. So at pixel column `x` the fraction is
//! `f = (x + 0.5) / W` (the pixel-center NDC map) and the expected byte per channel is exactly
//! `round(A*(1-f) + B*f)`, computed here in Rust from A and B — never read back from the executor. The test
//! samples the two endpoints, the midpoint, and two off-center columns and asserts each matches, then proves
//! the midpoint differs from BOTH endpoints (a real interpolation, not a flat fill or a nearest pick).
//!
//! TOLERANCE ±2 per channel: the rasterizer evaluates barycentric weights in fixed point (a fraction of a
//! ULP away from the real `f`) and the unorm store rounds to nearest; ±2 bounds those two effects and
//! nothing larger — the gradient is otherwise an exact integer ramp. Skips if no adapter is reachable.

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

const W: u32 = 64;
const H: u32 = 8;

const A: [u8; 4] = [40, 80, 200, 255]; // left edge color
const B: [u8; 4] = [220, 160, 30, 255]; // right edge color

// Left verts (vertex_index even → NDC x = -1) get A; right verts (odd → x = +1) get B. Triangle strip of 4.
const VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 a; vec4 b; } u;
layout(location = 0) out vec4 vColor;
void main() {
    float x = ((gl_VertexIndex & 1) == 1) ? 1.0 : -1.0;
    float y = ((gl_VertexIndex >> 1) == 1) ? 1.0 : -1.0;
    vColor = ((gl_VertexIndex & 1) == 1) ? u.b : u.a;
    gl_Position = vec4(x, y, 0.0, 1.0);
}
"#;

const FS: &str = r#"#version 460
layout(location = 0) in vec4 vColor;
layout(location = 0) out vec4 o;
void main() { o = vColor; }
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
    let mut uni = le_f32(&f(A));
    uni.extend(le_f32(&f(B)));

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
                    size: 32,
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
                    topology: Topology::TriangleStrip,
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
                            size: 32,
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
                        vertex_count: 4,
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
    .expect("linear gradient draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

/// The analytic lerp byte at column `x`: `round(A*(1-f) + B*f)`, `f = (x + 0.5) / W`.
fn expected(x: u32) -> [u8; 4] {
    let f = (x as f32 + 0.5) / W as f32;
    let mut out = [0u8; 4];
    for c in 0..4 {
        let v = A[c] as f32 * (1.0 - f) + B[c] as f32 * f;
        out[c] = (v + 0.5) as u8;
    }
    out
}

fn near2(a: [u8; 4], b: [u8; 4]) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= 2)
}

#[test]
fn linear_gradient_matches_the_analytic_lerp() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    let img = render(&mut exec);
    write_png("linear_gradient", W, H, &img);

    let row = H / 2;
    // Endpoints, midpoint, and two off-center columns.
    for &x in &[2u32, W / 4, W / 2, 3 * W / 4, W - 3] {
        let got = px(&img, W, x, row);
        let want = expected(x);
        assert!(
            near2(got, want),
            "linear gradient at column {x}: expected analytic lerp {want:?}, got {got:?}"
        );
    }

    // The gradient is horizontal only — every row must agree (no vertical drift).
    let mid_top = px(&img, W, W / 2, 0);
    let mid_bot = px(&img, W, W / 2, H - 1);
    assert!(
        near2(mid_top, mid_bot),
        "gradient must be purely horizontal: {mid_top:?} vs {mid_bot:?}"
    );

    // Real interpolation: the midpoint is neither endpoint.
    let mid = px(&img, W, W / 2, row);
    assert!(
        !near2(mid, A) && !near2(mid, B),
        "midpoint {mid:?} must differ from both endpoints A {A:?} and B {B:?} (a flat fill would fail)"
    );

    eprintln!(
        "technique 2 (linear_gradient): endpoints+midpoint match analytic lerp within ±2 — PASS"
    );
}
