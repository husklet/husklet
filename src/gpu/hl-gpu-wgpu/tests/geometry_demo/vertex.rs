use super::grid::{assert_grid, FLAT_COLOR_FS, GRID};
use super::*;

const VBUF_VS: &str = r#"#version 460
layout(location = 0) in vec4 rect; // per-instance: center.xy, half.zw
layout(location = 0) flat out vec4 vColor;
void main() {
    vec2 corner = vec2(float(gl_VertexIndex & 1), float((gl_VertexIndex >> 1) & 1)) * 2.0 - 1.0;
    vec2 pos = rect.xy + corner * rect.zw;
    vec4 pal[4] = vec4[4](
        vec4(220.0/255.0, 40.0/255.0, 40.0/255.0, 1.0),
        vec4(40.0/255.0, 200.0/255.0, 60.0/255.0, 1.0),
        vec4(50.0/255.0, 90.0/255.0, 230.0/255.0, 1.0),
        vec4(240.0/255.0, 240.0/255.0, 240.0/255.0, 1.0)
    );
    vColor = pal[gl_InstanceIndex];
    gl_Position = vec4(pos, 0.0, 1.0);
}
"#;

// Packed vertex-attribute format (the GL driver's `vertex_format_wire`): comps | (kind<<8) | (norm<<16).
// A 4-component f32 → comps=4, kind=0 → 4.
const VFMT_F32X4: u32 = 4;

#[test]
fn instanced_from_vertex_buffer() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let mut s = new_session(&exec);

    let insts: Vec<f32> = GRID.iter().flatten().copied().collect();

    hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            // The per-instance vertex buffer: 4 × vec4 (center.xy, half.zw).
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 64,
                    usage: buffer_usage::VERTEX,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: le_f32(&insts),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VBUF_VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FLAT_COLOR_FS),
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
                    // ONE per-instance vertex buffer: stride 16, step_mode=1 (Instance), a single vec4 attr.
                    vertex_buffers: vec![VertexLayout {
                        stride: 16,
                        step_mode: 1,
                        attrs: vec![VertexAttr {
                            location: 0,
                            format: VFMT_F32X4,
                            offset: 0,
                        }],
                    }],
                    color_targets: vec![color_target()],
                    depth: None,
                    topology: Topology::TriangleStrip,
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
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 1,
                        offset: 0,
                    },
                    Enc::Draw {
                        vertex_count: 4,
                        instance_count: 4,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the vertex-buffer-fed instanced grid draw must run cleanly");

    let px = exec.read_texture(&s.resources, 1).unwrap();
    assert_grid("instanced_vertexbuffer_grid", &px);
}

// ---------------------------------------------------------------------------------------------------
// tiny built-in PNG encoder (RGBA8, uncompressed/stored DEFLATE) — for human visual confirmation only
// ---------------------------------------------------------------------------------------------------
