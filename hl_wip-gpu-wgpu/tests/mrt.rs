//! DEMO 1 — Multiple Render Targets (MRT): one fragment shader writing TWO color attachments.
//!
//! Question answered with exact pixels: does a single draw fan its fragment outputs to the CORRECT distinct
//! attachments? A fullscreen triangle runs a fragment that writes `location = 0` and `location = 1` to two
//! separate `Rgba8Unorm` targets. The two colors are chosen far apart, and BOTH readbacks are asserted
//! exactly: target 0 must be entirely color A, target 1 must be entirely color B. A backend that dropped the
//! second attachment, wrote the same color to both, or swapped them fails.
//!
//! To prove the outputs are DATA (not two baked constants that happen to land right), the two colors arrive
//! in a single std140 uniform block and the fragment routes `u.a → location 0`, `u.b → location 1`.

mod common;
use common::*;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, LoadOp, Topology};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 16;
const H: u32 = 16;
const A: [u8; 4] = [220, 40, 40, 255]; // → attachment 0
const B: [u8; 4] = [40, 120, 230, 255]; // → attachment 1

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

// Two outputs, each routed from a distinct member of the uniform block — proving the fan-out is data-driven.
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 a; vec4 b; } u;
layout(location = 0) out vec4 o0;
layout(location = 1) out vec4 o1;
void main() {
    o0 = u.a;
    o1 = u.b;
}
"#;

fn f4(c: [u8; 4]) -> [f32; 4] {
    [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, c[3] as f32 / 255.0]
}

#[test]
fn two_color_attachments_each_hold_their_own_output() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return, // no adapter — skip like the rest of the suite
    };
    let mut s = new_session(&exec);

    let mut ubo = f4(A).to_vec();
    ubo.extend_from_slice(&f4(B));

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
            Cmd::CreateTexture(2, tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
            Cmd::CreateBuffer(1, BufferDesc { size: 32, usage: buffer_usage::UNIFORM, label: String::new() }),
            Cmd::WriteBuffer { id: 1, offset: 0, data: le_f32(&ubo) },
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", VS) },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS) },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef { module: 1, entry: "vmain".into() },
                    fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
                    vertex_buffers: vec![],
                    // TWO color targets — the MRT part.
                    color_targets: vec![color_target(), color_target()],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    label: String::new(),
                },
            ),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 32 } }],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![
                            ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true },
                            ColorAttachment { texture: 2, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true },
                        ],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the two-attachment MRT draw must run cleanly");

    let t0 = exec.read_texture(&s.resources, 1).unwrap();
    let t1 = exec.read_texture(&s.resources, 2).unwrap();
    write_png("mrt_target0", W, H, &t0);
    write_png("mrt_target1", W, H, &t1);

    // Target 0 = A everywhere, target 1 = B everywhere; and the two must genuinely DIFFER.
    for (i, out) in t0.chunks_exact(4).enumerate() {
        assert!(near(out.try_into().unwrap(), A), "target0 px {i}: {out:?} != A {A:?}");
    }
    for (i, out) in t1.chunks_exact(4).enumerate() {
        assert!(near(out.try_into().unwrap(), B), "target1 px {i}: {out:?} != B {B:?}");
    }
    assert!(!near(A, B), "the two attachment colors must differ for the demo to prove anything");
    eprintln!("demo `mrt`: target0=A target1=B exact — PNGs at {OUT_DIR}/mrt_target0.png, mrt_target1.png");
}
