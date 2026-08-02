//! Exact-slice proof for D3 `BlitTexture` depth resampling on the wgpu executor.

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

fn volume(depth: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: 1,
        height: 1,
        depth,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D3,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

fn run(source: &[[u8; 4]], dst_depth: u32, filter: Filter, mirror_z: bool) -> Vec<u8> {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let src_depth = source.len() as u32;
    let source: Vec<u8> = source.iter().flatten().copied().collect();

    hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                volume(
                    src_depth,
                    texture_usage::SAMPLED | texture_usage::COPY_SRC | texture_usage::COPY_DST,
                ),
            ),
            Cmd::CreateTexture(
                2,
                volume(
                    dst_depth,
                    texture_usage::RENDER_TARGET
                        | texture_usage::COPY_SRC
                        | texture_usage::COPY_DST,
                ),
            ),
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
            Cmd::CreateBuffer(
                2,
                BufferDesc {
                    size: u64::from(dst_depth) * 4,
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
                            depth: src_depth,
                        },
                    },
                    Enc::BlitTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        src_extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: src_depth,
                        },
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        dst_extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: dst_depth,
                        },
                        filter,
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
                            depth: dst_depth,
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
    .expect("D3 blit must execute");

    exec.read_buffer(&session.resources, BufferId(2), 0, dst_depth as usize * 4)
        .expect("read destination volume")
}

#[test]
fn unscaled_d3_blit_maps_each_slice_and_z_mirror_exactly() {
    let straight: Vec<u8> = SLICES.into_iter().flatten().collect();
    let reversed: Vec<u8> = SLICES.into_iter().rev().flatten().collect();
    assert_eq!(run(&SLICES, 3, Filter::Nearest, false), straight);
    assert_eq!(run(&SLICES, 3, Filter::Nearest, true), reversed);
}

#[test]
fn depth_scaled_d3_blit_samples_destination_slice_centers() {
    let ramp = [
        [0, 0, 0, 255],
        [64, 64, 64, 255],
        [128, 128, 128, 255],
        [255, 255, 255, 255],
    ];

    assert_eq!(
        run(&ramp, 2, Filter::Nearest, false),
        vec![64, 64, 64, 255, 255, 255, 255, 255],
        "4→2 nearest must select the source slices under destination centers"
    );
    assert_eq!(
        run(&ramp, 2, Filter::Nearest, true),
        vec![255, 255, 255, 255, 64, 64, 64, 255],
        "z mirroring must reverse the continuous source coordinate"
    );
    assert_eq!(
        run(&ramp, 1, Filter::Nearest, false),
        vec![128, 128, 128, 255],
        "a one-slice destination samples the midpoint of the source span"
    );

    let two = [[0, 0, 0, 255], [200, 200, 200, 255]];
    assert_eq!(
        run(&two, 4, Filter::Nearest, false),
        vec![0, 0, 0, 255, 0, 0, 0, 255, 200, 200, 200, 255, 200, 200, 200, 255,],
        "2→4 nearest must duplicate the source slice selected at each destination center"
    );
    assert_eq!(
        run(&two, 4, Filter::Linear, false),
        vec![0, 0, 0, 255, 50, 50, 50, 255, 150, 150, 150, 255, 200, 200, 200, 255,],
        "linear expansion must interpolate between adjacent source slices"
    );
}
