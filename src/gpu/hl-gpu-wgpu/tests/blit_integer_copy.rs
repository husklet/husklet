//! Exact-copy first slice of integer `BlitTexture` support.

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, Extent3d, Mirror, Origin3d, TextureDesc, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, Filter, TextureDim, TextureFormat,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const FORMATS: &[TextureFormat] = &[
    TextureFormat::R8Uint,
    TextureFormat::R8Sint,
    TextureFormat::Rg8Uint,
    TextureFormat::Rg8Sint,
    TextureFormat::Rgba8Uint,
    TextureFormat::Rgba8Sint,
];

fn texture(format: TextureFormat) -> TextureDesc {
    TextureDesc {
        width: 3,
        height: 2,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format,
        usage: texture_usage::COPY_SRC | texture_usage::COPY_DST,
        label: String::new(),
    }
}

fn source(format: TextureFormat) -> Vec<u8> {
    let channels = format.bytes_per_texel().expect("integer texel width");
    (0..6)
        .flat_map(|texel| {
            (0..channels).map(move |channel| {
                // Includes values above i8::MAX. Signed formats must preserve their raw two's-complement
                // bytes rather than normalize or saturate them.
                17u8.wrapping_add(texel * 37)
                    .wrapping_add(channel as u8 * 61)
            })
        })
        .collect()
}

fn run(format: TextureFormat) -> Vec<u8> {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let bytes = source(format);
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, texture(format)),
            Cmd::CreateTexture(2, texture(format)),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: bytes.len() as u64,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: bytes,
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTextureRegion {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 3 * format.bytes_per_texel().unwrap() as u32,
                        rows_per_image: 2,
                        dst: 1,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 3,
                            height: 2,
                            depth: 1,
                        },
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: 3,
                            height: 2,
                            depth: 1,
                        },
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: 3,
                            height: 2,
                            depth: 1,
                        },
                        filter: Filter::Nearest,
                        mirror: Mirror::NONE,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("same-format integer 1:1 blit");
    executor
        .read_texture(&session.resources, 2)
        .expect("read integer destination")
}

#[test]
fn same_format_integer_blit_preserves_exact_raw_texels() {
    for &format in FORMATS {
        assert_eq!(run(format), source(format), "{format:?}");
    }
}

#[test]
fn same_format_integer_d3_blit_copies_each_slice_exactly() {
    for &format in FORMATS {
        let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
        let mut limits = Limits::from_capabilities(executor.capabilities());
        limits.copy_alignment = 1;
        let mut session = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        let channels = format.bytes_per_texel().unwrap();
        let bytes: Vec<u8> = (0..24 * channels)
            .map(|i| 13u8.wrapping_add((i as u8).wrapping_mul(29)))
            .collect();
        let sentinel = vec![0xA5; bytes.len()];
        let mut desc = texture(format);
        desc.depth = 4;
        desc.dim = TextureDim::D3;
        let extent = Extent3d {
            width: 3,
            height: 2,
            depth: 2,
        };
        hl_gpu::runtime::submit(
            &mut session,
            &mut executor,
            0,
            &[
                Cmd::CreateTexture(1, desc.clone()),
                Cmd::CreateTexture(2, desc),
                Cmd::CreateBuffer(
                    1,
                    BufferDesc {
                        size: bytes.len() as u64,
                        usage: buffer_usage::COPY_SRC,
                        label: String::new(),
                    },
                ),
                Cmd::WriteBuffer {
                    id: 1,
                    offset: 0,
                    data: bytes.clone(),
                },
                Cmd::CreateBuffer(
                    3,
                    BufferDesc {
                        size: sentinel.len() as u64,
                        usage: buffer_usage::COPY_SRC,
                        label: String::new(),
                    },
                ),
                Cmd::WriteBuffer {
                    id: 3,
                    offset: 0,
                    data: sentinel.clone(),
                },
                Cmd::CreateBuffer(
                    2,
                    BufferDesc {
                        size: bytes.len() as u64,
                        usage: buffer_usage::COPY_DST,
                        label: String::new(),
                    },
                ),
                Cmd::Submit(CommandBuffer {
                    encoder: vec![
                        Enc::CopyBufferToTextureRegion {
                            src: 1,
                            src_offset: 0,
                            bytes_per_row: (3 * channels) as u32,
                            rows_per_image: 2,
                            dst: 1,
                            dst_sub: TextureSubresource::base(),
                            dst_origin: Origin3d::default(),
                            extent: Extent3d { depth: 4, ..extent },
                        },
                        Enc::CopyBufferToTextureRegion {
                            src: 3,
                            src_offset: 0,
                            bytes_per_row: (3 * channels) as u32,
                            rows_per_image: 2,
                            dst: 2,
                            dst_sub: TextureSubresource::base(),
                            dst_origin: Origin3d::default(),
                            extent: Extent3d { depth: 4, ..extent },
                        },
                        Enc::BlitTexture {
                            src: 1,
                            src_sub: TextureSubresource::base(),
                            src_origin: Origin3d { x: 0, y: 0, z: 1 },
                            src_extent: extent,
                            dst: 2,
                            dst_sub: TextureSubresource::base(),
                            dst_origin: Origin3d { x: 0, y: 0, z: 2 },
                            dst_extent: extent,
                            filter: Filter::Nearest,
                            mirror: Mirror::NONE,
                        },
                        Enc::CopyTextureToBufferRegion {
                            src: 2,
                            src_sub: TextureSubresource::base(),
                            src_origin: Origin3d::default(),
                            extent: Extent3d { depth: 4, ..extent },
                            dst: 2,
                            dst_offset: 0,
                            bytes_per_row: (3 * channels) as u32,
                            rows_per_image: 2,
                        },
                    ],
                    signal: None,
                }),
            ],
        )
        .expect("same-format integer D3 blit");
        let result = executor
            .read_buffer(&session.resources, BufferId(2), 0, bytes.len())
            .expect("read D3 destination");
        let plane = 6 * channels;
        let mut expected = sentinel;
        expected[2 * plane..4 * plane].copy_from_slice(&bytes[plane..3 * plane]);
        assert_eq!(result, expected, "{format:?}");
    }
}

