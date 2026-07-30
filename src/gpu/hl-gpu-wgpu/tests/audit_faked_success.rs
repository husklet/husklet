//! FAKED-SUCCESS audit locks (task #209): each test here mints IR exercising an op/field the executor used
//! to silently drop, and asserts the REAL device effect — so a regression that re-fakes the op turns the
//! test red. Three previously-faked behaviours are covered:
//!
//!   1. `CopyTextureToBuffer { mip }` — the handler ignored `mip` (`..` in the destructure) and always read
//!      back mip 0 through a hardcoded `mip_level: 0` readback. A guest reading a non-base mip got the base
//!      level's bytes with an `Ok`. Now the named level is read.
//!   2. `ColorTargetState::write_mask` — the render pipeline hardcoded `ColorWrites::ALL`, so a masked
//!      channel (`glColorMask`) was written anyway. Now the mask is honored: a masked channel is untouched.
//!   3. `RenderPipelineDesc::cull` / `front_face` — the pipeline used `PrimitiveState::default()` (no cull,
//!      Ccw), so `glCullFace`/`glFrontFace` state vanished. Now both are honored.
//!
//! These are wgpu-only (self-consistent) asserts, not oracle-compared: the CPU oracle models none of the
//! three (it rejects a non-zero mip copy, and never culls or masks — see `hl-gpu/src/cpu`), exactly like
//! the MSAA analytic tests. If no wgpu adapter is reachable the tests skip, like the rest of the suite.

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, ColorAttachment, ColorTargetState, RenderPipelineDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{BufferId, Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// A fullscreen-triangle vertex shader (no vertex buffer — drives from gl_VertexIndex).
const VS_FULLSCREEN: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

// A fragment shader emitting a fixed constant colour, so the readback is a pure function of the pipeline
// state under test (write mask / cull), not of any bound resource.
fn fs_const(c: [u8; 4]) -> String {
    format!(
        "#version 460\nlayout(location = 0) out vec4 o;\nvoid main() {{ o = vec4({}, {}, {}, {}); }}\n",
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    )
}

/// A render pipeline drawing the constant-colour fullscreen triangle, parameterised by the state under test.
fn const_pipeline(cull: u32, front_face: u32, write_mask: u32) -> RenderPipelineDesc {
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
        color_targets: vec![ColorTargetState {
            format: TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask,
        }],
        depth: None,
        topology: Topology::TriangleList,
        cull,
        front_face,
        sample_count: 1,
        label: String::new(),
    }
}

/// Render the constant-colour fullscreen triangle into a 1×1 target cleared to `clear`, with the given
/// pipeline `cull` / `front_face` / colour `write_mask`, and return the single readback pixel.
fn render(
    exec: &mut WgpuExecutor,
    color: [u8; 4],
    clear: [u8; 4],
    cull: u32,
    front_face: u32,
    write_mask: u32,
) -> [u8; 4] {
    let mut s = new_session(exec);
    let clear_f = [
        clear[0] as f32 / 255.0,
        clear[1] as f32 / 255.0,
        clear[2] as f32 / 255.0,
        clear[3] as f32 / 255.0,
    ];
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(1, 1, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS_FULLSCREEN),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", &fs_const(color)),
            },
            Cmd::CreateRenderPipeline(1, const_pipeline(cull, front_face, write_mask)),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: clear_f,
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
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
    .expect("the constant-colour draw must run cleanly");
    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

// ---------------------------------------------------------------------------------------------------
// 1. CopyTextureToBuffer honors `mip`
// ---------------------------------------------------------------------------------------------------

const M0: [u8; 4] = [210, 50, 60, 255]; // base-mip texel
const M1: [u8; 4] = [50, 200, 90, 255]; // mip-1 texel (distinct from M0)

