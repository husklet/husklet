//! TECHNIQUE 7 — sub-pixel text coverage / glyph-atlas sampling (the Skia/GskGpu text path).
//!
//! GskGpu and Skia render glyphs by sampling a COVERAGE atlas (an alpha/coverage mask) at a SUB-PIXEL offset
//! — the glyph's fractional device position — with bilinear filtering, so the emitted coverage is the
//! bilinear blend of the four neighboring atlas texels. This test builds a 2x2 coverage atlas with four
//! distinct coverage bytes and samples it with a LINEAR sampler at several sub-pixel positions, reading back
//! the sampled coverage.
//!
//! INDEPENDENT REFERENCE — the bilinear formula, computed in Rust. For a 2x2 texture the texel centers sit
//! at uv 0.25 and 0.75, so sampling at `uv` maps to texel space `t = uv*2 - 0.5`; the reference takes the
//! four surrounding texels and blends them by the fractional weights `(1-fx)(1-fy)`, `fx(1-fy)`,
//! `(1-fx)fy`, `fx·fy`. That is the DEFINITION of bilinear interpolation, evaluated here from the atlas
//! bytes — never read back from the executor. The sub-pixel positions are chosen inside `[0.25, 0.75]` so no
//! edge clamping is involved, keeping the reference a clean closed form.
//!
//! Asserts each sampled coverage equals its bilinear reference, and that a genuinely sub-pixel position
//! (unequal weights) differs from every one of the four atlas texels — proving real bilinear positioning,
//! not a nearest tap. TOLERANCE ±2: hardware bilinear uses finite sub-texel fixed-point weights; ±2 bounds
//! that plus the unorm store. Skips if no adapter is reachable.

mod gpu_harness;
use gpu_harness::{glsl, le_f32, new_session, tex2d, write_png};

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

use gpu_harness::color_target;

// 2x2 coverage atlas, `COV[col][row]` — four distinct coverage bytes so the bilinear weights are observable.
const COV: [[u8; 2]; 2] = [
    [40, 100],  // col 0: row0, row1
    [200, 255], // col 1: row0, row1
];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

// Sample the coverage atlas (stored grayscale) at the uniform uv; emit the sampled coverage.
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec2 uv; } u;
layout(set = 0, binding = 1) uniform texture2D t;
layout(set = 0, binding = 2) uniform sampler   s;
layout(location = 0) out vec4 o;
void main() { o = texture(sampler2D(t, s), u.uv); }
"#;

fn linear_clamp() -> SamplerDesc {
    SamplerDesc {
        min_filter: Filter::Linear,
        mag_filter: Filter::Linear,
        mip_filter: Filter::Nearest,
        address_u: AddressMode::ClampToEdge,
        address_v: AddressMode::ClampToEdge,
        address_w: AddressMode::ClampToEdge,
        ..SamplerDesc::default()
    }
}

/// Sample the atlas at `uv` with bilinear filtering into a 1x1 target; return the readback pixel.
fn tap(exec: &mut WgpuExecutor, uv: [f32; 2]) -> [u8; 4] {
    let mut s = new_session(exec);
    // Atlas bytes, row-major: (col0,row0),(col1,row0),(col0,row1),(col1,row1); grayscale RGBA.
    let g = |c: u8| [c, c, c, 255];
    let mut atlas = Vec::new();
    atlas.extend_from_slice(&g(COV[0][0]));
    atlas.extend_from_slice(&g(COV[1][0]));
    atlas.extend_from_slice(&g(COV[0][1]));
    atlas.extend_from_slice(&g(COV[1][1]));

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(1, 1, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateTexture(
                2,
                tex2d(2, 2, texture_usage::SAMPLED | texture_usage::COPY_DST),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 8,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: le_f32(&uv),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: atlas,
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
            Cmd::CreateSampler(1, linear_clamp()),
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
                                size: 8,
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
                        height: 2,
                    },
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
    .expect("glyph-atlas bilinear sample must run cleanly");
    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

/// Reference bilinear coverage at `uv` on the 2x2 atlas (texel centers at 0.25/0.75, clamp-to-edge).
fn bilinear(uv: [f32; 2]) -> u8 {
    let tx = uv[0] * 2.0 - 0.5;
    let ty = uv[1] * 2.0 - 0.5;
    let i0 = tx.floor().clamp(0.0, 1.0) as usize;
    let i1 = (i0 + 1).min(1);
    let j0 = ty.floor().clamp(0.0, 1.0) as usize;
    let j1 = (j0 + 1).min(1);
    let fx = (tx - tx.floor()).clamp(0.0, 1.0);
    let fy = (ty - ty.floor()).clamp(0.0, 1.0);
    let c = |i: usize, j: usize| COV[i][j] as f32;
    let top = c(i0, j0) * (1.0 - fx) + c(i1, j0) * fx;
    let bot = c(i0, j1) * (1.0 - fx) + c(i1, j1) * fx;
    let v = top * (1.0 - fy) + bot * fy;
    (v + 0.5) as u8
}

#[test]
fn glyph_atlas_bilinear_coverage_matches_the_subpixel_reference() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Sub-pixel positions (glyph fractional placements), all inside [0.25,0.75] → no edge clamping.
    let positions = [
        [0.5f32, 0.5],  // dead center: equal weights → average of all four
        [0.375, 0.625], // fx=0.25, fy=0.75
        [0.625, 0.375], // fx=0.75, fy=0.25
        [0.45, 0.55],   // an off-grid sub-pixel offset
    ];

    for uv in positions {
        let got = tap(&mut exec, uv);
        let want = bilinear(uv);
        write_png(
            &format!("text_coverage_{:.0}_{:.0}", uv[0] * 100.0, uv[1] * 100.0),
            1,
            1,
            &got,
        );
        assert!(
            got[0] == got[1] && got[1] == got[2],
            "coverage sample at uv={uv:?} must stay grayscale, got {got:?}"
        );
        assert!(
            (got[0] as i16 - want as i16).abs() <= 2,
            "sub-pixel coverage at uv={uv:?}: expected bilinear {want}, got {}",
            got[0]
        );
    }

    // A genuinely sub-pixel offset (unequal weights) must differ from EVERY atlas texel — real bilinear
    // positioning, not a nearest tap that snapped to one texel.
    let off = tap(&mut exec, [0.375, 0.625])[0];
    for (col, column) in COV.iter().enumerate() {
        for (row, coverage) in column.iter().enumerate() {
            assert!(
                (off as i16 - *coverage as i16).abs() > 2,
                "sub-pixel coverage {off} must differ from atlas texel COV[{col}][{row}]={} — else it is a \
                 nearest tap, not bilinear",
                coverage
            );
        }
    }

    eprintln!("technique 7 (text_coverage): sub-pixel glyph-atlas taps match the bilinear reference within ±2 — PASS");
}