#[test]
fn integer_blit_observes_a_pending_native_upload() {
    const WIDTH: u32 = 64;
    let format = TextureFormat::Rgba8Uint;
    let bytes: Vec<u8> = (0..WIDTH * 4)
        .map(|i| 7u8.wrapping_add((i as u8).wrapping_mul(43)))
        .collect();
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut desc = texture(format);
    desc.width = WIDTH;
    desc.height = 1;
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, desc.clone()),
            Cmd::CreateTexture(2, desc.clone()),
            Cmd::CreateTexture(3, desc),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: bytes.len() as u64,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: bytes.clone(),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTextureRegion {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: WIDTH * 4,
                    rows_per_image: 1,
                    dst: 1,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: WIDTH,
                        height: 1,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    // The native copy remains pending until an ordering boundary flushes it. The following
                    // standalone integer blit must see the copied bytes rather than texture 2's old contents.
                    Enc::CopyTextureToTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: WIDTH,
                            height: 1,
                            depth: 1,
                        },
                    },
                    Enc::BlitTexture {
                        src: 2,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: WIDTH,
                            height: 1,
                            depth: 1,
                        },
                        dst: 3,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: WIDTH,
                            height: 1,
                            depth: 1,
                        },
                        filter: Filter::Nearest,
                        mirror: Mirror::NONE,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("pending upload followed by integer blit");
    assert_eq!(executor.read_texture(&session.resources, 3).unwrap(), bytes);
}

fn attempt_with_usage(
    src_format: TextureFormat,
    dst_format: TextureFormat,
    src_usage: u32,
    dst_usage: u32,
    filter: Filter,
) -> hl_gpu::Result<()> {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut src = texture(src_format);
    src.width = 1;
    src.height = 1;
    src.usage = src_usage;
    let mut dst = texture(dst_format);
    dst.width = 1;
    dst.height = 1;
    dst.usage = dst_usage;
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, src),
            Cmd::CreateTexture(2, dst),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::BlitTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    dst_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    filter,
                    mirror: Mirror::NONE,
                }],
                signal: None,
            }),
        ],
    )
    .map(|_| ())
}

