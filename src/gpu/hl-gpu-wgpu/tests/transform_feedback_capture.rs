mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const VS: &str = r#"#version 460
layout(location=0) in vec4 a_position;
layout(location=1) in float a_pointSize;
void hl_tf_user_main(){ gl_Position=a_position; gl_PointSize=a_pointSize; }
layout(set=0,binding=64,std430) buffer Out { uint words[]; } tf;
layout(set=0,binding=68,std140) uniform Offset { uvec4 base_words; } off;
void main(){ hl_tf_user_main(); tf.words[off.base_words.x]=floatBitsToUint(gl_PointSize); }
"#;
const FS: &str = "#version 460\nlayout(location=0) out vec4 c; void main(){discard;}";

#[test]
fn separate_scalar_arrays_compile_with_raw_word_capture() {
    let source = r#"#version 460
layout(location=0) in vec4 a_position;
layout(location=1) in float a_varA_e0;
layout(location=2) in float a_varB_e0;
layout(location=3) in float a_varB_e1;
layout(location=0) out float v_varA[1];
layout(location=1) out float v_varB[2];
void hl_tf_user_main() {
    gl_Position = a_position;
    v_varA[0] = a_varA_e0;
    v_varB[0] = a_varB_e0;
    v_varB[1] = a_varB_e1;
}
layout(set=0,binding=64,std430) buffer OutA { uint words[]; } tf_a;
layout(set=0,binding=65,std430) buffer OutB { uint words[]; } tf_b;
layout(set=0,binding=68,std140) uniform Offsets { uvec4 base_words; } offsets;
void main() {
    hl_tf_user_main();
    tf_a.words[offsets.base_words.x] = floatBitsToUint(v_varA[0]);
    tf_b.words[offsets.base_words.y] = floatBitsToUint(v_varB[0]);
    tf_b.words[offsets.base_words.y + 1u] = floatBitsToUint(v_varB[1]);
}
"#;
    let mut parser = naga::front::glsl::Frontend::default();
    parser
        .parse(
            &naga::front::glsl::Options::from(naga::ShaderStage::Vertex),
            source,
        )
        .expect("separate transform-feedback array wrapper must compile");
}

#[test]
fn vertex_stage_captures_actual_output_to_storage() {
    let mut gpu = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut session = new_session(&gpu);
    let expected = 6.3965964f32;
    let commands = vec![
        Cmd::CreateTexture(1, tex2d(1, 1, texture_usage::RENDER_TARGET)),
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 16,
                usage: buffer_usage::VERTEX,
                label: "position".into(),
            },
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: [0.075f32, -0.214, -0.471, 1.143]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        },
        Cmd::CreateBuffer(
            4,
            BufferDesc {
                size: 4,
                usage: buffer_usage::VERTEX,
                label: "point-size".into(),
            },
        ),
        Cmd::WriteBuffer {
            id: 4,
            offset: 0,
            data: expected.to_le_bytes().to_vec(),
        },
        Cmd::CreateBuffer(
            2,
            BufferDesc {
                size: 4,
                usage: buffer_usage::STORAGE | buffer_usage::COPY_SRC,
                label: "output".into(),
            },
        ),
        Cmd::CreateBuffer(
            3,
            BufferDesc {
                size: 16,
                usage: buffer_usage::UNIFORM,
                label: "offset".into(),
            },
        ),
        Cmd::WriteBuffer {
            id: 3,
            offset: 0,
            data: vec![0; 16],
        },
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
                vertex_buffers: vec![
                    VertexLayout {
                        stride: 16,
                        step_mode: 0,
                        attrs: vec![VertexAttr {
                            location: 0,
                            format: 0x04,
                            offset: 0,
                        }],
                    },
                    VertexLayout {
                        stride: 4,
                        step_mode: 0,
                        attrs: vec![VertexAttr {
                            location: 1,
                            format: 0x01,
                            offset: 0,
                        }],
                    },
                ],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0,
                }],
                depth: None,
                topology: Topology::PointList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: "tf".into(),
            },
        ),
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry {
                        binding: 64,
                        resource: BindResource::Buffer {
                            id: 2,
                            offset: 0,
                            size: 4,
                        },
                    },
                    BindEntry {
                        binding: 68,
                        resource: BindResource::Buffer {
                            id: 3,
                            offset: 0,
                            size: 16,
                        },
                    },
                ],
            },
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load: LoadOp::Clear,
                        clear: [0.0; 4],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::SetVertexBuffer {
                    slot: 0,
                    buffer: 1,
                    offset: 0,
                },
                Enc::SetVertexBuffer {
                    slot: 1,
                    buffer: 4,
                    offset: 0,
                },
                Enc::Draw {
                    vertex_count: 1,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        }),
    ];
    hl_gpu::runtime::submit(&mut session, &mut gpu, 0, &commands).expect("capture draw");
    let bytes = gpu
        .read_buffer(&session.resources, hl_gpu::BufferId(2), 0, 4)
        .expect("readback");
    assert_eq!(bytes, expected.to_le_bytes());
}

