mod gpu_harness;

use gpu_harness::glsl;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BlendState, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    blend_factor, blend_op, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat,
    Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

fn target() -> TextureDesc {
    TextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::R10x6g10x6b10x6a10x6Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
    }
}

fn pipeline(fragment: u32, blend: Option<BlendState>) -> RenderPipelineDesc {
    RenderPipelineDesc {
        vertex: ShaderRef { module: 1, entry: "vmain".into() },
        fragment: Some(ShaderRef { module: fragment, entry: "fmain".into() }),
        vertex_buffers: vec![],
        color_targets: vec![ColorTargetState {
            format: TextureFormat::R10x6g10x6b10x6a10x6Unorm,
            blend,
            write_mask: 0xf,
        }],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        sample_count: 1,
        label: String::new(),
    }
}

fn fragment(value: f64) -> String {
    format!(
        "#version 460\nlayout(location=0) out vec4 o;\nvoid main() {{ o=vec4({value},0,0,1); }}"
    )
}

#[test]
fn two_blended_draws_consume_the_ten_bit_destination() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("GPU adapter");
    let mut resources = SessionResources::default();
    let additive = BlendState {
        src_color: blend_factor::ONE,
        dst_color: blend_factor::ONE,
        op_color: blend_op::ADD,
        src_alpha: blend_factor::ONE,
        dst_alpha: blend_factor::ZERO,
        op_alpha: blend_op::ADD,
    };
    executor
        .execute(
            &mut resources,
            &[
            Cmd::CreateTexture(1, target()),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", &fragment(0.29552093575793315)),
            },
            Cmd::CreateShader {
                id: 3,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", &fragment(0.5116272819654833)),
            },
            Cmd::CreateRenderPipeline(1, pipeline(2, None)),
            Cmd::CreateRenderPipeline(2, pipeline(3, Some(additive))),
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
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::SetPipeline(2),
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
            ],
        )
        .unwrap();

    let bytes = executor.read_texture(&resources, 1).unwrap();
    assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]) >> 6, 825);
}

#[test]
fn later_pass_samples_the_ten_bit_value_not_the_shadow_precision() {
    const SAMPLE: &str = r#"#version 460
layout(set=0,binding=0) uniform texture2D t;
layout(set=0,binding=1) uniform sampler s;
layout(location=0) out vec4 o;
void main() { o=texture(sampler2D(t,s),vec2(0.5)); }
"#;
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("GPU adapter");
    let mut resources = SessionResources::default();
    let mut sampled_target = target();
    sampled_target.usage = texture_usage::RENDER_TARGET | texture_usage::SAMPLED;
    let output = TextureDesc {
        format: TextureFormat::Rgba16Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        ..target()
    };
    let sample_pipeline = RenderPipelineDesc {
        vertex: ShaderRef { module: 1, entry: "vmain".into() },
        fragment: Some(ShaderRef { module: 3, entry: "fmain".into() }),
        vertex_buffers: vec![],
        color_targets: vec![ColorTargetState {
            format: TextureFormat::Rgba16Unorm,
            blend: None,
            write_mask: 0xf,
        }],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        sample_count: 1,
        label: String::new(),
    };
    executor
        .execute(
            &mut resources,
            &[
                Cmd::CreateTexture(1, sampled_target),
                Cmd::CreateTexture(2, output),
                Cmd::CreateShader {
                    id: 1,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
                },
                Cmd::CreateShader {
                    id: 2,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: glsl(
                        glsl_stage::FRAGMENT,
                        "fmain",
                        &fragment(0.29552093575793315),
                    ),
                },
                Cmd::CreateShader {
                    id: 3,
                    kind: ShaderPayloadKind::Glsl,
                    spirv: glsl(glsl_stage::FRAGMENT, "fmain", SAMPLE),
                },
                Cmd::CreateSampler(
                    1,
                    SamplerDesc {
                        min_filter: Filter::Nearest,
                        mag_filter: Filter::Nearest,
                        mip_filter: Filter::Nearest,
                        address_u: AddressMode::ClampToEdge,
                        address_v: AddressMode::ClampToEdge,
                        address_w: AddressMode::ClampToEdge,
                        ..SamplerDesc::default()
                    },
                ),
                Cmd::CreateRenderPipeline(1, pipeline(2, None)),
                Cmd::CreateRenderPipeline(2, sample_pipeline),
                Cmd::CreateBindGroup(
                    1,
                    BindGroupDesc {
                        set: 0,
                        entries: vec![
                            BindEntry {
                                binding: 0,
                                resource: BindResource::Texture { id: 1 },
                            },
                            BindEntry {
                                binding: 1,
                                resource: BindResource::Sampler { id: 1 },
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
                        Enc::BeginRenderPass {
                            color: vec![ColorAttachment {
                                texture: 2,
                                load: LoadOp::Clear,
                                clear: [0.0, 0.0, 0.0, 1.0],
                                store: true,
                            }],
                            depth: None,
                        },
                        Enc::SetPipeline(2),
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
        .unwrap();

    let bytes = executor.read_texture(&resources, 2).unwrap();
    let stored = u16::from_le_bytes([bytes[0], bytes[1]]);
    let ten_bit = (0.29552093575793315_f64 * 1023.0).round();
    let expected = (ten_bit / 1023.0 * 65535.0).round() as u16;
    assert_eq!(stored, expected);
}
