use dd_gpu::backend::GpuBackend;
use dd_gpu::id::TextureId;
use dd_gpu::ir::*;
use dd_gpu::software::SoftwareBackend;
use dd_gpu::{replay, GpuError};

fn desc(samples: u32) -> TextureDesc {
    TextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        mip_levels: 1,
        sample_count: samples,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::COPY_SRC | texture_usage::COPY_DST,
        label: String::new(),
    }
}

fn resolve(src_sub: TextureSubresource) -> Enc {
    Enc::ResolveTexture {
        src: 1,
        src_sub,
        src_origin: Origin3d::default(),
        dst: 2,
        dst_sub: TextureSubresource::base(),
        dst_origin: Origin3d::default(),
        extent: Extent3d { width: 1, height: 1, depth: 1 },
    }
}

#[test]
fn resolve_round_trips_every_region_and_subresource_field() {
    let op = Enc::ResolveTexture {
        src: 7,
        src_sub: TextureSubresource { mip: 2, layer: 3, aspect: TextureAspect::All },
        src_origin: Origin3d { x: 4, y: 5, z: 0 },
        dst: 8,
        dst_sub: TextureSubresource { mip: 6, layer: 7, aspect: TextureAspect::All },
        dst_origin: Origin3d { x: 8, y: 9, z: 0 },
        extent: Extent3d { width: 10, height: 11, depth: 1 },
    };
    let cmds = vec![Cmd::Submit(CommandBuffer { encoder: vec![op], signal: None })];
    assert_eq!(decode_stream(&encode_stream(&cmds)).unwrap(), cmds);
}

#[test]
fn software_resolve_averages_distinguishable_samples() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &desc(4)).unwrap();
    be.create_texture(TextureId(2), &desc(1)).unwrap();
    be.write_texture_samples(TextureId(1), &[
        0, 20, 40, 60, 40, 60, 80, 100, 80, 100, 120, 140, 120, 140, 160, 180,
    ]).unwrap();
    replay::replay(&mut be, &[Cmd::Submit(CommandBuffer { encoder: vec![resolve(TextureSubresource::base())], signal: None })]).unwrap();
    let mut out = [0u8; 4];
    be.read_texture(TextureId(2), &mut out).unwrap();
    assert_eq!(out, [60, 80, 100, 120]);
}

#[test]
fn invalid_resolve_subresource_is_atomic() {
    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &desc(4)).unwrap();
    be.create_texture(TextureId(2), &desc(1)).unwrap();
    be.write_texture_samples(TextureId(2), &[9, 8, 7, 6]).unwrap();
    let bad = resolve(TextureSubresource { mip: 0, layer: 1, aspect: TextureAspect::All });
    assert_eq!(
        replay::replay(&mut be, &[Cmd::Submit(CommandBuffer { encoder: vec![bad], signal: None })]),
        Err(GpuError::Unsupported("software: array-layer texture copy"))
    );
    let mut out = [0u8; 4];
    be.read_texture(TextureId(2), &mut out).unwrap();
    assert_eq!(out, [9, 8, 7, 6]);
}

#[test]
fn legacy_copy_blit_read_and_clear_reject_multisample_storage_atomically() {
    use dd_gpu::id::BufferId;

    let mut be = SoftwareBackend::new();
    be.create_texture(TextureId(1), &desc(4)).unwrap();
    be.create_texture(TextureId(2), &desc(1)).unwrap();
    let samples = [
        1, 2, 3, 4, 11, 12, 13, 14, 21, 22, 23, 24, 31, 32, 33, 34,
    ];
    be.write_texture_samples(TextureId(1), &samples).unwrap();
    be.write_texture_samples(TextureId(2), &[90, 91, 92, 93]).unwrap();
    be.create_buffer(
        BufferId(3),
        &BufferDesc { size: 16, usage: buffer_usage::COPY_SRC | buffer_usage::COPY_DST, label: String::new() },
    ).unwrap();
    be.write_buffer(BufferId(3), 0, &[0xaa; 16]).unwrap();

    let base = TextureSubresource::base();
    let extent = Extent3d { width: 1, height: 1, depth: 1 };
    let cases = [
        Enc::ClearRect { texture: 1, x: 0, y: 0, w: 1, h: 1, color: [1.0; 4] },
        Enc::CopyTextureToTexture {
            src: 1, src_sub: base, src_origin: Origin3d::default(),
            dst: 2, dst_sub: base, dst_origin: Origin3d::default(), extent,
        },
        Enc::BlitTexture {
            src: 1, src_sub: base, src_origin: Origin3d::default(), src_extent: extent,
            dst: 2, dst_sub: base, dst_origin: Origin3d::default(), dst_extent: extent,
            filter: Filter::Nearest,
        },
        Enc::CopyTextureToBuffer {
            src: 1, mip: 0, width: 1, height: 1, dst: 3, dst_offset: 0, bytes_per_row: 4,
        },
        Enc::CopyBufferToTexture {
            src: 3, src_offset: 0, bytes_per_row: 4, dst: 1, mip: 0, width: 1, height: 1,
        },
    ];
    for op in cases {
        let result = replay::replay(
            &mut be,
            &[Cmd::Submit(CommandBuffer { encoder: vec![op], signal: None })],
        );
        assert!(matches!(result, Err(GpuError::Unsupported(_))));
        let mut src = [0; 16];
        be.read_texture(TextureId(1), &mut src).unwrap();
        assert_eq!(src, samples, "rejected operation did not alter a sample plane");
        let mut dst = [0; 4];
        be.read_texture(TextureId(2), &mut dst).unwrap();
        assert_eq!(dst, [90, 91, 92, 93], "rejected operation did not alter destination");
        let mut buffer = [0; 16];
        be.read_buffer(BufferId(3), 0, &mut buffer).unwrap();
        assert_eq!(buffer, [0xaa; 16], "rejected readback did not alter destination buffer");
    }
}
