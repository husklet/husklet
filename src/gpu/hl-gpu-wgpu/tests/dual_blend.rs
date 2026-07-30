//! Pixel proof for the four dual-source blend factors used by ANGLE.
//!
//! The fragment shader emits two colors at location zero. The fixed-function
//! blend unit must use the second output for `SRC1_*`; silently lowering any
//! of these factors to `ONE` produces a different pixel.

use hl_gpu::protocol::model::descriptor::{
    BlendState, ColorAttachment, ColorTargetState, RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    blend_factor, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const WIDTH: u32 = 4;
const HEIGHT: u32 = 4;
const SOURCE: [f32; 4] = [0.8, 0.6, 0.4, 0.5];
const SOURCE1: [f32; 4] = [0.25, 0.4, 0.75, 0.6];

const VERTEX: &str = r#"#version 460
void main() {
    vec2 positions[3] = vec2[3](
        vec2(-1.0, -1.0),
        vec2(3.0, -1.0),
        vec2(-1.0, 3.0)
    );
    gl_Position = vec4(positions[gl_VertexIndex], 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"#version 320 es
precision highp float;
layout(location = 0, index = 0) out vec4 source;
layout(location = 0, index = 1) out vec4 source1;
void main() {
    source = vec4(0.8, 0.6, 0.4, 0.5);
    source1 = vec4(0.25, 0.4, 0.75, 0.6);
}
"#;

fn shader(stage: u32, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: "main".to_owned(),
        source: source.to_owned(),
    }
    .to_words()
}

fn session(executor: &WgpuExecutor) -> Session {
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

fn draw(executor: &mut WgpuExecutor, factor: u32) -> [u8; 4] {
    let mut session = session(executor);
    let texture = TextureDesc {
        width: WIDTH,
        height: HEIGHT,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: "dual-source-target".to_owned(),
    };
    let blend = BlendState {
        src_color: factor,
        dst_color: blend_factor::ZERO,
        op_color: 0,
        src_alpha: blend_factor::ONE,
        dst_alpha: blend_factor::ZERO,
        op_alpha: 0,
    };

    hl_gpu::runtime::submit(
        &mut session,
        executor,
        0,
        &[
            Cmd::CreateTexture(1, texture),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: shader(glsl_stage::VERTEX, VERTEX),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: shader(glsl_stage::FRAGMENT, FRAGMENT),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "main".to_owned(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "main".to_owned(),
                    }),
                    vertex_buffers: Vec::new(),
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: Some(blend),
                        write_mask: 0xf,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: "dual-source-pipeline".to_owned(),
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
    .expect("dual-source draw");

    let pixels = executor
        .read_texture(&session.resources, 1)
        .expect("readback");
    let center = ((HEIGHT / 2 * WIDTH + WIDTH / 2) * 4) as usize;
    pixels[center..center + 4].try_into().unwrap()
}

fn quantize(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[test]
fn source1_factors_use_the_second_fragment_output() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let cases = [
        (
            blend_factor::SRC1_COLOR,
            [SOURCE1[0], SOURCE1[1], SOURCE1[2], SOURCE1[3]],
        ),
        (
            blend_factor::ONE_MINUS_SRC1_COLOR,
            [
                1.0 - SOURCE1[0],
                1.0 - SOURCE1[1],
                1.0 - SOURCE1[2],
                1.0 - SOURCE1[3],
            ],
        ),
        (
            blend_factor::SRC1_ALPHA,
            [SOURCE1[3], SOURCE1[3], SOURCE1[3], SOURCE1[3]],
        ),
        (blend_factor::ONE_MINUS_SRC1_ALPHA, [1.0 - SOURCE1[3]; 4]),
    ];

    for (factor, multiplier) in cases {
        let actual = draw(&mut executor, factor);
        let expected = [
            quantize(SOURCE[0] * multiplier[0]),
            quantize(SOURCE[1] * multiplier[1]),
            quantize(SOURCE[2] * multiplier[2]),
            quantize(SOURCE[3]),
        ];
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(actual, expected)| (*actual as i16 - expected as i16).abs() <= 2),
            "factor {factor}: expected {expected:?}, got {actual:?}"
        );
    }
}
