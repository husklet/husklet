//! EXHAUSTIVE depth-compare coverage: every one of the 8 protocol compare functions (NEVER..ALWAYS) run
//! through a real depth-tested draw on the `WgpuExecutor`, asserting the fragment survives exactly when the
//! shared oracle predicate `compare::passes(code, frag, stored)` says it should.
//!
//! `pipeline::compare_function` has an arm per compare code, but before this only LESS/GREATER were driven
//! (via the depth demos + differential). Here a full-screen quad is drawn at a controlled fragment depth
//! `frag` into an attachment pre-cleared to `stored`; if the depth test passes the fragment paints RED, else
//! the cleared BLACK survives. Each (compare, frag) pair is checked against `compare::passes` — the SAME
//! predicate the CPU oracle uses — so the wgpu mapping is pinned to the neutral semantics. Skips with no
//! adapter.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    DepthAttachment, DepthState, RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 4;
const H: u32 = 4;

// Vertex reads the fragment depth from the uniform's `.x` and emits a full-screen triangle at that z.
const VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 params; } u;
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], u.params.x, 1.0);
}
"#;

const FS: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(1.0, 0.0, 0.0, 1.0); }
"#;

fn glsl(stage: u32, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: "main".to_string(),
        source: source.to_string(),
    }
    .to_words()
}

fn color_tex() -> TextureDesc {
    TextureDesc {
        width: W,
        height: H,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
    }
}

fn depth_tex() -> TextureDesc {
    TextureDesc {
        width: W,
        height: H,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Depth32Float,
        usage: texture_usage::RENDER_TARGET,
        label: String::new(),
    }
}

fn session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

/// Draw at depth `frag` with `depth_compare` into an attachment cleared to `stored`; return whether the
/// fragment survived (center pixel is red).
fn drew_through(exec: &mut WgpuExecutor, frag: f32, stored: f32, depth_compare: u32) -> bool {
    let mut s = session(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, color_tex()),
            Cmd::CreateTexture(2, depth_tex()),
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
                data: [frag, 0.0, 0.0, 0.0]
                    .iter()
                    .flat_map(|f: &f32| f.to_le_bytes())
                    .collect(),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, FS),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "main".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "main".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
                    depth: Some(DepthState::depth_only(
                        TextureFormat::Depth32Float,
                        false,
                        depth_compare,
                    )),
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
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: Some(DepthAttachment {
                            texture: 2,
                            depth_load: LoadOp::Clear,
                            stencil_load: LoadOp::Clear,
                            clear_depth: stored,
                            clear_stencil: 0,
                        }),
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
    .expect("the depth-tested draw must run cleanly");
    let px = exec.read_texture(&s.resources, 1).unwrap();
    let c = ((H / 2 * W + W / 2) * 4) as usize;
    px[c] > 200 && px[c + 1] < 50 // red survived vs black cleared
}

#[test]
fn every_depth_compare_gates_exactly_like_the_oracle() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let stored = 0.5_f32;
    let codes = [
        compare::NEVER,
        compare::LESS,
        compare::EQUAL,
        compare::LESS_EQUAL,
        compare::GREATER,
        compare::NOT_EQUAL,
        compare::GREATER_EQUAL,
        compare::ALWAYS,
    ];
    // Three relationships to `stored` distinguish all eight comparisons.
    for &frag in &[0.25_f32, 0.5, 0.75] {
        for &code in &codes {
            let want = compare::passes(code, frag, stored);
            let got = drew_through(&mut exec, frag, stored, code);
            assert_eq!(
                got, want,
                "depth compare {code} at frag={frag} stored={stored}: oracle says pass={want}, executor drew={got}"
            );
        }
    }
}
