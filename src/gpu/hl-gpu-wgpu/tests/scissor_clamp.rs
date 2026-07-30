//! GL→wgpu scissor clamping: `glScissor` is a clip rectangle GL intersects with the framebuffer, so a
//! rect that overhangs the target is legal and simply clips. wgpu's `set_scissor_rect` instead REJECTS the
//! whole pass when `x + w > extent.width || y + h > extent.height`, and computes that sum with wrapping
//! `u32` arithmetic — so an app that scissors to its logical layer size after the framebuffer shrank (or
//! any `glScissor(0, 0, HUGE, HUGE)` reset) NACKed the frame, and a hostile `x = u32::MAX` overflowed.
//! This is the scissor half of the same mismatch `viewport_clamp.rs` covers for `glViewport`.
//!
//! Every case drives a FULLSCREEN triangle with no viewport op, so the pixels read back are exactly the
//! effective scissor rect.

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{ColorAttachment, RenderPipelineDesc, ShaderRef};
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp, Topology};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 32;
const H: u32 = 32;
const FILL: [u8; 4] = [230, 170, 40, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 255];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(230.0/255.0, 170.0/255.0, 40.0/255.0, 1.0); }
"#;

/// Clear a `W×H` target and draw the fullscreen triangle under `scissor`. Returns the submit result and
/// the read-back plane; `None` when no GPU adapter is reachable.
#[allow(clippy::type_complexity)]
fn run_with_scissor(scissor: Enc) -> Option<(hl_gpu::Result<()>, Vec<u8>)> {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).ok()?;
    let mut s = new_session(&exec);

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
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
        ],
    )
    .expect("resource setup must succeed");

    let result = hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        1,
        &[Cmd::Submit(CommandBuffer {
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
                scissor,
                Enc::Draw {
                    vertex_count: 3,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        })],
    )
    .map(|_| ());

    let img = exec.read_texture(&s.resources, 1).unwrap();
    Some((result, img))
}

/// Assert the plane is FILL exactly inside `rect` (x0, y0, x1, y1 — half-open) and CLEAR everywhere else.
fn assert_filled_rect(img: &[u8], rect: (u32, u32, u32, u32)) {
    let (x0, y0, x1, y1) = rect;
    for index in 0..W * H {
        let (pxi, py) = (index % W, index / W);
        let inside = (x0..x1).contains(&pxi) && (y0..y1).contains(&py);
        let got = px(img, W, pxi, py);
        let want = if inside { FILL } else { CLEAR };
        assert!(
            near(got, want),
            "px ({pxi},{py}): {got:?} != {want:?} (inside={inside})"
        );
    }
}

/// An overhanging scissor must clip against the target instead of NACKing the frame:
/// `x=10, y=8, w=100, h=100` into a 32×32 target clips to `[10,32)×[8,32)`.
#[test]
fn overhanging_scissor_clips_to_the_target_and_does_not_nack() {
    let Some((result, img)) = run_with_scissor(Enc::SetScissor {
        x: 10,
        y: 8,
        w: 100,
        h: 100,
    }) else {
        return; // no adapter — skip like the rest of the wgpu suite
    };
    write_png("scissor_clamp_overhang", W, H, &img);

    // The load-bearing assertion: previously this exact shape failed wgpu's `InvalidScissorRect`
    // validation and NACKed the whole pass.
    result.expect("an overhanging scissor must clip and run cleanly, not NACK");
    assert_filled_rect(&img, (10, 8, W, H));
}

/// A scissor entirely outside the target draws NOTHING and does not NACK.
#[test]
fn wholly_out_of_bounds_scissor_draws_nothing_without_nack() {
    let Some((result, img)) = run_with_scissor(Enc::SetScissor {
        x: 40,
        y: 40,
        w: 8,
        h: 8,
    }) else {
        return;
    };
    write_png("scissor_clamp_empty", W, H, &img);

    result.expect("a wholly-out-of-bounds scissor must run cleanly (draw dropped), not NACK");
    assert_filled_rect(&img, (0, 0, 0, 0));
}

/// A scissor whose `x + w` overflows `u32` must not reach wgpu's wrapping bounds check: the intersection
/// is empty, so nothing rasterizes and the frame stays valid.
#[test]
fn overflowing_scissor_origin_draws_nothing_without_nack() {
    let Some((result, img)) = run_with_scissor(Enc::SetScissor {
        x: u32::MAX,
        y: u32::MAX,
        w: 4,
        h: 4,
    }) else {
        return;
    };

    result.expect("an overflowing scissor rect must be refused by intersection, not by wgpu");
    assert_filled_rect(&img, (0, 0, 0, 0));
}

/// A zero-area scissor clips everything away in GL. It must not paint through wgpu's default full-target
/// scissor.
#[test]
fn zero_area_scissor_draws_nothing() {
    let Some((result, img)) = run_with_scissor(Enc::SetScissor {
        x: 4,
        y: 4,
        w: 0,
        h: 16,
    }) else {
        return;
    };

    result.expect("a zero-width scissor must run cleanly");
    assert_filled_rect(&img, (0, 0, 0, 0));
}
