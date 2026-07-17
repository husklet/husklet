//! Round-trip + malformed-input tests for the protocol codec: `decode(encode(x)) == x` for a
//! representative set of command streams, per-command frame round-trips, and clean typed rejection of
//! truncated / bogus / trailing-byte inputs.

use hl_gpu::protocol::codec::wire::{Decoder, Encoder};
use hl_gpu::protocol::model::command::*;
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::{decode_stream, encode_stream, GpuError};

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
        // A payload with neither magic classifies as LegacyMsl on decode.
        Cmd::CreateShader {
            id: 11,
            kind: ShaderPayloadKind::LegacyMsl,
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

#[test]
fn stream_round_trips_unchanged() {
    let cmds = representative_stream();
    let bytes = encode_stream(&cmds);
    let back = decode_stream(&bytes).expect("decode");
    assert_eq!(cmds, back, "stream must survive encode→decode unchanged");
}

#[test]
fn each_command_frame_round_trips() {
    for c in representative_stream() {
        let framed = c.frame();
        let mut d = Decoder::new(&framed);
        assert_eq!(
            Cmd::decode_frame(&mut d).unwrap(),
            c,
            "per-command frame round-trip"
        );
    }
}

#[test]
fn shader_payload_kind_is_reclassified_by_neutral_magic() {
    // The wire carries no kind byte; the decoder re-derives the kind from the payload's leading word
    // against the NEUTRAL magics in model::kernel (never a CUDA/PTX constant).
    use hl_gpu::protocol::model::kernel::{KernelDescriptor, KERNEL_MAGIC, SPIRV_MAGIC};
    let kd = KernelDescriptor {
        ptx: "ret;".into(),
        entry: "k".into(),
        block: [64, 1, 1],
    };
    let cmds = vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv: vec![SPIRV_MAGIC, 0, 0],
        },
        Cmd::CreateShader {
            id: 2,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kd.to_words(),
        },
        Cmd::CreateShader {
            id: 3,
            kind: ShaderPayloadKind::LegacyMsl,
            spirv: vec![0x0000_00ff, 1, 2],
        },
    ];
    let back = decode_stream(&encode_stream(&cmds)).unwrap();
    assert!(matches!(
        back[0],
        Cmd::CreateShader {
            kind: ShaderPayloadKind::SpirV,
            ..
        }
    ));
    assert!(matches!(
        back[1],
        Cmd::CreateShader {
            kind: ShaderPayloadKind::PtxKernel,
            ..
        }
    ));
    assert!(matches!(
        back[2],
        Cmd::CreateShader {
            kind: ShaderPayloadKind::LegacyMsl,
            ..
        }
    ));
    assert_eq!(
        back[1].clone(),
        cmds[1],
        "kernel payload words survive intact"
    );
    assert_eq!(KERNEL_MAGIC, 0xDD6B_0001);
    assert_eq!(SPIRV_MAGIC, 0x0723_0203);
}

#[test]
fn decode_rejects_truncation_and_bad_tags() {
    let bytes = encode_stream(&representative_stream());
    // truncate mid-stream -> contextual ShortBuffer, never a panic
    let err = decode_stream(&bytes[..bytes.len() - 3]).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("command") && m.contains("short buffer"))
    );
    // a bogus leading tag byte
    let err = decode_stream(&[250, 0, 0, 0, 0]).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("command 0") && m.contains("bad command/encoder tag 250"))
    );
}

#[test]
fn framed_command_decode_rejects_trailing_bytes() {
    let cmd = Cmd::CreateFence(1);
    let good = cmd.frame();
    let mut d = Decoder::new(&good);
    assert_eq!(Cmd::decode_frame(&mut d).unwrap(), cmd);
    // trailing garbage inside the frame body is malformed
    let mut e = Encoder::new();
    e.frame(|inner| {
        cmd.encode(inner);
        inner.u8(0xEE);
    });
    let framed = e.into_vec();
    let mut d = Decoder::new(&framed);
    assert_eq!(Cmd::decode_frame(&mut d), Err(GpuError::TrailingBytes));
}

#[test]
fn decoder_does_not_preallocate_on_bogus_counts() {
    // A CreateBindGroup claiming ~4 billion entries but with an empty body must fail cleanly, never
    // attempt a multi-gigabyte reservation first.
    let mut e = Encoder::new();
    e.u8(hl_gpu::protocol::model::command::tag::CREATE_BIND_GROUP);
    e.u32(1); // id
    e.u32(0); // set
    e.u32(0xFFFF_FFFF); // entry count = ~4 billion, no entries follow
    let bytes = e.into_vec();
    let err = decode_stream(&bytes).unwrap_err();
    assert!(matches!(&err, GpuError::Decode(m) if m.contains("short buffer")));
}

