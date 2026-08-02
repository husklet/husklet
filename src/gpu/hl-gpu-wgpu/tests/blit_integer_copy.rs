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

fn attempt_with_usage(src_usage: u32, dst_usage: u32) -> hl_gpu::Result<()> {
    let format = TextureFormat::Rgba8Uint;
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut src = texture(format);
    src.width = 1;
    src.height = 1;
    src.usage = src_usage;
    let mut dst = src.clone();
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
                    filter: Filter::Nearest,
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
    attempt_with_usage(both, both).expect("positive control with both declared usages");

    let source_error = attempt_with_usage(texture_usage::COPY_DST, both)
        .expect_err("missing source COPY_SRC must be refused");
    assert!(
        matches!(source_error, hl_gpu::GpuError::Invalid(message) if message.contains("source lacks COPY_SRC")),
        "unexpected source refusal: {source_error:?}"
    );

    let destination_error = attempt_with_usage(both, texture_usage::COPY_SRC)
        .expect_err("missing destination COPY_DST must be refused");
    assert!(
        matches!(destination_error, hl_gpu::GpuError::Invalid(message) if message.contains("destination lacks COPY_DST")),
        "unexpected destination refusal: {destination_error:?}"
    );
}
