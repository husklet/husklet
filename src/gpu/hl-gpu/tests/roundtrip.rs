//! Round-trip + malformed-input tests for the protocol codec: `decode(encode(x)) == x` for a
//! representative set of command streams, per-command frame round-trips, and clean typed rejection of
//! truncated / bogus / trailing-byte inputs.

use hl_gpu::protocol::codec::wire::{Decoder, Encoder};
use hl_gpu::protocol::model::command::*;
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::GpuError;

/// A representative stream touching every command family: buffer create/write, texture, sampler, both
/// shader kinds, render + compute pipelines, bind group, surface, fence, a Submit command buffer with a
/// clear + a texture-to-texture copy + a draw, a wait, a present, and destroys.
fn representative_stream() -> Vec<Cmd> {
    vec![
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 256,
                usage: buffer_usage::VERTEX | buffer_usage::COPY_DST,
                label: "vb".into(),
            },
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vec![1, 2, 3, 4, 5, 6, 7, 8],
        },
        Cmd::CreateTexture(
            2,
            TextureDesc {
                width: 64,
                height: 32,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Bgra8Unorm,
                usage: texture_usage::RENDER_TARGET | texture_usage::PRESENT,
                label: "rt".into(),
            },
        ),
        Cmd::CreateTexture(
            9,
            TextureDesc {
                width: 64,
                height: 32,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Bgra8Unorm,
                usage: texture_usage::COPY_DST,
                label: "rt2".into(),
            },
        ),
        Cmd::CreateSampler(
            10,
            SamplerDesc {
                min_filter: Filter::Linear,
                mag_filter: Filter::Nearest,
                mip_filter: Filter::Linear,
                address_u: AddressMode::Repeat,
                address_v: AddressMode::ClampToEdge,
                address_w: AddressMode::MirrorRepeat,
            },
        ),
        Cmd::CreateShader {
            id: 3,
            kind: ShaderPayloadKind::SpirV,
            spirv: vec![0x0723_0203, 0x0001_0000, 42, 7],
        },
        Cmd::CreateShader {
            id: 4,
            kind: ShaderPayloadKind::SpirV,
            spirv: vec![0x0723_0203, 0x0001_0000, 99],
        },
        // A payload with neither magic classifies as Msl on decode.
        Cmd::CreateShader {
            id: 11,
            kind: ShaderPayloadKind::Msl,
            spirv: vec![0x4141_4141, 0x4242_4242],
        },
        Cmd::CreateRenderPipeline(
            5,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 3,
                    entry: "vs_main".into(),
                },
                fragment: Some(ShaderRef {
                    module: 4,
                    entry: "fs_main".into(),
                }),
                vertex_buffers: vec![VertexLayout {
                    stride: 16,
                    step_mode: 0,
                    attrs: vec![VertexAttr {
                        location: 0,
                        format: 23,
                        offset: 0,
                    }],
                }],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Bgra8Unorm,
                    blend: Some(BlendState {
                        src_color: 1,
                        dst_color: 0,
                        op_color: 0,
                        src_alpha: 1,
                        dst_alpha: 0,
                        op_alpha: 0,
                    }),
                    write_mask: 0xF,
                }],
                depth: Some(DepthState::depth_only(TextureFormat::Depth32Float, true, 2)),
                topology: Topology::TriangleList,
                cull: 2,
                front_face: 0,
                sample_count: 1,
                label: "pipe".into(),
            },
        ),
        Cmd::CreateComputePipeline(
            12,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 3,
                    entry: "cs_main".into(),
                },
                label: "cpipe".into(),
            },
        ),
        Cmd::CreateBindGroup(
            6,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 256,
                        },
                    },
                    BindEntry {
                        binding: 1,
                        resource: BindResource::Texture { id: 2 },
                    },
                    BindEntry {
                        binding: 2,
                        resource: BindResource::Sampler { id: 10 },
                    },
                ],
            },
        ),
        Cmd::CreateSurface(
            7,
            SurfaceDesc {
                width: 64,
                height: 32,
                format: TextureFormat::Bgra8Unorm,
                hlp_surface: 100,
            },
        ),
        Cmd::CreateFence(8),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 2,
                        load: LoadOp::Clear,
                        clear: [0.1, 0.2, 0.3, 1.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::SetViewport {
                    x: 0.0,
                    y: 0.0,
                    w: 64.0,
                    h: 32.0,
                    min_depth: 0.0,
                    max_depth: 1.0,
                },
                Enc::SetScissor {
                    x: 0,
                    y: 0,
                    w: 64,
                    h: 32,
                },
                Enc::SetPipeline(5),
                Enc::SetBindGroup { index: 0, group: 6 },
                Enc::SetVertexBuffer {
                    slot: 0,
                    buffer: 1,
                    offset: 0,
                },
                Enc::SetIndexBuffer {
                    buffer: 1,
                    offset: 0,
                    format: IndexFormat::U32,
                },
                Enc::Draw {
                    vertex_count: 3,
                    instance_count: 1,
                    first_vertex: 0,
                    first_instance: 0,
                },
                Enc::DrawIndexed {
                    index_count: 3,
                    instance_count: 1,
                    first_index: 0,
                    base_vertex: 0,
                    first_instance: 0,
                },
                Enc::EndRenderPass,
                Enc::CopyTextureToTexture {
                    src: 2,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    dst: 9,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 64,
                        height: 32,
                        depth: 1,
                    },
                },
                Enc::BlitTexture {
                    src: 2,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 64,
                        height: 32,
                        depth: 1,
                    },
                    dst: 9,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 32,
                        height: 16,
                        depth: 1,
                    },
                    filter: Filter::Linear,
                },
                Enc::ResolveTexture {
                    src: 2,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    dst: 9,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: 64,
                        height: 32,
                        depth: 1,
                    },
                },
                Enc::BeginComputePass,
                Enc::Dispatch { x: 8, y: 1, z: 1 },
                Enc::EndComputePass,
                Enc::CopyBufferToBuffer {
                    src: 1,
                    src_offset: 0,
                    dst: 1,
                    dst_offset: 8,
                    size: 4,
                },
                Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: 256,
                    dst: 2,
                    mip: 0,
                    width: 64,
                    height: 32,
                },
                Enc::CopyTextureToBuffer {
                    src: 2,
                    mip: 0,
                    width: 64,
                    height: 32,
                    dst: 1,
                    dst_offset: 0,
                    bytes_per_row: 256,
                },
                Enc::ClearRect {
                    texture: 2,
                    x: 0,
                    y: 0,
                    w: 8,
                    h: 8,
                    color: [1.0, 0.0, 0.0, 1.0],
                },
            ],
            signal: Some((8, 1)),
        }),
        Cmd::WaitFence { id: 8, value: 1 },
        Cmd::Present {
            surface: 7,
            texture: 2,
        },
        Cmd::DestroyBindGroup(6),
        Cmd::DestroyPipeline(5),
        Cmd::DestroyPipeline(12),
        Cmd::DestroyShader(3),
        Cmd::DestroyShader(4),
        Cmd::DestroyShader(11),
        Cmd::DestroySampler(10),
        Cmd::DestroySurface(7),
        Cmd::DestroyTexture(2),
        Cmd::DestroyTexture(9),
        Cmd::DestroyFence(8),
        Cmd::DestroyBuffer(1),
    ]
}

#[path = "roundtrip/enums.rs"]
mod enums;
#[path = "roundtrip/shader.rs"]
mod shader;
#[path = "roundtrip/stream.rs"]
mod stream;
