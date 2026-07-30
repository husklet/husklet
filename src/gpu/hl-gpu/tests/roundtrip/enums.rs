use super::*;

/// Round-trip one command stream through the codec and assert it comes back unchanged.
fn rt(cmds: Vec<Cmd>) {
    assert_eq!(
        hl_gpu::Decoder::stream(&hl_gpu::Encoder::stream(&cmds)).unwrap(),
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
                    ..SamplerDesc::default()
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
