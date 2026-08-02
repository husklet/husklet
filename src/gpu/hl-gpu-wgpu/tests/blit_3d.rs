//! Exact-slice proof for an unscaled D3 `BlitTexture` on the wgpu executor.

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

const SLICES: [[u8; 4]; 3] = [[220, 20, 30, 255], [40, 210, 50, 255], [60, 70, 200, 255]];

fn volume(usage: u32) -> TextureDesc {
    TextureDesc {
        width: 1,
        height: 1,
        depth: 3,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D3,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

fn run(mirror_z: bool) -> Vec<u8> {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let source: Vec<u8> = SLICES.into_iter().flatten().collect();

    hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(1, volume(texture_usage::SAMPLED | texture_usage::COPY_DST)),
            Cmd::CreateTexture(
                2,
                volume(texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 12,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: source,
            },
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: 12,
                    usage: buffer_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTextureRegion {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 4,
                        rows_per_image: 1,
                        dst: 1,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 3,
                        },
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 3,
                        },
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 3,
                        },
                        filter: Filter::Nearest,
                        mirror: Mirror {
                            x: false,
                            y: false,
                            z: mirror_z,
                        },
                    },
                    Enc::CopyTextureToBufferRegion {
                        src: 2,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 3,
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
    .expect("unscaled D3 blit must execute");

    exec.read_buffer(&session.resources, BufferId(2), 0, 12)
        .expect("read destination volume")
}

#[test]
fn unscaled_d3_blit_maps_each_slice_and_z_mirror_exactly() {
    let straight: Vec<u8> = SLICES.into_iter().flatten().collect();
    let reversed: Vec<u8> = SLICES.into_iter().rev().flatten().collect();
    assert_eq!(run(false), straight);
    assert_eq!(run(true), reversed);
}