#[test]
fn copy_texture_to_buffer_reads_the_named_mip_level() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let mut s = new_session(&exec);

    let m0_plane: Vec<u8> = M0.iter().cycle().take(16).copied().collect(); // 2×2 of M0

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            // A 2×2 texture with 2 mip levels; both copy directions used.
            Cmd::CreateTexture(
                1,
                tex2d_mips(2, 2, 2, texture_usage::COPY_DST | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: m0_plane,
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: M1.to_vec(),
            },
            // Readback destinations (mip 1 → buf 3, mip 0 first texel → buf 4).
            Cmd::CreateBuffer(
                3,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(
                4,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    // Upload each level to its OWN mip slot.
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 8,
                        dst: 1,
                        mip: 0,
                        width: 2,
                        height: 2,
                    },
                    Enc::CopyBufferToTexture {
                        src: 2,
                        src_offset: 0,
                        bytes_per_row: 4,
                        dst: 1,
                        mip: 1,
                        width: 1,
                        height: 1,
                    },
                    // Read back the 1×1 MIP-1 level: must be M1, NOT the base level's M0.
                    Enc::CopyTextureToBuffer {
                        src: 1,
                        mip: 1,
                        width: 1,
                        height: 1,
                        dst: 3,
                        dst_offset: 0,
                        bytes_per_row: 4,
                    },
                    // Read back the base level's first texel: M0 (proving mip 0 still works).
                    Enc::CopyTextureToBuffer {
                        src: 1,
                        mip: 0,
                        width: 1,
                        height: 1,
                        dst: 4,
                        dst_offset: 0,
                        bytes_per_row: 4,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("the two-level upload + per-mip readback must run cleanly");

    let mip1 = exec.read_buffer(&s.resources, BufferId(3), 0, 4).unwrap();
    let mip0 = exec.read_buffer(&s.resources, BufferId(4), 0, 4).unwrap();
    assert_eq!(
        mip1.as_slice(),
        M1.as_slice(),
        "CopyTextureToBuffer {{ mip: 1 }} must read the MIP-1 texel {M1:?} (the bug read the base level {M0:?})",
    );
    assert_eq!(
        mip0.as_slice(),
        M0.as_slice(),
        "CopyTextureToBuffer {{ mip: 0 }} must still read the base texel"
    );
}

#[test]
fn copy_texture_to_buffer_rejects_out_of_range_mip() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let mut s = new_session(&exec);

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d_mips(2, 2, 2, texture_usage::COPY_DST | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
        ],
    )
    .expect("resource creation must succeed");

    // Mip 2 does not exist on a 2-level texture — an honest typed error, not a faked Ok reading some level.
    let r = hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyTextureToBuffer {
                src: 1,
                mip: 2,
                width: 1,
                height: 1,
                dst: 1,
                dst_offset: 0,
                bytes_per_row: 4,
            }],
            signal: None,
        })],
    );
    assert!(
        r.is_err(),
        "a CopyTextureToBuffer naming a non-existent mip must error, not fake success"
    );
}

// ---------------------------------------------------------------------------------------------------
// 2. write_mask is honored
// ---------------------------------------------------------------------------------------------------

const CLEAR: [u8; 4] = [10, 20, 30, 250];
const DRAW: [u8; 4] = [200, 100, 50, 40];

#[test]
fn color_write_mask_leaves_the_masked_channel_untouched() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    // Full mask (0xF): every channel is the drawn colour (±1 for unorm rounding on written channels).
    let full = render(&mut exec, DRAW, CLEAR, 0, 0, 0xF);
    assert!(
        near(full, DRAW),
        "write_mask 0xF must write every channel: expected ~{DRAW:?}, got {full:?}"
    );

    // RGB-only mask (0x7): R,G,B are the drawn colour; ALPHA stays the CLEAR alpha EXACTLY (never written,
    // so no rounding). Before the fix the executor hardcoded ColorWrites::ALL and alpha became the draw's 40.
    let masked = render(&mut exec, DRAW, CLEAR, 0, 0, 0x7);
    assert!(
        (masked[0] as i16 - DRAW[0] as i16).abs() <= 1
            && (masked[1] as i16 - DRAW[1] as i16).abs() <= 1
            && (masked[2] as i16 - DRAW[2] as i16).abs() <= 1,
        "write_mask 0x7 must still write R,G,B: expected ~{:?}, got {masked:?}",
        &DRAW[..3]
    );
    assert_eq!(
        masked[3], CLEAR[3],
        "write_mask 0x7 masks ALPHA — it must keep the cleared alpha {} exactly (the bug wrote the draw's {})",
        CLEAR[3], DRAW[3]
    );
}

// ---------------------------------------------------------------------------------------------------
// 3. cull + front_face are honored
// ---------------------------------------------------------------------------------------------------

#[test]
fn cull_and_front_face_are_honored() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let culled = |p: [u8; 4]| p == CLEAR; // nothing drawn ⇒ still the clear colour
    let drawn = |p: [u8; 4]| near(p, DRAW);

    // Baseline: cull = None (0) always draws the triangle regardless of winding.
    let none = render(&mut exec, DRAW, CLEAR, 0, 0, 0xF);
    assert!(
        drawn(none),
        "cull=None must draw the triangle: expected ~{DRAW:?}, got {none:?}"
    );

    // cull=Front (1) vs cull=Back (2) at a fixed winding: EXACTLY ONE culls this single triangle (it is
    // either front- or back-facing, never both), so the two results must differ — one drawn, one culled.
    // If `cull` were ignored, both would draw and be equal (the pre-fix behaviour).
    let front = render(&mut exec, DRAW, CLEAR, 1, 0, 0xF);
    let back = render(&mut exec, DRAW, CLEAR, 2, 0, 0xF);
    assert!(
        (drawn(front) && culled(back)) || (culled(front) && drawn(back)),
        "cull=Front and cull=Back must give OPPOSITE outcomes for one triangle: front={front:?} back={back:?}"
    );

    // front_face flips which winding is 'front', so at a fixed cull=Back it flips the cull outcome:
    // the same triangle is culled under one front_face and drawn under the other. If `front_face` were
    // ignored the two would be equal.
    let ccw_back = render(&mut exec, DRAW, CLEAR, 2, 0, 0xF);
    let cw_back = render(&mut exec, DRAW, CLEAR, 2, 1, 0xF);
    assert!(
        (drawn(ccw_back) && culled(cw_back)) || (culled(ccw_back) && drawn(cw_back)),
        "flipping front_face at cull=Back must flip the cull outcome: ccw={ccw_back:?} cw={cw_back:?}"
    );
}
