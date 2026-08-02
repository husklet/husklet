//! Vulkan permits integer fragment outputs and color attachments to differ only in signedness. WebGPU
//! requires their numeric classes to match exactly, so the executor must specialize the fragment output at
//! pipeline creation without changing the value. This drives both directions plus an unchanged control.

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, ColorTargetState, RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp, TextureDim, TextureFormat, Topology};
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

fn spirv(source: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(source).expect("seed WGSL parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed WGSL validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("seed emits SPIR-V")
}

fn target(format: TextureFormat) -> ColorTargetState {
    ColorTargetState {
        format,
        blend: None,
        write_mask: 0xf,
    }
}

fn integer_texture(format: TextureFormat) -> TextureDesc {
    TextureDesc {
        width: 2,
        height: 2,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
    }
}

#[test]
fn vulkan_integer_outputs_convert_to_attachment_signedness() {
    let shader = spirv(
        r#"
        struct Outputs {
            @location(0) signed_to_unsigned: vec4<i32>,
            @location(1) unsigned_to_signed: vec4<u32>,
            @location(2) unsigned_control: vec4<u32>,
        }

        @vertex
        fn vs_main(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
            var positions = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
            return vec4<f32>(positions[vertex], 0.0, 1.0);
        }

        @fragment
        fn fs_main() -> Outputs {
            return Outputs(vec4<i32>(111), vec4<u32>(123), vec4<u32>(77));
        }
        "#,
    );
    let formats = [
        TextureFormat::Rgba8Uint,
        TextureFormat::Rgba8Sint,
        TextureFormat::Rgba8Uint,
    ];
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("host GPU");
    let mut session = new_session(&exec);
    let mut commands = Vec::new();
    for (index, format) in formats.into_iter().enumerate() {
        commands.push(Cmd::CreateTexture(
            index as u32 + 1,
            integer_texture(format),
        ));
    }
    commands.extend([
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv: shader.clone(),
        },
        Cmd::CreateShader {
            id: 2,
            kind: ShaderPayloadKind::SpirV,
            spirv: shader,
        },
        Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 1,
                    entry: "vs_main".into(),
                },
                fragment: Some(ShaderRef {
                    module: 2,
                    entry: "fs_main".into(),
                }),
                vertex_buffers: vec![],
                color_targets: formats.into_iter().map(target).collect(),
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
                    color: (1..=3)
                        .map(|texture| ColorAttachment {
                            texture,
                            load: LoadOp::Clear,
                            clear: [0.0; 4],
                            store: true,
                        })
                        .collect(),
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
    ]);
    hl_gpu::runtime::submit(&mut session, &mut exec, 0, &commands)
        .expect("Vulkan signedness-compatible pipeline executes");

    for (texture, expected) in [(1, 111u8), (2, 123u8), (3, 77u8)] {
        let bytes = exec.read_texture(&session.resources, texture).unwrap();
        assert!(
            bytes.iter().all(|byte| *byte == expected),
            "texture {texture} must contain {expected}, got {bytes:?}"
        );
    }
}
