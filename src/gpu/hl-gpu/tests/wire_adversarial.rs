//! EXTREME adversarial coverage for the protocol wire codec — the highest-value target, since a real
//! remote peer sends UNTRUSTED bytes. Three families of guarantees are locked in here:
//!
//! 1. **Total decode robustness**: for EVERY encoder-op (`etag`) and EVERY top-level command (`tag`),
//!    truncating the encoded bytes at every prefix, corrupting the tag, and mutating fields must return a
//!    clean typed `Err` — NEVER a panic / OOB read / UB.
//! 2. **Byte-stability**: for ANY byte string the decoder ACCEPTS, `encode(decode(bytes)) == bytes`. The
//!    decoder normalizes nothing silently; a decodable frame re-encodes to itself, byte-for-byte. A single
//!    counterexample is a real bug (a producer/consumer desync).
//! 3. **Typed rejection of every malformed shape**: unknown tag/etag → `BadTag`; out-of-range enum →
//!    `BadEnum`; non-canonical bool → `NonCanonicalBool`; non-finite render float → `NonFinite`; trailing
//!    frame bytes → `TrailingBytes`; a bogus length prefix → `ShortBuffer` (no giant prealloc).
//!
//! Complements `tests/fuzz.rs` (random/bitflip/truncation on 2 streams) and `tests/roundtrip.rs`.

use std::panic::catch_unwind;

use hl_gpu::protocol::codec::wire::{Decoder, Encoder};
use hl_gpu::protocol::model::command::{etag, tag, Cmd, CommandBuffer, Enc};
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::protocol::model::kernel::{
    glsl_stage, GlslDescriptor, KernelDescriptor, GLSL_MAGIC, KERNEL_MAGIC, SPIRV_MAGIC,
};
use hl_gpu::{GpuError, ShaderPayloadKind, WIRE_VERSION};

// ---------------------------------------------------------------------------------------------------
// exhaustive op/command inventories (one canonical value of EVERY etag and EVERY tag)
// ---------------------------------------------------------------------------------------------------

/// One canonical, well-formed value of EVERY encoder op (all 22 etags), with finite floats + canonical
/// bools so each participates in the value-round-trip guarantee.
fn every_encoder_op() -> Vec<Enc> {
    let sub = TextureSubresource::base();
    let org = Origin3d::default();
    let ext = Extent3d {
        width: 4,
        height: 4,
        depth: 1,
    };
    vec![
        Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: 2,
                load: LoadOp::Clear,
                clear: [0.0, 0.5, 1.0, 1.0],
                store: true,
            }],
            depth: Some(DepthAttachment {
                texture: 3,
                load: LoadOp::Load,
                clear_depth: 1.0,
                clear_stencil: 7,
            }),
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
            format: IndexFormat::U16,
        },
        Enc::SetViewport {
            x: 0.0,
            y: 0.0,
            w: 4.0,
            h: 4.0,
            min_depth: 0.0,
            max_depth: 1.0,
        },
        Enc::SetScissor {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        },
        Enc::ClearRect {
            texture: 2,
            x: 0,
            y: 0,
            w: 2,
            h: 2,
            color: [1.0, 0.0, 0.0, 1.0],
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
            base_vertex: -1,
            first_instance: 0,
        },
        Enc::EndRenderPass,
        Enc::BeginComputePass,
        Enc::Dispatch { x: 8, y: 1, z: 1 },
        Enc::EndComputePass,
        Enc::CopyBufferToBuffer {
            src: 1,
            src_offset: 0,
            dst: 1,
            dst_offset: 4,
            size: 4,
        },
        Enc::CopyBufferToTexture {
            src: 1,
            src_offset: 0,
            bytes_per_row: 16,
            dst: 2,
            mip: 0,
            width: 4,
            height: 4,
        },
        Enc::CopyTextureToBuffer {
            src: 2,
            mip: 0,
            width: 4,
            height: 4,
            dst: 1,
            dst_offset: 0,
            bytes_per_row: 16,
        },
        Enc::CopyBufferToTextureRegion {
            src: 1,
            src_offset: 32,
            bytes_per_row: 64,
            rows_per_image: 8,
            dst: 2,
            dst_sub: sub,
            dst_origin: org,
            extent: ext,
        },
        Enc::CopyTextureToBufferRegion {
            src: 2,
            src_sub: sub,
            src_origin: org,
            extent: ext,
            dst: 1,
            dst_offset: 48,
            bytes_per_row: 64,
            rows_per_image: 8,
        },
        Enc::CopyTextureToTexture {
            src: 2,
            src_sub: sub,
            src_origin: org,
            dst: 9,
            dst_sub: sub,
            dst_origin: org,
            extent: ext,
        },
        Enc::BlitTexture {
            src: 2,
            src_sub: sub,
            src_origin: org,
            src_extent: ext,
            dst: 9,
            dst_sub: sub,
            dst_origin: org,
            dst_extent: ext,
            filter: Filter::Linear,
        },
        Enc::ResolveTexture {
            src: 2,
            src_sub: sub,
            src_origin: org,
            dst: 9,
            dst_sub: sub,
            dst_origin: org,
            extent: ext,
        },
        Enc::FillBuffer {
            buffer: 1,
            offset: 0,
            size: 8,
            value: 0xDEAD_BEEF,
        },
        Enc::SetStencilReference {
            reference: 0x0000_00A5,
        },
        Enc::SetBlendConstant {
            color: [0.125, 0.25, 0.5, 1.0],
        },
    ]
}

