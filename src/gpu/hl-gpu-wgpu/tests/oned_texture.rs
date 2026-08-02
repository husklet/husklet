//! DEMO — 1D texture texel selection: sample a 1D texture and assert the selected texel.
//!
//! A mipmapped `TextureDim::D1` materializes as a one-row 2D texture, while shader lowering preserves
//! Vulkan's scalar 1D sampling contract. A 2-texel 1D texture gets two distinct colors; sampling `u = 0.25` lands on texel 0 and
//! `u = 0.75` on texel 1 (NEAREST), so each readback must equal that texel's exact color.

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const TEXELS: [[u8; 4]; 2] = [
    [230, 60, 10, 255], // texel 0
    [10, 60, 230, 255], // texel 1
];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U { vec4 c; } u;
layout(set = 0, binding = 1) uniform texture1D t;
layout(set = 0, binding = 2) uniform sampler   s;
layout(location = 0) out vec4 o;
void main() { o = texture(sampler1D(t, s), u.c.x); }
"#;

fn glsl_to_spirv(src: &str, stage: naga::ShaderStage, entry: &str) -> Vec<u32> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let mut module = frontend
        .parse(&naga::front::glsl::Options::from(stage), src)
        .expect("seed GLSL parses");
    module.entry_points[0].name = entry.into();
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed GLSL validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit SPIR-V")
}

fn spirv_instructions(words: &[u32]) -> impl Iterator<Item = (u16, &[u32])> {
    let mut offset = 5;
    std::iter::from_fn(move || {
        let first = *words.get(offset)?;
        let count = (first >> 16) as usize;
        let opcode = first as u16;
        let operands = words.get(offset + 1..offset + count)?;
        offset += count;
        Some((opcode, operands))
    })
}

fn nearest() -> SamplerDesc {
    SamplerDesc {
        min_filter: Filter::Nearest,
        mag_filter: Filter::Nearest,
        mip_filter: Filter::Nearest,
        address_u: AddressMode::ClampToEdge,
        address_v: AddressMode::ClampToEdge,
        address_w: AddressMode::ClampToEdge,
        ..SamplerDesc::default()
    }
}

/// A `width`×1 `Rgba8Unorm` 1D texture descriptor.
fn tex1d(width: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width,
        height: 1,
        depth: 1,
        mip_levels: 2,
        sample_count: 1,
        dim: TextureDim::D1,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

/// Sample the 1D texture at `coord`; return the single readback pixel.
fn sample_1d(exec: &mut WgpuExecutor, coord: f32) -> [u8; 4] {
    let mut s = new_session(exec);
    let row: Vec<u8> = TEXELS.iter().flatten().copied().collect(); // 2 texels
    let fragment = glsl_to_spirv(FS, naga::ShaderStage::Fragment, "fmain");
    assert!(
        spirv_instructions(&fragment)
            .any(|(opcode, operands)| opcode == 25 && operands.get(2) == Some(&0)),
        "control must contain OpTypeImage Dim1D"
    );
    assert!(
        spirv_instructions(&fragment).any(|(opcode, _)| matches!(opcode, 87 | 88)),
        "control must execute OpImageSampleImplicitLod or OpImageSampleExplicitLod"
    );

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(1, 1, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateTexture(
                2,
                tex1d(2, texture_usage::SAMPLED | texture_usage::COPY_DST),
            ),
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
                data: le_f32(&[coord, 0.0, 0.0, 0.0]),
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 8,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: row,
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::SpirV,
                spirv: fragment,
            },
            Cmd::CreateSampler(1, nearest()),
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
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 16,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Sampler { id: 1 },
                        },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 2,
                        src_offset: 0,
                        bytes_per_row: 8,
                        dst: 2,
                        mip: 0,
                        width: 2,
                        height: 1,
                    },
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
    .expect("the 1D texel sample draw must run cleanly");

    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

#[test]
fn sampling_a_1d_texel_returns_that_texel() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    for (texel, &want) in TEXELS.iter().enumerate() {
        let coord = (texel as f32 + 0.5) / 2.0; // 0.25 -> texel 0, 0.75 -> texel 1
        let got = sample_1d(&mut exec, coord);
        write_png(&format!("oned_texel{texel}"), 1, 1, &got);
        assert!(
            near(got, want),
            "texel {texel} (u={coord}): must sample {want:?}, got {got:?}"
        );
    }
    eprintln!("demo `oned_texture`: both 1D texels sampled to their exact texels");
}
