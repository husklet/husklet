//! TECHNIQUE 4 — separable box blur (two passes), the Skia/GskGpu blur/shadow primitive.
//!
//! A sharp WHITE rectangle on BLACK is blurred by a 3-tap (radius-1) box kernel applied in TWO separable
//! passes: a horizontal pass (T0 → T1) then a vertical pass (T1 → T2). Each pass samples three NEAREST taps
//! with CLAMP-to-edge addressing and averages them. Both passes do real work because the feature varies in
//! both axes.
//!
//! INDEPENDENT REFERENCE — the two-stage convolution, computed in Rust. The reference reproduces the exact
//! pipeline the GPU runs, INCLUDING the intermediate unorm8 quantization between the two passes:
//!   pass1_byte[x,y] = round(255 * (feat(x-1)+feat(x)+feat(x+1)) / 3),   feat ∈ {0,1}, clamp x
//!   out_byte[x,y]   = round(255 * (v(y-1)+v(y)+v(y+1)) / 3),            v = pass1_byte / 255, clamp y
//! With `feat ∈ {0,1}` the pass-1 means are exactly `{0, 85, 170, 255}` (no rounding), and the pass-2 means
//! never land on a `.5` unorm tie, so the reference is exact up to f32 summation-order ULPs. This is a
//! genuine convolution reference derived from the kernel definition, never a snapshot of the executor.
//!
//! Asserts: (a) the whole output equals the two-stage reference within ±2; (b) the blur actually SOFTENED
//! the feature — edge pixels that were pure 0/255 in the source now hold intermediate values, and there are
//! many such pixels; (c) a deep-interior feature pixel stays ~white and a far-background pixel stays black.
//!
//! TOLERANCE ±2 per channel: bounds the intermediate + final unorm rounding and f32 summation-order ULPs;
//! the box means avoid `.5` ties so nothing larger can occur. Skips if no adapter is reachable.

mod gpu_harness;
use gpu_harness::{glsl, le_f32, new_session, px, write_png};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

use gpu_harness::color_target;

const W: u32 = 16;
const H: u32 = 16;

/// The sharp source feature: a white 8x8 rect [4,12) on black. `1` = white texel, `0` = black.
fn feat(x: i32, y: i32) -> u8 {
    if (4..12).contains(&x) && (4..12).contains(&y) {
        1
    } else {
        0
    }
}

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

// u.xy = 1/resolution (uv scale), u.zw = the per-tap step (1/W,0) or (0,1/H). Nearest + clamp addressing.
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 u; } p;
layout(set = 0, binding = 1) uniform texture2D t;
layout(set = 0, binding = 2) uniform sampler   s;
layout(location = 0) out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy * p.u.xy;
    vec2 d = p.u.zw;
    vec4 c = texture(sampler2D(t, s), uv - d)
           + texture(sampler2D(t, s), uv)
           + texture(sampler2D(t, s), uv + d);
    o = c / 3.0;
}
"#;

fn tex(w: u32, h: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

fn nearest_clamp() -> SamplerDesc {
    SamplerDesc {
        min_filter: Filter::Nearest,
        mag_filter: Filter::Nearest,
        mip_filter: Filter::Nearest,
        address_u: AddressMode::ClampToEdge,
        address_v: AddressMode::ClampToEdge,
        address_w: AddressMode::ClampToEdge,
    }
}

fn pipeline() -> RenderPipelineDesc {
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
    }
}

fn bind_group(set_id: u32, uni: u32, texture: u32) -> Cmd {
    Cmd::CreateBindGroup(
        set_id,
        BindGroupDesc {
            set: 0,
            entries: vec![
                BindEntry {
                    binding: 0,
                    resource: BindResource::Buffer {
                        id: uni,
                        offset: 0,
                        size: 16,
                    },
                },
                BindEntry {
                    binding: 1,
                    resource: BindResource::Texture { id: texture },
                },
                BindEntry {
                    binding: 2,
                    resource: BindResource::Sampler { id: 1 },
                },
            ],
        },
    )
}