/// One canonical value of EVERY top-level command (all 21 tags). Shader payloads lead with the magic that
/// matches their declared kind so each survives the value round-trip (the kind is re-derived on decode).
fn every_command() -> Vec<Cmd> {
    let kd = KernelDescriptor {
        ptx: "ret;".into(),
        entry: "k".into(),
        block: [64, 1, 1],
    };
    let gd = GlslDescriptor {
        stage: glsl_stage::FRAGMENT,
        entry: "fmain".into(),
        source: "#version 460\nvoid main(){}\n".into(),
    };
    vec![
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 256,
                usage: 0x3F,
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
                width: 4,
                height: 4,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Bgra8Unorm,
                usage: 0x3F,
                label: "rt".into(),
            },
        ),
        Cmd::CreateTextureView(
            20,
            TextureViewDesc {
                texture: 2,
                dim: TextureDim::D2,
                format: TextureFormat::Bgra8Unorm,
                aspect: TextureAspect::All,
                base_mip: 0,
                mip_count: 1,
                base_layer: 0,
                layer_count: 1,
            },
        ),
        Cmd::CreateSampler(
            3,
            SamplerDesc {
                min_filter: Filter::Linear,
                mag_filter: Filter::Nearest,
                mip_filter: Filter::Linear,
                address_u: AddressMode::Repeat,
                address_v: AddressMode::ClampToEdge,
                address_w: AddressMode::MirrorRepeat,
                ..SamplerDesc::default()
            },
        ),
        Cmd::CreateShader {
            id: 4,
            kind: ShaderPayloadKind::SpirV,
            spirv: vec![SPIRV_MAGIC, 1, 2, 3],
        },
        Cmd::CreateShader {
            id: 5,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kd.to_words(),
        },
        Cmd::CreateShader {
            id: 6,
            kind: ShaderPayloadKind::Glsl,
            spirv: gd.to_words(),
        },
        // A payload with no magic classifies as Msl.
        Cmd::CreateShader {
            id: 7,
            kind: ShaderPayloadKind::Msl,
            spirv: vec![0x4141_4141],
        },
        Cmd::CreateRenderPipeline(
            8,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 4,
                    entry: "vs".into(),
                },
                fragment: Some(ShaderRef {
                    module: 6,
                    entry: "fs".into(),
                }),
                vertex_buffers: vec![VertexLayout {
                    stride: 16,
                    step_mode: 1,
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
                depth: Some(DepthState {
                    format: TextureFormat::Depth24PlusStencil8,
                    depth_write: true,
                    depth_compare: 3,
                    stencil_front: StencilFaceState {
                        compare: compare::EQUAL,
                        fail_op: stencil_op::KEEP,
                        depth_fail_op: stencil_op::INCREMENT_CLAMP,
                        pass_op: stencil_op::REPLACE,
                    },
                    stencil_back: StencilFaceState {
                        compare: compare::NOT_EQUAL,
                        fail_op: stencil_op::INVERT,
                        depth_fail_op: stencil_op::ZERO,
                        pass_op: stencil_op::DECREMENT_WRAP,
                    },
                    stencil_read_mask: 0x0000_00FF,
                    stencil_write_mask: 0x0000_007F,
                    bias_constant: 7,
                    bias_slope_scale: 1.25,
                    bias_clamp: 0.5,
                }),
                topology: Topology::TriangleStrip,
                cull: 2,
                front_face: 1,
                sample_count: 1,
                label: "pipe".into(),
            },
        ),
        Cmd::CreateComputePipeline(
            9,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 5,
                    entry: "k".into(),
                },
                label: "cp".into(),
            },
        ),
        Cmd::CreateRenderPipelineLayout(
            18,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 4,
                    entry: "vs".into(),
                },
                fragment: None,
                vertex_buffers: Vec::new(),
                color_targets: Vec::new(),
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: "layout-render".into(),
            },
            PipelineLayout {
                bindings: vec![PipelineBinding {
                    group: 0,
                    binding: 0,
                    count: 2,
                    kind: PipelineBindingKind::UniformBuffer,
                }],
            },
            RenderMultisample {
                mask: 0x55aa,
                sample_shading: true,
            },
        ),
        Cmd::CreateComputePipelineLayout(
            19,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 5,
                    entry: "k".into(),
                },
                label: "layout-compute".into(),
            },
            PipelineLayout {
                bindings: vec![PipelineBinding {
                    group: 1,
                    binding: 3,
                    count: 4,
                    kind: PipelineBindingKind::StorageBuffer,
                }],
            },
        ),
        Cmd::CreateBindGroup(
            10,
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
                        resource: BindResource::Sampler { id: 3 },
                    },
                ],
            },
        ),
        Cmd::CreateSurface(
            11,
            SurfaceDesc {
                width: 4,
                height: 4,
                format: TextureFormat::Bgra8Unorm,
                token: hl_gpu::SurfaceToken::new(100).unwrap(),
            },
        ),
        Cmd::CreateFence(12),
        Cmd::Submit(CommandBuffer {
            encoder: every_encoder_op(),
            signal: Some((12, 7)),
        }),
        Cmd::WaitFence { id: 12, value: 7 },
        Cmd::Present {
            surface: 11,
            texture: 2,
            serial: hl_gpu::FrameSerial::new(13).unwrap(),
        },
        Cmd::DestroyBindGroup(10),
        Cmd::DestroyPipeline(8),
        Cmd::DestroyShader(4),
        Cmd::DestroySampler(3),
        Cmd::DestroySurface(11),
        Cmd::DestroyTextureView(20),
        Cmd::DestroyTexture(2),
        Cmd::DestroyFence(12),
        Cmd::DestroyBuffer(1),
    ]
}

fn no_panic(bytes: &[u8]) -> hl_gpu::Result<Vec<Cmd>> {
    let owned = bytes.to_vec();
    match catch_unwind(move || hl_gpu::Decoder::stream(&owned)) {
        Ok(r) => r,
        Err(_) => panic!(
            "decode_stream PANICKED on {} bytes: {:02x?}",
            bytes.len(),
            bytes
        ),
    }
}

// ---------------------------------------------------------------------------------------------------
// 1. every op / command round-trips, and truncating at EVERY prefix never panics
// ---------------------------------------------------------------------------------------------------

#[path = "wire_adversarial/compatibility.rs"]
mod compatibility;
#[path = "wire_adversarial/mutation.rs"]
mod mutation;
#[path = "wire_adversarial/roundtrip.rs"]
mod roundtrip;
#[path = "wire_adversarial/validation.rs"]
mod validation;
