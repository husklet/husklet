//! An encoder op that cannot run inside a render pass is REFUSED, not silently dropped.
//!
//! `run_render_pass` receives every op between `BeginRenderPass` and `EndRenderPass` and used to ignore
//! any it did not recognise. `ClearRect`, every copy, `BlitTexture`, `ResolveTexture` and `FillBuffer` all
//! fell into that hole: the work vanished and the submit reported success, so a region of a surface the
//! guest asked to have written stayed exactly as it was minted. That is the shape traced tonight from a
//! translation failure through dropped work to a zero-filled surface showing through a window.
//!
//! It was also a silent DISAGREEMENT between the two executors. The CPU oracle's op loop is flat and
//! state-driven, so it executes a `ClearRect` from inside a pass; the wgpu path dropped it. No test caught
//! that because no test put such an op inside a pass — the differential battery only compares programs
//! both executors were already known to handle.
//!
//! Refusing is the honest state, not necessarily the final one: either the oracle should refuse too, or
//! this path should split the pass and do the work. What must not stand is the third option, where one
//! executor does it, the other does not, and neither says so.

mod gpu_harness;
use gpu_harness::{color_target, glsl, new_session, tex2d};

use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, Extent3d, Origin3d, RenderPipelineDesc, ShaderRef, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp, TextureAspect, Topology};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 4;
const H: u32 = 4;

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(0.0, 1.0, 0.0, 1.0); }
"#;

/// Run a render pass whose body is `body`, wrapped in the setup every case shares.
fn run_pass(exec: &mut WgpuExecutor, body: Vec<Enc>) -> hl_gpu::Result<()> {
    let mut session = new_session(exec);
    let mut encoder = vec![Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: 1,
            load: LoadOp::Clear,
            clear: [0.0, 0.0, 0.0, 1.0],
            store: true,
        }],
        depth: None,
    }];
    encoder.extend(body);
    encoder.push(Enc::EndRenderPass);

    hl_gpu::runtime::submit(
        &mut session,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateTexture(
                2,
                tex2d(W, H, texture_usage::COPY_DST | texture_usage::COPY_SRC),
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
                encoder,
                signal: None,
            }),
        ],
    )
    .map(|_| ())
}

/// THE POSITIVE CONTROL: an ordinary pass body — the state-setting ops plus a draw — must still run. A
/// refusal that swallowed these would satisfy the assertions below while breaking every real frame.
#[test]
fn an_ordinary_render_pass_body_still_runs() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    run_pass(
        &mut exec,
        vec![
            Enc::SetPipeline(1),
            Enc::SetViewport {
                x: 0.0,
                y: 0.0,
                w: W as f32,
                h: H as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            },
            Enc::SetScissor {
                x: 0,
                y: 0,
                w: W,
                h: H,
            },
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
        ],
    )
    .expect("a normal pass body must run");
}

#[test]
fn a_clear_rect_inside_a_render_pass_is_refused_not_dropped() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let outcome = run_pass(
        &mut exec,
        vec![
            Enc::SetPipeline(1),
            Enc::ClearRect {
                texture: 1,
                x: 0,
                y: 0,
                w: 2,
                h: 2,
                color: [1.0, 0.0, 0.0, 1.0],
                base_array_layer: 0,
                layer_count: 1,
                mip_level: 0,
            },
        ],
    );
    assert!(
        outcome.is_err(),
        "a ClearRect inside a pass must be refused — dropping it leaves the region unwritten while the \
         submit reports success, and the CPU oracle performs it from the same position"
    );
}

#[test]
fn a_copy_inside_a_render_pass_is_refused_not_dropped() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let outcome = run_pass(
        &mut exec,
        vec![
            Enc::SetPipeline(1),
            Enc::CopyTextureToTexture {
                src: 1,
                src_sub: TextureSubresource { mip: 0, layer: 0, aspect: TextureAspect::All },
                src_origin: Origin3d { x: 0, y: 0, z: 0 },
                dst: 2,
                dst_sub: TextureSubresource { mip: 0, layer: 0, aspect: TextureAspect::All },
                dst_origin: Origin3d { x: 0, y: 0, z: 0 },
                extent: Extent3d {
                    width: W,
                    height: H,
                    depth: 1,
                },
            },
        ],
    );
    assert!(
        outcome.is_err(),
        "a texture copy inside a pass must be refused rather than silently discarded"
    );
}
