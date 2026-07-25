use super::*;

// (6) GLSL — an advertised payload really executing (naga glsl-in → wgsl-out → device)
// =================================================================================================

#[test]
fn glsl_vertex_fragment_triangle_renders() {
    let vs = "#version 450\n\
        void main() {\n\
            float x = -1.0; float y = -1.0;\n\
            if (gl_VertexIndex == 1) { x = 3.0; }\n\
            if (gl_VertexIndex == 2) { y = 3.0; }\n\
            gl_Position = vec4(x, y, 0.0, 1.0);\n\
        }\n";
    let fs = "#version 450\n\
        layout(location = 0) out vec4 o;\n\
        void main() { o = vec4(0.0, 0.0, 1.0, 1.0); }\n";
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl_words(glsl_stage::VERTEX, "vmain", vs),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl_words(glsl_stage::FRAGMENT, "fmain", fs),
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
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
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
                            clear: [1.0, 0.0, 0.0, 1.0],
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
    );
    all_texels_eq(&g.read_texture(&s.resources, 1).unwrap(), [0, 0, 255, 255]);
}

// =================================================================================================
