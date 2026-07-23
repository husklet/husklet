use super::*;

// =================================================================================================
// (5) DEPTH / attachment mismatches
// =================================================================================================

#[test]
fn depth_attachment_on_color_format_is_invalid() {
    let Some(mut g) = exec() else { return };
    hostile(
        &mut g,
        "depth_attach_color",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateTexture(2, tex(4, 4, TextureFormat::Rgba8Unorm, RT)), // color, misused as depth
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
                            load: LoadOp::Clear,
                            clear_depth: 1.0,
                            clear_stencil: 0,
                        }),
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
        is_invalid,
    );
}

#[test]
fn depth_tested_pipeline_in_color_only_pass_is_invalid() {
    let Some(mut g) = exec() else { return };
    let vs = "#version 460\nvoid main(){ gl_Position = vec4(0.0,0.0,0.5,1.0); }\n";
    let fs = "#version 460\nlayout(location=0) out vec4 c; void main(){ c = vec4(1.0); }\n";
    hostile(
        &mut g,
        "depth_pipe_color_pass",
        &[
            Cmd::CreateTexture(1, tex(4, 4, TextureFormat::Rgba8Unorm, RT)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", vs),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs),
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
                    depth: Some(DepthState::depth_only(
                        TextureFormat::Depth32Float,
                        true,
                        compare::LESS,
                    )),
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    // pipeline wants a depth attachment; the pass has none.
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
        is_invalid,
    );
}