// ---------------------------------------------------------------------------------------------------
// EXHAUSTIVE enum-value codec coverage: every valid value of every wire enum round-trips byte-identical,
// both at the `to_u32`∘`from_u32` boundary (what encode/decode call) AND embedded in a real command that
// carries it on the wire. A single enum value that fails to survive is a producer/consumer desync.
// ---------------------------------------------------------------------------------------------------

/// Round-trip one command stream through the codec and assert it comes back unchanged.
fn rt(cmds: Vec<Cmd>) {
    assert_eq!(
        decode_stream(&encode_stream(&cmds)).unwrap(),
        cmds,
        "stream must round-trip: {cmds:?}"
    );
}

#[test]
fn every_enum_value_round_trips_through_to_from_u32() {
    // Every valid value of every repr-u32 wire enum survives `from_u32(to_u32(v)) == v`, and `to_u32`
    // reproduces the exact wire constant. These are the primitives the descriptor/command codec calls.
    let texture_formats = [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Bgra8Unorm,
        TextureFormat::Rgba8Srgb,
        TextureFormat::Bgra8Srgb,
        TextureFormat::R8Unorm,
        TextureFormat::Rg8Unorm,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba32Float,
        TextureFormat::R32Float,
        TextureFormat::Depth32Float,
        TextureFormat::Depth24PlusStencil8,
    ];
    for (i, f) in texture_formats.iter().enumerate() {
        assert_eq!(f.to_u32(), (i + 1) as u32);
        assert_eq!(TextureFormat::from_u32(f.to_u32()).unwrap(), *f);
    }
    for d in [
        TextureDim::D1,
        TextureDim::D2,
        TextureDim::D3,
        TextureDim::Cube,
    ] {
        assert_eq!(TextureDim::from_u32(d.to_u32()).unwrap(), d);
    }
    for f in [IndexFormat::U16, IndexFormat::U32] {
        assert_eq!(IndexFormat::from_u32(f.to_u32()).unwrap(), f);
    }
    for t in [
        Topology::PointList,
        Topology::LineList,
        Topology::LineStrip,
        Topology::TriangleList,
        Topology::TriangleStrip,
    ] {
        assert_eq!(Topology::from_u32(t.to_u32()).unwrap(), t);
    }
    for l in [LoadOp::Load, LoadOp::Clear, LoadOp::DontCare] {
        assert_eq!(LoadOp::from_u32(l.to_u32()).unwrap(), l);
    }
    for f in [Filter::Nearest, Filter::Linear] {
        assert_eq!(Filter::from_u32(f.to_u32()).unwrap(), f);
    }
    for a in [
        TextureAspect::All,
        TextureAspect::DepthOnly,
        TextureAspect::StencilOnly,
    ] {
        assert_eq!(TextureAspect::from_u32(a.to_u32()).unwrap(), a);
    }
    for m in [
        AddressMode::ClampToEdge,
        AddressMode::Repeat,
        AddressMode::MirrorRepeat,
    ] {
        assert_eq!(AddressMode::from_u32(m.to_u32()).unwrap(), m);
    }
}

#[test]
fn every_texture_format_and_dim_round_trips_in_a_create_texture() {
    // Carry each TextureFormat × each TextureDim across a real CreateTexture command. depth=1 keeps the
    // descriptor valid for every dim (Cube's 6-multiple / 1D's height==1 are runtime checks, not codec).
    let formats = [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Bgra8Unorm,
        TextureFormat::Rgba8Srgb,
        TextureFormat::Bgra8Srgb,
        TextureFormat::R8Unorm,
        TextureFormat::Rg8Unorm,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba32Float,
        TextureFormat::R32Float,
        TextureFormat::Depth32Float,
        TextureFormat::Depth24PlusStencil8,
    ];
    for fmt in formats {
        for dim in [
            TextureDim::D1,
            TextureDim::D2,
            TextureDim::D3,
            TextureDim::Cube,
        ] {
            rt(vec![Cmd::CreateTexture(
                1,
                TextureDesc {
                    width: 4,
                    height: 4,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim,
                    format: fmt,
                    usage: 0x3F,
                    label: "t".into(),
                },
            )]);
        }
    }
}

#[test]
fn every_sampler_enum_combination_round_trips() {
    // Each Filter (3 sites) and each AddressMode (3 axes) survives a real CreateSampler.
    for filter in [Filter::Nearest, Filter::Linear] {
        for addr in [
            AddressMode::ClampToEdge,
            AddressMode::Repeat,
            AddressMode::MirrorRepeat,
        ] {
            rt(vec![Cmd::CreateSampler(
                1,
                SamplerDesc {
                    min_filter: filter,
                    mag_filter: filter,
                    mip_filter: filter,
                    address_u: addr,
                    address_v: addr,
                    address_w: addr,
                },
            )]);
        }
    }
}

