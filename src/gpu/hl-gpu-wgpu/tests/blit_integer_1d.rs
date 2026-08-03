//! Exact integer blits of one-dimensional textures use the native copy path.

use hl_gpu::protocol::model::descriptor::{
    BufferDesc, Extent3d, Mirror, Origin3d, TextureDesc, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, Filter, TextureDim, TextureFormat,
};
use hl_gpu::{Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const FORMATS: &[TextureFormat] = &[
    TextureFormat::R8Uint,
    TextureFormat::R8Sint,
    TextureFormat::Rg8Uint,
    TextureFormat::Rg8Sint,
    TextureFormat::Rgba8Uint,
    TextureFormat::Rgba8Sint,
];

#[test]
fn exact_integer_blit_copies_a_one_dimensional_texture() {
    for &format in FORMATS {
        let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("Metal adapter");
        let mut limits = Limits::from_capabilities(executor.capabilities());
        limits.copy_alignment = 1;
        let mut session = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        let texel_bytes = format.bytes_per_texel().expect("integer texel width");
        let source: Vec<u8> = (0..4 * texel_bytes)
            .map(|index| 19u8.wrapping_add((index as u8).wrapping_mul(47)))
            .collect();
        let texture = TextureDesc {
            width: 4,
            height: 1,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D1,
            format,
            usage: texture_usage::COPY_SRC | texture_usage::COPY_DST,
            label: String::new(),
        };
        let extent = Extent3d {
            width: 4,
            height: 1,
            depth: 1,
        };

        hl_gpu::runtime::submit(
            &mut session,
            &mut executor,
            0,
            &[
                Cmd::CreateTexture(1, texture.clone()),
                Cmd::CreateTexture(2, texture),
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
                    data: source.clone(),
                },
                Cmd::Submit(CommandBuffer {
                    encoder: vec![
                        Enc::CopyBufferToTextureRegion {
                            src: 1,
                            src_offset: 0,
                            bytes_per_row: source.len() as u32,
                            rows_per_image: 1,
                            dst: 1,
                            dst_sub: TextureSubresource::base(),
                            dst_origin: Origin3d::default(),
                            extent,
                        },
                        Enc::BlitTexture {
                            src: 1,
                            src_sub: TextureSubresource::base(),
                            src_origin: Origin3d::default(),
                            src_extent: extent,
                            dst: 2,
                            dst_sub: TextureSubresource::base(),
                            dst_origin: Origin3d::default(),
                            dst_extent: extent,
                            filter: Filter::Nearest,
                            mirror: Mirror::NONE,
                        },
                    ],
                    signal: None,
                }),
            ],
        )
        .unwrap_or_else(|error| panic!("{format:?}: exact D1 integer blit failed: {error:?}"));

        let actual = executor
            .read_texture(&session.resources, 2)
            .expect("read destination");
        assert_eq!(actual, source, "{format:?}");
    }
}