#[test]
fn integer_copy_requires_declared_source_and_destination_usage() {
    let both = texture_usage::COPY_SRC | texture_usage::COPY_DST;
    attempt_with_usage(
        TextureFormat::Rgba8Uint,
        TextureFormat::Rgba8Uint,
        both,
        both,
        Filter::Nearest,
    )
    .expect("positive control with both declared usages");

    let source_error = attempt_with_usage(
        TextureFormat::Rgba8Uint,
        TextureFormat::Rgba8Uint,
        texture_usage::COPY_DST,
        both,
        Filter::Nearest,
    )
    .expect_err("missing source COPY_SRC must be refused");
    assert!(
        matches!(source_error, hl_gpu::GpuError::Invalid(message) if message.contains("source lacks COPY_SRC")),
        "unexpected source refusal: {source_error:?}"
    );

    let destination_error = attempt_with_usage(
        TextureFormat::Rgba8Uint,
        TextureFormat::Rgba8Uint,
        both,
        texture_usage::COPY_SRC,
        Filter::Nearest,
    )
    .expect_err("missing destination COPY_DST must be refused");
    assert!(
        matches!(destination_error, hl_gpu::GpuError::Invalid(message) if message.contains("destination lacks COPY_DST")),
        "unexpected destination refusal: {destination_error:?}"
    );
}

#[test]
fn integer_linear_filter_is_refused_before_dispatch() {
    let both = texture_usage::COPY_SRC | texture_usage::COPY_DST;
    let error = attempt_with_usage(
        TextureFormat::Rgba8Uint,
        TextureFormat::Rgba8Uint,
        both,
        both,
        Filter::Linear,
    )
    .expect_err("integer LINEAR");
    assert!(
        matches!(error, hl_gpu::GpuError::Unsupported(message) if message.contains("linear filtering is invalid for an integer")),
        "{error:?}"
    );
}

#[test]
fn mixed_numeric_class_wins_over_filter_and_usage_errors() {
    let both = texture_usage::COPY_SRC | texture_usage::COPY_DST;
    for (dst_format, filter, src_usage, dst_usage) in [
        (TextureFormat::Rgba8Unorm, Filter::Linear, both, both),
        (
            TextureFormat::Rgba8Sint,
            Filter::Nearest,
            texture_usage::COPY_DST,
            texture_usage::COPY_SRC,
        ),
    ] {
        let error = attempt_with_usage(
            TextureFormat::Rgba8Uint,
            dst_format,
            src_usage,
            dst_usage,
            filter,
        )
        .expect_err("mixed numeric classes");
        assert!(
            matches!(error, hl_gpu::GpuError::Invalid(message) if message.contains("numeric classes differ")),
            "class mismatch must win, got {error:?}"
        );
    }
}

fn scaled(
    format_src: TextureFormat,
    format_dst: TextureFormat,
    dw: u32,
    dh: u32,
    mirror: Mirror,
) -> Vec<u8> {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let sc = format_src.bytes_per_texel().unwrap();
    let source: Vec<u8> = (0..4 * sc)
        .map(|i| 11u8.wrapping_add((i as u8).wrapping_mul(37)))
        .collect();
    let mut s = texture(format_src);
    s.width = 2;
    s.height = 2;
    let mut d = texture(format_dst);
    d.width = dw;
    d.height = dh;
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, s),
            Cmd::CreateTexture(2, d),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: source.len() as u64,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: source,
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTextureRegion {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: (2 * sc) as u32,
                        rows_per_image: 2,
                        dst: 1,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                    },
                    Enc::BlitTexture {
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
                            width: dw,
                            height: dh,
                            depth: 1,
                        },
                        filter: Filter::Nearest,
                        mirror,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("scaled integer blit");
    executor.read_texture(&session.resources, 2).unwrap()
}

#[test]
fn integer_nearest_scaling_conversion_and_xy_mirror_are_exact() {
    // R8Uint -> RGBA8Uint upscale. Missing G/B are zero and alpha is one in integer texture loads.
    assert_eq!(
        scaled(
            TextureFormat::R8Uint,
            TextureFormat::Rgba8Uint,
            4,
            4,
            Mirror::NONE
        ),
        [
            11u8, 0, 0, 1, 11, 0, 0, 1, 48, 0, 0, 1, 48, 0, 0, 1, 11, 0, 0, 1, 11, 0, 0, 1, 48, 0,
            0, 1, 48, 0, 0, 1, 85, 0, 0, 1, 85, 0, 0, 1, 122, 0, 0, 1, 122, 0, 0, 1, 85, 0, 0, 1,
            85, 0, 0, 1, 122, 0, 0, 1, 122, 0, 0, 1
        ]
    );
    // Signed RGBA -> R downscale selects the bottom-right source texel exactly.
    assert_eq!(
        scaled(
            TextureFormat::Rgba8Sint,
            TextureFormat::R8Sint,
            1,
            1,
            Mirror::NONE
        ),
        vec![199]
    );
    // Both-axis reflection reverses the 2x2 unsigned source in row-major order.
    assert_eq!(
        scaled(
            TextureFormat::R8Uint,
            TextureFormat::R8Uint,
            2,
            2,
            Mirror {
                x: true,
                y: true,
                z: false
            }
        ),
        vec![122, 85, 48, 11]
    );
}

