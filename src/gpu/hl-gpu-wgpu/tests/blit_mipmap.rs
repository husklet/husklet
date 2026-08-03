//! A Vulkan blit names mip subresources explicitly. Advertising BLIT_SRC/BLIT_DST while refusing every
//! non-zero mip made mipmap generation fail at command-buffer completion even though both native views
//! and the software oracle can address the levels.

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, Extent3d, Mirror, Origin3d, TextureDesc, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, Filter, TextureAspect, TextureDim, TextureFormat,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuError, GpuExecutor, Limits,
    Session,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

fn texture() -> TextureDesc {
    TextureDesc {
        width: 8,
        height: 8,
        depth: 1,
        mip_levels: 4,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::SAMPLED
            | texture_usage::RENDER_TARGET
            | texture_usage::COPY_SRC
            | texture_usage::COPY_DST,
        label: String::new(),
    }
}

fn sub(mip: u32) -> TextureSubresource {
    TextureSubresource {
        mip,
        layer: 0,
        aspect: TextureAspect::All,
    }
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

fn default_alignment_session(executor: &WgpuExecutor) -> Session {
    Session::new(
        Limits::from_capabilities(executor.capabilities()),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

#[test]
fn packed_two_byte_texture_rows_cross_the_default_runtime_alignment() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut session = default_alignment_session(&executor);
    let mut descriptor = texture();
    descriptor.width = 1;
    descriptor.height = 1;
    descriptor.mip_levels = 1;
    descriptor.format = TextureFormat::R16Float;
    let half_one = [0x00, 0x3c];
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, descriptor),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 2,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 2,
                    usage: buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: half_one.to_vec(),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTextureRegion {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 2,
                        rows_per_image: 1,
                        dst: 1,
                        dst_sub: sub(0),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                    },
                    Enc::CopyTextureToBufferRegion {
                        src: 1,
                        src_sub: sub(0),
                        src_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                        dst: 2,
                        dst_offset: 0,
                        bytes_per_row: 2,
                        rows_per_image: 1,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("packed R16 texture transfers must use the executor fallback, not fail validation");
    assert_eq!(
        executor
            .read_buffer(&session.resources, BufferId(2), 0, 2)
            .unwrap(),
        half_one,
    );
}

#[test]
fn blit_reads_and_writes_the_named_mip_levels() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut session = session(&executor);
    let red = [211, 17, 43, 255];
    let source = red.repeat(16);
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, texture()),
            Cmd::CreateTexture(2, texture()),
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
                    size: 16,
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
                        bytes_per_row: 16,
                        rows_per_image: 4,
                        dst: 1,
                        dst_sub: sub(1),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    },
                    Enc::ClearRect {
                        texture: 2,
                        x: 0,
                        y: 0,
                        w: 2,
                        h: 2,
                        color: [0.0, 0.0, 1.0, 1.0],
                        base_array_layer: 0,
                        layer_count: 1,
                        mip_level: 2,
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: sub(1),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                        dst: 2,
                        dst_sub: sub(2),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                        filter: Filter::Nearest,
                        mirror: Mirror::NONE,
                    },
                    Enc::CopyTextureToBufferRegion {
                        src: 2,
                        src_sub: sub(2),
                        src_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                        dst: 2,
                        dst_offset: 0,
                        bytes_per_row: 8,
                        rows_per_image: 2,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("non-base mip blit must execute");
    assert_eq!(
        executor
            .read_buffer(&session.resources, BufferId(2), 0, 16)
            .unwrap(),
        red.repeat(4),
    );
}

#[test]
fn blit_between_disjoint_mips_of_one_texture_is_exact() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut session = session(&executor);
    let green = [13, 197, 61, 255];
    let source = green.repeat(16);
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, texture()),
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
                    size: 16,
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
                        bytes_per_row: 16,
                        rows_per_image: 4,
                        dst: 1,
                        dst_sub: sub(1),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: sub(1),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                        dst: 1,
                        dst_sub: sub(2),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                        filter: Filter::Nearest,
                        mirror: Mirror::NONE,
                    },
                    Enc::CopyTextureToBufferRegion {
                        src: 1,
                        src_sub: sub(2),
                        src_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                        dst: 2,
                        dst_offset: 0,
                        bytes_per_row: 8,
                        rows_per_image: 2,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("disjoint mip views of one texture must not alias");
    assert_eq!(
        executor
            .read_buffer(&session.resources, BufferId(2), 0, 16)
            .unwrap(),
        green.repeat(4),
    );
}

#[test]
fn blit_between_mips_of_a_named_array_layer_is_exact() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut session = session(&executor);
    let violet = [119, 23, 201, 255];
    let source = violet.repeat(16);
    let mut array = texture();
    array.depth = 6;
    let layer = |mip| TextureSubresource {
        mip,
        layer: 5,
        aspect: TextureAspect::All,
    };
    hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, array),
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
                    size: 16,
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
                        bytes_per_row: 16,
                        rows_per_image: 4,
                        dst: 1,
                        dst_sub: layer(1),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: layer(1),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                        dst: 1,
                        dst_sub: layer(2),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                        filter: Filter::Nearest,
                        mirror: Mirror::NONE,
                    },
                    Enc::CopyTextureToBufferRegion {
                        src: 1,
                        src_sub: layer(2),
                        src_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 2,
                            height: 2,
                            depth: 1,
                        },
                        dst: 2,
                        dst_offset: 0,
                        bytes_per_row: 8,
                        rows_per_image: 2,
                    },
                ],
                signal: None,
            }),
        ],
    )
    .expect("an array layer is one independently addressable Vulkan blit subresource");
    assert_eq!(
        executor
            .read_buffer(&session.resources, BufferId(2), 0, 16)
            .unwrap(),
        violet.repeat(4),
    );
}

#[test]
fn blit_refuses_a_mip_outside_the_allocated_chain() {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut session = session(&executor);
    let error = hl_gpu::runtime::submit(
        &mut session,
        &mut executor,
        0,
        &[
            Cmd::CreateTexture(1, texture()),
            Cmd::CreateTexture(2, texture()),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::BlitTexture {
                    src: 1,
                    src_sub: sub(4),
                    src_origin: Origin3d::default(),
                    src_extent: Extent3d {
                        width: 1,
                        height: 1,
                        depth: 1,
                    },
                    dst: 2,
                    dst_sub: sub(0),
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
    .unwrap_err();
    assert_eq!(error, GpuError::OutOfBounds);
}