#[test]
fn every_encoder_enum_value_round_trips_in_its_op() {
    // Topology on the pipeline, LoadOp on the color attachment, IndexFormat on SetIndexBuffer, Filter +
    // TextureAspect on Blit/Copy — the enum-carrying encoder ops, each value exercised.
    for topo in [
        Topology::PointList,
        Topology::LineList,
        Topology::LineStrip,
        Topology::TriangleList,
        Topology::TriangleStrip,
    ] {
        rt(vec![Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 1,
                    entry: "v".into(),
                },
                fragment: None,
                vertex_buffers: vec![],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xF,
                }],
                depth: None,
                topology: topo,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        )]);
    }
    for load in [LoadOp::Load, LoadOp::Clear, LoadOp::DontCare] {
        rt(vec![Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 1,
                        load,
                        clear: [0.0; 4],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: None,
        })]);
    }
    for fmt in [IndexFormat::U16, IndexFormat::U32] {
        rt(vec![Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::SetIndexBuffer {
                buffer: 1,
                offset: 0,
                format: fmt,
            }],
            signal: None,
        })]);
    }
    for filter in [Filter::Nearest, Filter::Linear] {
        rt(vec![Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::BlitTexture {
                src: 1,
                src_sub: TextureSubresource::base(),
                src_origin: Origin3d::default(),
                src_extent: Extent3d {
                    width: 2,
                    height: 2,
                    depth: 1,
                },
                dst: 2,
                dst_sub: TextureSubresource::base(),
                dst_origin: Origin3d::default(),
                dst_extent: Extent3d {
                    width: 2,
                    height: 2,
                    depth: 1,
                },
                filter,
            }],
            signal: None,
        })]);
    }
    for aspect in [
        TextureAspect::All,
        TextureAspect::DepthOnly,
        TextureAspect::StencilOnly,
    ] {
        rt(vec![Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyTextureToTexture {
                src: 1,
                src_sub: TextureSubresource {
                    mip: 0,
                    layer: 0,
                    aspect,
                },
                src_origin: Origin3d::default(),
                dst: 2,
                dst_sub: TextureSubresource {
                    mip: 0,
                    layer: 0,
                    aspect,
                },
                dst_origin: Origin3d::default(),
                extent: Extent3d {
                    width: 1,
                    height: 1,
                    depth: 1,
                },
            }],
            signal: None,
        })]);
    }
}

#[test]
fn every_shader_payload_kind_classifies_deterministically() {
    // The wire carries NO kind byte; the decoder re-derives the kind from the payload's leading magic.
    // SpirV/PtxKernel/Glsl are magic-led (round-trip exactly); DemoBuiltin + LegacyMsl are magic-less and
    // BOTH classify as LegacyMsl on decode (documented lossy classification — no magic to distinguish).
    use hl_gpu::protocol::model::kernel::{
        glsl_stage, GlslDescriptor, KernelDescriptor, SPIRV_MAGIC,
    };
    let kd = KernelDescriptor {
        ptx: "ret;".into(),
        entry: "k".into(),
        block: [1, 1, 1],
    };
    let gd = GlslDescriptor {
        stage: glsl_stage::VERTEX,
        entry: "v".into(),
        source: "x".into(),
    };
    let magic_led = vec![
        (
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: vec![SPIRV_MAGIC, 0],
            },
            ShaderPayloadKind::SpirV,
        ),
        (
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::PtxKernel,
                spirv: kd.to_words(),
            },
            ShaderPayloadKind::PtxKernel,
        ),
        (
            Cmd::CreateShader {
                id: 3,
                kind: ShaderPayloadKind::Glsl,
                spirv: gd.to_words(),
            },
            ShaderPayloadKind::Glsl,
        ),
    ];
    for (cmd, want) in magic_led {
        let back = decode_stream(&encode_stream(&[cmd.clone()])).unwrap();
        match &back[0] {
            Cmd::CreateShader { kind, .. } => {
                assert_eq!(*kind, want, "magic-led payload must reclassify to {want:?}")
            }
            other => panic!("expected CreateShader, got {other:?}"),
        }
    }
    // DemoBuiltin (no magic) decodes as LegacyMsl — the only kinds that do not round-trip by design.
    for kind in [ShaderPayloadKind::DemoBuiltin, ShaderPayloadKind::LegacyMsl] {
        let back = decode_stream(&encode_stream(&[Cmd::CreateShader {
            id: 1,
            kind,
            spirv: vec![0x4141_4141, 0x4242_4242],
        }]))
        .unwrap();
        assert!(
            matches!(
                back[0],
                Cmd::CreateShader {
                    kind: ShaderPayloadKind::LegacyMsl,
                    ..
                }
            ),
            "magic-less payload classifies as LegacyMsl"
        );
    }
}