#[test]
fn integer_mipmap_generation_reads_the_previous_level_of_the_same_texture() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut image = texture(TextureFormat::Rgba8Uint);
    image.width = 2;
    image.height = 2;
    image.depth = 6;
    image.mip_levels = 2;
    let source = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    let mip = |level| TextureSubresource {
        mip: level,
        layer: 5,
        ..TextureSubresource::base()
    };
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, image),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: source.len() as u64,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: source,
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTextureRegion {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 8,
                        rows_per_image: 2,
                        dst: 1,
                        dst_sub: mip(0),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: mip(0),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                        dst: 1,
                        dst_sub: mip(1),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                        filter: Filter::Nearest,
                        mirror: Mirror::NONE,
                    },
                    Enc::CopyTextureToBufferRegion {
                        src: 1,
                        src_sub: mip(1),
                        src_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                        dst: 2,
                        dst_offset: 0,
                        bytes_per_row: 4,
                        rows_per_image: 1,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("integer mip generation must execute");
    assert_eq!(
        executor
            .read_buffer(&session.resources, BufferId(2), 0, 4)
            .unwrap(),
        vec![13, 14, 15, 16],
    );
}

#[test]
fn integer_nonintegral_subrect_blit_preserves_destination_sentinels() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).unwrap();
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let src: Vec<u8> = (0..16).map(|i| 10 + i).collect();
    let sentinel = vec![0xE7; 16];
    let mut desc = texture(TextureFormat::R8Uint);
    desc.width = 4;
    desc.height = 4;
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, desc.clone()),
            Cmd::CreateTexture(2, desc),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src,
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: sentinel.clone(),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTextureRegion {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 4,
                        rows_per_image: 4,
                        dst: 1,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    },
                    Enc::CopyBufferToTextureRegion {
                        src: 2,
                        src_offset: 0,
                        bytes_per_row: 4,
                        rows_per_image: 4,
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d { x: 1, y: 1, z: 0 },
                        src_extent: Extent3d {
                            width: 3,
                            height: 3,
                            depth: 1,
                        },
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d { x: 1, y: 1, z: 0 },
                        dst_extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                        filter: Filter::Nearest,
                        mirror: Mirror::NONE,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .unwrap();
    let mut expected = sentinel;
    expected[5] = 15;
    expected[6] = 17;
    expected[9] = 23;
    expected[10] = 25;
    assert_eq!(
        executor.read_texture(&session.resources, 2).unwrap(),
        expected
    );
}

#[test]
fn integer_d3_scaled_blit_maps_z_mirror_per_slice() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).unwrap();
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let src = vec![10u8, 20, 30, 40, 50, 60, 70, 80];
    let mut s = texture(TextureFormat::R8Uint);
    s.width = 2;
    s.height = 2;
    s.depth = 2;
    s.dim = TextureDim::D3;
    let mut d = s.clone();
    d.width = 4;
    d.height = 4;
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, s),
            Cmd::CreateTexture(2, d),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 8,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: src,
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 32,
                    usage: buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTextureRegion {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 2,
                        rows_per_image: 2,
                        dst: 1,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 2,
                        },
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 2,
                        },
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 2,
                        },
                        filter: Filter::Nearest,
                        mirror: Mirror {
                            x: false,
                            y: false,
                            z: true,
                        },
                    },
                    Enc::CopyTextureToBufferRegion {
                        src: 2,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 2,
                        },
                        dst: 2,
                        dst_offset: 0,
                        bytes_per_row: 4,
                        rows_per_image: 4,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .unwrap();
    let expand = |p: [u8; 4]| {
        vec![
            p[0], p[0], p[1], p[1], p[0], p[0], p[1], p[1], p[2], p[2], p[3], p[3], p[2], p[2],
            p[3], p[3],
        ]
    };
    let mut expected = expand([50, 60, 70, 80]);
    expected.extend(expand([10, 20, 30, 40]));
    assert_eq!(
        executor
            .read_buffer(&session.resources, BufferId(2), 0, 32)
            .unwrap(),
        expected
    );
}
