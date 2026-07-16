//! TECHNIQUE 1 — anti-aliased filled-path edge (the coverage a Skia/GskGpu AA fill produces).
//!
//! Skia and GskGpu antialias a filled path's boundary by giving edge pixels PARTIAL coverage. The wgpu
//! executor's AA path is MSAA (`sample_count = 4` pipeline + `Enc::ResolveTexture`), so this test drives a
//! slanted-edge triangle (opaque WHITE on a BLACK clear) through a 4x multisampled target, resolves it, and
//! reads back.
//!
//! INDEPENDENT REFERENCE — the "4-sample box filter" quantization. A 4x MSAA resolve averages exactly 4
//! samples per texel, so with a WHITE (255) / BLACK (0) contrast every resolved edge pixel MUST be one of
//! the five coverage levels `round(255 * k / 4)` for `k in 0..=4`, i.e. `{0, 64, 128, 191, 255}`, and MUST
//! be neutral gray (`r == g == b`, since white and black are both gray). This reference needs NO knowledge
//! of lavapipe's exact sample POSITIONS — it follows purely from "4 samples averaged with equal weight," so
//! it is a genuine closed-form check, not a snapshot of the executor's own output.
//!
//! The battery asserts, against that reference:
//!   * every resolved pixel is gray and sits on one of the 5 quantized coverage levels (±2 unorm rounding),
//!   * a deep-interior pixel is EXACT white and a deep-exterior pixel is EXACT black,
//!   * PARTIAL-coverage pixels actually exist (the AA ramp — a hard rasterizer produces none), and
//!   * the single-sample (1x) control has ZERO partial pixels (every pixel pure white or pure black),
//!     proving the intermediate values come from multisampling, not from the shader or the format.
//!
//! Skips cleanly if no adapter (lavapipe/Vulkan ICD) is reachable, like the rest of the suite.

mod common;
use common::{glsl, new_session, write_png};

use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, ColorTargetState, Extent3d, Origin3d, RenderPipelineDesc, ShaderRef, TextureDesc,
    TextureSubresource,
};
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp, TextureDim, TextureFormat, Topology};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 40;
const H: u32 = 40;

// A right triangle covering the lower-left half (fy > fx): vertices bottom-left, bottom-right, top-left, so
// its hypotenuse is the main framebuffer diagonal fy == fx — a slanted edge that partially covers a run of
// pixels straddling that diagonal.
const TRI_VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

const WHITE_FS: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(1.0, 1.0, 1.0, 1.0); }
"#;

fn tex(sample_count: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: W,
        height: H,
        depth: 1,
        mip_levels: 1,
        sample_count,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

fn pipeline(sample_count: u32) -> RenderPipelineDesc {
    RenderPipelineDesc {
        vertex: ShaderRef { module: 1, entry: "vmain".into() },
        fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
        vertex_buffers: vec![],
        color_targets: vec![ColorTargetState {
            format: TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask: 0xF,
        }],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        sample_count,
        label: String::new(),
    }
}

/// Draw the triangle at `sample_count`x and (for MSAA) resolve to a single-sample texture; return the
/// tight single-sample RGBA readback plane.
fn render(exec: &mut WgpuExecutor, sample_count: u32) -> Vec<u8> {
    let mut s = new_session(exec);
    let shaders = vec![
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", TRI_VS) },
        Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", WHITE_FS) },
        Cmd::CreateRenderPipeline(1, pipeline(sample_count)),
    ];

    let read_id = if sample_count > 1 {
        let mut cmds = vec![
            Cmd::CreateTexture(1, tex(sample_count, texture_usage::RENDER_TARGET)),
            Cmd::CreateTexture(2, tex(1, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
        ];
        cmds.extend(shaders);
        cmds.push(Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                Enc::EndRenderPass,
                Enc::ResolveTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d { width: W, height: H, depth: 1 },
                },
            ],
            signal: None,
        }));
        hl_gpu::runtime::submit(&mut s, exec, 0, &cmds).expect("MSAA draw + resolve must run");
        2
    } else {
        let mut cmds =
            vec![Cmd::CreateTexture(1, tex(1, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC))];
        cmds.extend(shaders);
        cmds.push(Cmd::Submit(CommandBuffer {
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
        }));
        hl_gpu::runtime::submit(&mut s, exec, 0, &cmds).expect("single-sample draw must run");
        1
    };
    exec.read_texture(&s.resources, read_id).unwrap()
}

