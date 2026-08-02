//! TECHNIQUE 5 — clipping, two ways: a hardware SCISSOR rect and an ALPHA-MASK (coverage) clip via
//! `discard`. Both are the clip paths Skia and GskGpu emit — a rectangular device clip (scissor) and an
//! arbitrary coverage-mask clip (Skia's "clip via alpha", GskGpu's mask ops).
//!
//! Each phase draws a full-screen RED quad over a BLUE clear, but restricts where the draw LANDS, and asserts
//! the classic clip invariant: INSIDE the clip is the drawn color, OUTSIDE is the untouched clear (the clip
//! must not paint, blend, or corrupt those pixels).
//!
//!   * SCISSOR: `Enc::SetScissor { x:4, y:4, w:8, h:8 }` before the draw. The reference is the exact
//!     framebuffer rectangle `[4,12) x [4,12)` — a pixel is RED iff it lies in that rect, else the BLUE clear.
//!   * ALPHA MASK: a 2x1 mask texture (left texel alpha=1 → keep, right texel alpha=0 → `discard`) sampled at
//!     `uv = fragcoord/res`. The reference is the analytic left/right split at `x < W/2`.
//!
//! Both references are computed here from the clip geometry, not snapshotted. Exact integer colors, so the
//! tolerance is ±1 (last-ULP unorm only).

mod gpu_harness;
use gpu_harness::{glsl, le_f32, new_session, px, write_png};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    SamplerDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

use gpu_harness::{color_target, tex2d};

const W: u32 = 16;
const H: u32 = 16;
const RED: [u8; 4] = [220, 40, 40, 255];
const BLUE: [u8; 4] = [30, 60, 220, 255];

// Scissor sub-rect (framebuffer pixels, top-left origin).
const SX: u32 = 4;
const SY: u32 = 4;
const SW: u32 = 8;
const SH: u32 = 8;

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

// Solid-color fragment (scissor phase).
const SOLID_FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 color; } u;
layout(location = 0) out vec4 o;
void main() { o = u.color; }
"#;

// Alpha-mask clip fragment: discard where the sampled mask alpha is low; else paint the color.
const MASK_FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 color; vec4 res; } u;
layout(set = 0, binding = 1) uniform texture2D t;
layout(set = 0, binding = 2) uniform sampler   s;
layout(location = 0) out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy / u.res.xy;
    if (texture(sampler2D(t, s), uv).a < 0.5) { discard; }
    o = u.color;
}
"#;

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

/// SCISSOR clip: draw RED full-screen but scissored to the sub-rect; return the readback.
fn render_scissor(exec: &mut WgpuExecutor) -> Vec<u8> {
    let mut s = new_session(exec);
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
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: le_f32(&colf(RED)),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", SOLID_FS),
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
                            clear: colf(BLUE).map(f64::from),
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::SetScissor {
                        x: SX,
                        y: SY,
                        w: SW,
                        h: SH,
                    },
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
    .expect("scissor clip draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

/// ALPHA-MASK clip: draw RED full-screen, discarding where the mask alpha is 0 (right half); return readback.
fn render_alpha_mask(exec: &mut WgpuExecutor) -> Vec<u8> {
    let mut s = new_session(exec);
    // 2x1 mask: left texel alpha=255 (keep), right texel alpha=0 (discard). rgb irrelevant.
    let mask: Vec<u8> = vec![255, 255, 255, 255, /*|*/ 0, 0, 0, 0];
    let mut uni = le_f32(&colf(RED));
    uni.extend(le_f32(&[W as f32, H as f32, 0.0, 0.0]));

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateTexture(
                2,
                tex2d(2, 1, texture_usage::SAMPLED | texture_usage::COPY_DST),
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
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 8,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: mask,
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", MASK_FS),
            },
            Cmd::CreateSampler(
                1,
                SamplerDesc {
                    min_filter: Filter::Nearest,
                    mag_filter: Filter::Nearest,
                    mip_filter: Filter::Nearest,
                    address_u: AddressMode::ClampToEdge,
                    address_v: AddressMode::ClampToEdge,
                    address_w: AddressMode::ClampToEdge,
                    ..SamplerDesc::default()
                },
            ),
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
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 32,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Sampler { id: 1 },
                        },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 2,
                        src_offset: 0,
                        bytes_per_row: 8,
                        dst: 2,
                        mip: 0,
                        width: 2,
                        height: 1,
                    },
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: colf(BLUE).map(f64::from),
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
    .expect("alpha-mask clip draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

#[test]
fn scissor_rect_clips_the_draw_to_its_rectangle() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let img = render_scissor(&mut exec);
    write_png("clip_scissor", W, H, &img);

    let mut inside = 0u32;
    for y in 0..H {
        for x in 0..W {
            let in_rect = (SX..SX + SW).contains(&x) && (SY..SY + SH).contains(&y);
            let want = if in_rect { RED } else { BLUE };
            let got = px(&img, W, x, y);
            assert!(
                near1(got, want),
                "scissor pixel ({x},{y}) in_rect={in_rect}: expected {want:?}, got {got:?} — the scissor \
                 either leaked outside its rect or failed to fill inside it"
            );
            if in_rect {
                inside += 1;
            }
        }
    }
    assert_eq!(
        inside,
        SW * SH,
        "exactly the {}-pixel scissor rect must be RED",
        SW * SH
    );
    eprintln!(
        "technique 5a (clip/scissor): exact {}-pixel rect clip, rest untouched — PASS",
        SW * SH
    );
}

#[test]
fn alpha_mask_discard_clips_to_the_covered_region() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let img = render_alpha_mask(&mut exec);
    write_png("clip_alpha_mask", W, H, &img);

    for y in 0..H {
        for x in 0..W {
            // uv.x = (x+0.5)/W; mask keeps where uv.x < 0.5, i.e. x < W/2.
            let kept = x < W / 2;
            let want = if kept { RED } else { BLUE };
            let got = px(&img, W, x, y);
            assert!(
                near1(got, want),
                "alpha-mask pixel ({x},{y}) kept={kept}: expected {want:?}, got {got:?} — discard either \
                 painted a masked-out pixel or dropped a covered one"
            );
        }
    }
    // The discarded (right) half must be EXACTLY the untouched clear — no partial write/blend.
    assert!(
        near1(px(&img, W, W - 1, H / 2), BLUE),
        "the discarded half must be the untouched BLUE clear"
    );
    assert!(
        near1(px(&img, W, 0, H / 2), RED),
        "the kept half must be RED"
    );
    eprintln!("technique 5b (clip/alpha-mask): discard clipped to the covered half, rest untouched — PASS");
}
