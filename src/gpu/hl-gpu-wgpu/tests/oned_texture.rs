//! DEMO — 1D texture texel selection: sample a 1D texture and assert the selected texel.
//!
//! A mipmapped `TextureDim::D1` materializes as a one-row 2D texture, while shader lowering preserves
//! Vulkan's scalar 1D sampling contract. A 2-texel 1D texture gets two distinct colors; sampling `u = 0.25` lands on texel 0 and
//! `u = 0.75` on texel 1 (NEAREST), so each readback must equal that texel's exact color.

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, Extent3d, Origin3d,
    RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc, TextureSubresource, TextureViewDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureAspect, TextureDim,
    TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{BufferId, Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
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
// glslang 16.3.0 output for a fragment shader that passes `texture1D` and `sampler` into a helper before
// sampling. Naga's own SPIR-V writer inlines that helper, so an external payload is required to preserve
// the OpFunctionParameter path this regression test exists to drive.
const FS: &[u32] = &[
    119734787, 65536, 524299, 43, 0, 131089, 1, 131089, 43, 393227, 1, 1280527431, 1685353262,
    808793134, 0, 196622, 0, 1, 393231, 4, 4, 1852399981, 0, 28, 196624, 4, 7, 196611, 2, 460,
    262149, 4, 1852399981, 0, 458757, 17, 1886152040, 1948807781, 1882927409, 828783409, 59,
    262149, 14, 1734438249, 101, 262149, 15, 1952543859, 101, 196613, 16, 120, 196613, 28, 111,
    196613, 29, 116, 196613, 30, 115, 196613, 31, 85, 262150, 31, 0, 99, 196613, 33, 117, 262149,
    36, 1634886000, 109, 262215, 28, 30, 0, 262215, 29, 33, 1, 262215, 29, 34, 0, 262215, 30, 33,
    2, 262215, 30, 34, 0, 196679, 31, 2, 327752, 31, 0, 35, 0, 262215, 33, 33, 0, 262215, 33, 34,
    0, 131091, 2, 196641, 3, 2, 196630, 6, 32, 589849, 7, 6, 0, 0, 0, 0, 1, 0, 262176, 8, 0, 7,
    131098, 9, 262176, 10, 0, 9, 262176, 11, 7, 6, 262167, 12, 6, 4, 393249, 13, 12, 8, 10, 11,
    196635, 21, 7, 262176, 27, 3, 12, 262203, 27, 28, 3, 262203, 8, 29, 0, 262203, 10, 30, 0,
    196638, 31, 12, 262176, 32, 2, 31, 262203, 32, 33, 2, 262165, 34, 32, 1, 262187, 34, 35, 0,
    262165, 37, 32, 0, 262187, 37, 38, 0, 262176, 39, 2, 6, 327734, 2, 4, 0, 3, 131320, 5, 262203,
    11, 36, 7, 393281, 39, 40, 33, 35, 38, 262205, 6, 41, 40, 196670, 36, 41, 458809, 12, 42, 17,
    29, 30, 36, 196670, 28, 42, 65789, 65592, 327734, 12, 17, 0, 13, 196663, 8, 14, 196663, 10, 15,
    196663, 11, 16, 131320, 18, 262205, 7, 19, 14, 262205, 9, 20, 15, 327766, 21, 22, 19, 20,
    262205, 6, 23, 16, 327767, 12, 24, 22, 23, 131326, 24, 65592,
];

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
    let fragment = FS.to_vec();
    assert!(
        spirv_instructions(&fragment)
            .any(|(opcode, operands)| opcode == 25 && operands.get(2) == Some(&0)),
        "control must contain OpTypeImage Dim1D"
    );
    assert!(
        spirv_instructions(&fragment).any(|(opcode, _)| matches!(opcode, 87 | 88)),
        "control must execute OpImageSampleImplicitLod or OpImageSampleExplicitLod"
    );
    assert!(
        spirv_instructions(&fragment).any(|(opcode, _)| opcode == 55)
            && spirv_instructions(&fragment).any(|(opcode, _)| opcode == 57),
        "control must retain OpFunctionParameter and OpFunctionCall"
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
                        entry: "main".into(),
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

#[test]
fn a_layer_view_of_a_1d_array_is_a_color_attachment() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let mut session = new_session(&exec);
    let mut array = tex1d(1, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC);
    array.depth = 2;
    array.mip_levels = 1;

    hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, array),
            Cmd::CreateTextureView(
                2,
                TextureViewDesc {
                    texture: 1,
                    dim: TextureDim::D1,
                    format: TextureFormat::Rgba8Unorm,
                    aspect: TextureAspect::All,
                    base_mip: 0,
                    mip_count: 1,
                    base_layer: 1,
                    layer_count: 1,
                },
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 2,
                            load: LoadOp::Clear,
                            clear: [1.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::EndRenderPass,
                    Enc::CopyTextureToBufferRegion {
                        src: 1,
                        src_sub: TextureSubresource {
                            mip: 0,
                            layer: 1,
                            aspect: TextureAspect::All,
                        },
                        src_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                        dst: 1,
                        dst_offset: 0,
                        bytes_per_row: 4,
                        rows_per_image: 1,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("a single-layer view of an emulated 1D array must render");

    assert_eq!(
        exec.read_buffer(&session.resources, BufferId(1), 0, 4)
            .unwrap(),
        [255, 0, 0, 255],
        "the render must land in the selected non-base array layer"
    );
}