#[test]
fn wide_vertex_layout_coexists_with_multiple_transform_feedback_buffers() {
    const WIDE_VS: &str = r#"#version 460
layout(location=0) in float a0;
layout(location=1) in float a1;
layout(location=2) in float a2;
layout(location=3) in float a3;
layout(location=4) in float a4;
layout(location=5) in float a5;
layout(location=6) in float a6;
layout(location=7) in float a7;
layout(location=8) in float a8;
layout(location=9) in float a9;
layout(location=10) in float a10;
layout(location=11) in float a11;
layout(location=12) in float a12;
layout(set=0,binding=64,std430) buffer OutA { uint words[]; } tf_a;
layout(set=0,binding=65,std430) buffer OutB { uint words[]; } tf_b;
layout(set=0,binding=68,std140) uniform Offsets { uvec4 base_words; } offsets;
void main() {
    gl_Position = vec4(0.0, 0.0, 0.0, 1.0);
    tf_a.words[offsets.base_words.x] = floatBitsToUint(a0);
    tf_b.words[offsets.base_words.y] = floatBitsToUint(a12);
}
"#;

    let mut gpu = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut session = new_session(&gpu);
    let first = 3.25f32;
    let last = -19.5f32;
    let mut commands = vec![Cmd::CreateTexture(
        1,
        tex2d(1, 1, texture_usage::RENDER_TARGET),
    )];
    for slot in 0..13u32 {
        let id = 10 + slot;
        let value = if slot == 0 {
            first
        } else if slot == 12 {
            last
        } else {
            slot as f32
        };
        commands.extend([
            Cmd::CreateBuffer(
                id,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::VERTEX,
                    label: format!("wide-vb-{slot}"),
                },
            ),
            Cmd::WriteBuffer {
                id,
                offset: 0,
                data: value.to_le_bytes().to_vec(),
            },
        ]);
    }
    for (id, label) in [(100, "output-a"), (101, "output-b")] {
        commands.push(Cmd::CreateBuffer(
            id,
            BufferDesc {
                size: 4,
                usage: buffer_usage::STORAGE | buffer_usage::COPY_SRC,
                label: label.into(),
            },
        ));
    }
    commands.extend([
        Cmd::CreateBuffer(
            102,
            BufferDesc {
                size: 16,
                usage: buffer_usage::UNIFORM,
                label: "offsets".into(),
            },
        ),
        Cmd::WriteBuffer {
            id: 102,
            offset: 0,
            data: vec![0; 16],
        },
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::VERTEX, "vmain", WIDE_VS),
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
                vertex_buffers: (0..13)
                    .map(|location| VertexLayout {
                        stride: 4,
                        step_mode: 0,
                        attrs: vec![VertexAttr {
                            location,
                            format: 0x01,
                            offset: 0,
                        }],
                    })
                    .collect(),
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0,
                }],
                depth: None,
                topology: Topology::PointList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: "wide-tf".into(),
            },
        ),
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry {
                        binding: 64,
                        resource: BindResource::Buffer {
                            id: 100,
                            offset: 0,
                            size: 4,
                        },
                    },
                    BindEntry {
                        binding: 65,
                        resource: BindResource::Buffer {
                            id: 101,
                            offset: 0,
                            size: 4,
                        },
                    },
                    BindEntry {
                        binding: 68,
                        resource: BindResource::Buffer {
                            id: 102,
                            offset: 0,
                            size: 16,
                        },
                    },
                ],
            },
        ),
    ]);
    let mut encoder = vec![
        Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: 1,
                load: LoadOp::Clear,
                clear: [0.0; 4],
                store: true,
            }],
            depth: None,
        },
        Enc::SetPipeline(1),
        Enc::SetBindGroup { index: 0, group: 1 },
    ];
    encoder.extend((0..13u32).map(|slot| Enc::SetVertexBuffer {
        slot,
        buffer: 10 + slot,
        offset: 0,
    }));
    encoder.extend([
        Enc::Draw {
            vertex_count: 1,
            instance_count: 1,
            first_vertex: 0,
            first_instance: 0,
        },
        Enc::EndRenderPass,
    ]);
    commands.push(Cmd::Submit(CommandBuffer {
        encoder,
        signal: None,
    }));

    hl_gpu::runtime::submit(&mut session, &mut gpu, 0, &commands).expect("wide capture draw");
    assert_eq!(
        gpu.read_buffer(&session.resources, hl_gpu::BufferId(100), 0, 4)
            .expect("first readback"),
        first.to_le_bytes()
    );
    assert_eq!(
        gpu.read_buffer(&session.resources, hl_gpu::BufferId(101), 0, 4)
            .expect("last readback"),
        last.to_le_bytes()
    );
}