fn at(plane: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [plane[i], plane[i + 1], plane[i + 2], plane[i + 3]]
}

/// Distance of `v` to the nearest of the 5 legal 4x-MSAA coverage levels `round(255 * k / 4)`, k = 0..=4.
fn nearest_level_err(v: u8) -> i16 {
    (0..=4)
        .map(|k| (v as i16 - ((255 * k + 2) / 4) as i16).abs())
        .min()
        .unwrap()
}

#[test]
fn msaa_edge_coverage_is_quantized_to_the_four_sample_levels() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return, // no adapter — skip, like the rest of the suite
    };

    let msaa = render(&mut exec, 4);
    write_png("aa_edge_msaa", W, H, &msaa);
    let noaa = render(&mut exec, 1);
    write_png("aa_edge_noaa", W, H, &noaa);

    // Deep interior (fy >> fx, covered) and deep exterior (fx >> fy, uncovered), well clear of the diagonal.
    let interior = at(&msaa, 3, H - 4);
    let exterior = at(&msaa, W - 4, 3);
    assert_eq!(interior, [255, 255, 255, 255], "MSAA interior must be EXACT white (full coverage)");
    assert_eq!(exterior, [0, 0, 0, 255], "MSAA exterior must be EXACT black (zero coverage)");

    // Every resolved pixel: neutral gray on one of the 5 quantized coverage levels. This is the closed-form
    // "4 samples, equal weight" reference — it does not assume any particular sample POSITION.
    let mut partial = 0u32;
    for y in 0..H {
        for x in 0..W {
            let p = at(&msaa, x, y);
            assert_eq!(p[3], 255, "pixel ({x},{y}) alpha must stay opaque (all samples are opaque)");
            assert!(
                (p[0] as i16 - p[1] as i16).abs() <= 1 && (p[1] as i16 - p[2] as i16).abs() <= 1,
                "pixel ({x},{y}) must be neutral gray (white/black average), got {p:?}"
            );
            let err = nearest_level_err(p[0]);
            assert!(
                err <= 2,
                "pixel ({x},{y}) value {} is not on a 4x-MSAA coverage level {{0,64,128,191,255}} (off by {err}) \
                 — the resolve is not a 4-sample box average",
                p[0]
            );
            if p[0] > 2 && p[0] < 253 {
                partial += 1;
            }
        }
    }

    // The AA ramp must EXIST — a hard rasterizer would produce zero partial-coverage pixels.
    assert!(
        partial >= W / 2,
        "MSAA must produce many partial-coverage edge pixels (the AA ramp); got only {partial}"
    );

    // The 1x control is hard-aliased: EVERY pixel is pure white or pure black — proving the intermediate
    // grays above come from multisampling, not from the shader or the unorm format.
    let mut noaa_partial = 0u32;
    for y in 0..H {
        for x in 0..W {
            let p = at(&noaa, x, y);
            let pure = (p[0] <= 1 || p[0] >= 254) && p[0] == p[1] && p[1] == p[2];
            if !pure {
                noaa_partial += 1;
            }
        }
    }
    assert_eq!(
        noaa_partial, 0,
        "the 1x control must have ZERO partial pixels (hard edge); found {noaa_partial} intermediate"
    );

    eprintln!(
        "technique 1 (aa_edge): {partial} quantized partial-coverage pixels on the MSAA edge, 0 on the 1x control — PASS"
    );
}
