//! GL→wgpu viewport clamping: a `glViewport` that starts negative and/or overhangs the framebuffer is
//! legal in GL (the viewport is purely the NDC→window transform; the framebuffer clips the fragments) but
//! wgpu's `set_viewport` REJECTS it as a device-validation error. Forwarding Chrome's scrolled-layer
//! viewport verbatim NACKed its first real frame and orphaned the pass's resources (a downstream
//! `UnknownId` cascade). The executor now intersects the GL viewport with the render target so wgpu accepts
//! it, and drops draws under a wholly-out-of-bounds viewport (which rasterize nothing in GL).
//!
//! Both cases below drive a FULLSCREEN triangle (covers all of NDC), so the viewport rectangle is filled
//! EXACTLY — which is what lets us read the effective viewport straight off the pixels.

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

/// Build a session with the shared pipeline (id 1) and a `W×H` render target (id 1), then run one pass that
/// clears the target and draws the fullscreen triangle under `viewport`. Returns the submit result and, on
/// success, the read-back plane. Skips (returns `None`) when no GPU adapter is reachable.
#[allow(clippy::type_complexity)]
fn run_with_viewport(viewport: Enc) -> Option<(hl_gpu::Result<()>, Vec<u8>)> {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).ok()?;
    let mut s = new_session(&exec);

    // Create resources first (their own submit), so the pass submit under test carries ONLY the draw.
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
                viewport,
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
    .map(|_| ()); // discard the presented-frame list; these tests read the target back directly

    let img = exec.read_texture(&s.resources, 1).unwrap();
    Some((result, img))
}

/// (a) A negative-Y, oversized viewport must NOT NACK, and must clip to the intersection with the target.
/// `x=-8, y=-16, w=48, h=40` into a 32×32 target intersects to `[0,32)×[0,24)`, so the fullscreen triangle
/// fills exactly rows `y ∈ [0,24)` (the visible part of the scrolled layer) and leaves `y ∈ [24,32)` clear.
#[test]
fn negative_and_oversized_viewport_clips_and_does_not_nack() {
    let Some((result, img)) = run_with_viewport(Enc::SetViewport {
        x: -8.0,
        y: -16.0,
        w: 48.0,
        h: 40.0,
        min_depth: 0.0,
        max_depth: 1.0,
    }) else {
        return; // no adapter — skip like the rest of the wgpu suite
    };
    write_png("viewport_clamp_partial", W, H, &img);

    // The load-bearing assertion: the frame is VALID now (previously this exact shape NACKed with
    // `InvalidViewportRect { y: -16, ... }` and orphaned the pass).
    result.expect("a negative-Y / oversized viewport must clamp and run cleanly, not NACK");

    let mut filled = 0usize;
    for py in 0..H {
        for pxi in 0..W {
            let inside = py < 24; // clamped viewport is [0,32)×[0,24)
            let got = px(&img, W, pxi, py);
            let want = if inside { FILL } else { CLEAR };
            assert!(
                near(got, want),
                "px ({pxi},{py}): {got:?} != {want:?} (inside={inside})"
            );
            if inside {
                filled += 1;
            }
        }
    }
    assert_eq!(
        filled,
        W as usize * 24,
        "the clamped viewport must fill exactly the 32×24 visible band"
    );
}

/// (b) A viewport entirely outside the target draws NOTHING and does not NACK: `x=100, y=100` into a 32×32
/// target has an empty intersection, so the draw is dropped and the target keeps its clear color.
#[test]
fn wholly_out_of_bounds_viewport_draws_nothing_without_nack() {
    let Some((result, img)) = run_with_viewport(Enc::SetViewport {
        x: 100.0,
        y: 100.0,
        w: 32.0,
        h: 32.0,
        min_depth: 0.0,
        max_depth: 1.0,
    }) else {
        return;
    };
    write_png("viewport_clamp_empty", W, H, &img);

    result.expect("a wholly-out-of-bounds viewport must run cleanly (draw dropped), not NACK");

    for py in 0..H {
        for pxi in 0..W {
            let got = px(&img, W, pxi, py);
            assert!(
                near(got, CLEAR),
                "px ({pxi},{py}): {got:?} must be CLEAR — nothing should rasterize"
            );
        }
    }
}
