//! DEMO 7 — viewport AND scissor combined: a non-default viewport TRANSFORMS a fullscreen draw into a
//! sub-rectangle, and a scissor rect further CLIPS it; the drawn pixels must equal exactly the intersection.
//!
//! A fullscreen triangle (covers all of NDC) is drawn with:
//!   * viewport (x=4, y=4, w=24, h=16) → the draw fills framebuffer pixels x∈[4,28), y∈[4,20). Because the
//!     triangle covers all of clip space, the viewport rectangle is filled EXACTLY — proving the transform.
//!   * scissor  (x=10, y=8, w=14, h=20) → pixels are additionally clipped to x∈[10,24), y∈[8,28).
//!
//! The visible result is the intersection x∈[10,24), y∈[8,20): its left/right/top edges come from the
//! scissor, its BOTTOM edge (y=20) comes from the viewport — so the exact rectangle can only be produced
//! when BOTH are honored. Every covered pixel must be FILL, every other pixel CLEAR.

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

#[test]
fn viewport_transforms_and_scissor_clips_to_exact_subrect() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };
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
                    Enc::SetViewport {
                        x: 4.0,
                        y: 4.0,
                        w: 24.0,
                        h: 16.0,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    },
                    Enc::SetScissor {
                        x: 10,
                        y: 8,
                        w: 14,
                        h: 20,
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
    .expect("the viewport+scissor draw must run cleanly");

    let img = exec.read_texture(&s.resources, 1).unwrap();
    write_png("viewport_scissor", W, H, &img);

    // Intersection of viewport [4,28)×[4,20) and scissor [10,24)×[8,28) = [10,24)×[8,20).
    let mut filled = 0usize;
    for py in 0..H {
        for pxi in 0..W {
            let inside = (10..24).contains(&pxi) && (8..20).contains(&py);
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
        14 * 12,
        "the clipped draw must cover exactly the 14×12 intersection rect"
    );
    eprintln!(
        "demo `viewport_scissor`: exact 14×12 intersection — PNG at {OUT_DIR}/viewport_scissor.png"
    );
}