fn render(exec: &mut WgpuExecutor) -> Vec<u8> {
    let mut s = new_session(exec);

    // Seed the source texture bytes.
    let mut src = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let v = if feat(x as i32, y as i32) == 1 {
                255
            } else {
                0
            };
            let i = ((y * W + x) * 4) as usize;
            src[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }

    let inv = [1.0 / W as f32, 1.0 / H as f32];
    let uni_h = le_f32(&[inv[0], inv[1], 1.0 / W as f32, 0.0]); // horizontal step
    let uni_v = le_f32(&[inv[0], inv[1], 0.0, 1.0 / H as f32]); // vertical step

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(W, H, texture_usage::SAMPLED | texture_usage::COPY_DST),
            ), // T0 source
            Cmd::CreateTexture(
                2,
                tex(W, H, texture_usage::RENDER_TARGET | texture_usage::SAMPLED),
            ), // T1
            Cmd::CreateTexture(
                3,
                tex(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ), // T2
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: (W * H * 4) as u64,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src,
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: uni_h,
            },
            Cmd::CreateBuffer(
                3,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 3,
                offset: 0,
                data: uni_v,
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
            Cmd::CreateSampler(1, nearest_clamp()),
            Cmd::CreateRenderPipeline(1, pipeline()),
            bind_group(1, 2, 1), // pass 1: sample T0 with the horizontal step
            bind_group(2, 3, 2), // pass 2: sample T1 with the vertical step
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: W * 4,
                        dst: 1,
                        mip: 0,
                        width: W,
                        height: H,
                    },
                    // Pass 1 — horizontal blur T0 → T1.
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 2,
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
                    // Pass 2 — vertical blur T1 → T2.
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 3,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 2 },
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
    .expect("two-pass separable blur must run cleanly");
    exec.read_texture(&s.resources, 3).unwrap()
}

/// The two-stage box-blur reference (grayscale luma; the feature is neutral so all rgb channels agree).
fn reference() -> Vec<u8> {
    let clampx = |x: i32| x.clamp(0, W as i32 - 1);
    let clampy = |y: i32| y.clamp(0, H as i32 - 1);
    // Pass 1: horizontal, quantized to unorm8.
    let mut p1 = vec![0u8; (W * H) as usize];
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            let sum =
                feat(clampx(x - 1), y) as f32 + feat(x, y) as f32 + feat(clampx(x + 1), y) as f32;
            p1[(y as u32 * W + x as u32) as usize] = (255.0 * (sum / 3.0) + 0.5) as u8;
        }
    }
    // Pass 2: vertical, reading p1 back as /255 floats, quantized to unorm8.
    let mut out = vec![0u8; (W * H) as usize];
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            let v = |yy: i32| p1[(clampy(yy) as u32 * W + x as u32) as usize] as f32 / 255.0;
            let sum = v(y - 1) + v(y) + v(y + 1);
            out[(y as u32 * W + x as u32) as usize] = (255.0 * (sum / 3.0) + 0.5) as u8;
        }
    }
    out
}

#[test]
fn separable_box_blur_matches_the_two_stage_convolution() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    let img = render(&mut exec);
    write_png("blur", W, H, &img);
    let refimg = reference();

    // (a) whole-image match to the two-stage reference.
    let mut worst = 0i16;
    let mut intermediate = 0u32;
    for y in 0..H {
        for x in 0..W {
            let got = px(&img, W, x, y);
            let want = refimg[(y * W + x) as usize];
            for k in 0..3 {
                worst = worst.max((got[k] as i16 - want as i16).abs());
                assert!(
                    (got[k] as i16 - want as i16).abs() <= 2,
                    "blur at ({x},{y}) ch{k}: expected two-stage reference {want}, got {}",
                    got[k]
                );
            }
            // (b) count softened pixels — a value strictly between black and white that the SHARP source
            // never had at this location.
            let src_v = if feat(x as i32, y as i32) == 1 {
                255
            } else {
                0
            };
            if got[0] > 2 && got[0] < 253 && (src_v == 0 || src_v == 255) {
                intermediate += 1;
            }
        }
    }

    assert!(
        intermediate >= W,
        "the blur must SOFTEN the feature: many pixels that were pure 0/255 in the source must now be \
         intermediate; got only {intermediate}"
    );

    // (c) deep interior stays ~white, far background stays black.
    assert!(
        px(&img, W, 7, 7)[0] >= 250,
        "the feature core must stay ~white after a radius-1 blur"
    );
    assert_eq!(
        px(&img, W, 0, 0)[0],
        0,
        "a far-corner background pixel must stay pure black"
    );

    eprintln!(
        "technique 4 (blur): whole-image match to two-stage box reference (worst ±{worst}), {intermediate} softened pixels — PASS"
    );
}
