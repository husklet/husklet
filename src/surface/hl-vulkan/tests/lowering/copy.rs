use super::*;
use hl_gpu::protocol::model::enums::{TextureAspect, TextureDim};

#[test]
fn copy_buffer_lowers_to_copy_buffer_to_buffer() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 256).unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 256).unwrap();
    let (s, t) = (buf_ir(&d, src), buf_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer(d, cb, src, dst, 16, 32, 64).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToBuffer {
            src: s,
            src_offset: 16,
            dst: t,
            dst_offset: 32,
            size: 64
        }]
    );
}

#[test]
fn copy_buffer_to_image_lowers_to_copy_buffer_to_texture() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // A 4x4 RGBA8 target: tight-packed bytes_per_row = 4*4 = 16; span = 16*3 + 16 = 64.
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 64).unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let (s, t) = (buf_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer_to_image(d, cb, src, dst, 0, 0, 0, 4, 4).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToTexture {
            src: s,
            src_offset: 0,
            bytes_per_row: 16,
            dst: t,
            mip: 0,
            width: 4,
            height: 4
        }]
    );
}

#[test]
fn copy_buffer_to_bc3_image_uses_one_compressed_block_row() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 16).unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        4,
        4,
        vk_format::BC3_UNORM_BLOCK,
        vk_image_usage::TRANSFER_SRC | vk_image_usage::TRANSFER_DST | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let (s, t) = (buf_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer_to_image(d, cb, src, dst, 0, 0, 0, 4, 4).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToTexture {
            src: s,
            src_offset: 0,
            bytes_per_row: 16,
            dst: t,
            mip: 0,
            width: 4,
            height: 4,
        }]
    );
}

#[test]
fn copy_buffer_to_image_r8_coverage_atlas_uses_one_byte_per_texel() {
    // GPUI's glyph-coverage atlas is `R8Unorm` (1 byte/texel), uploaded region-by-region via
    // `queue.write_texture` → `vkCmdCopyBufferToImage` from a TIGHTLY-PACKED staging buffer. Regression
    // guard: the lowering must use the image's real bytes-per-texel (1 for R8), not a hardcoded 4. With the
    // old `* 4` assumption the implied span was 4x oversized, FAILED the `end <= buf_size` bounds check, and
    // the copy was rejected — silently, because `vkCmdCopyBufferToImage` returns void — so every glyph
    // upload was dropped and text never rasterized.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // An 8x8 R8 region: tight bytes_per_row = 8*1 = 8; total staging = 8*8 = 64 bytes. (Under the old bug
    // the implied span was 8*4*7 + 8*4 = 256 > 64, so this exact copy errored.)
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 64).unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        16,
        16,
        vk_format::R8_UNORM,
        vk_image_usage::TRANSFER_DST | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let (s, t) = (buf_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer_to_image(d, cb, src, dst, 0, 0, 0, 8, 8).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToTexture {
            src: s,
            src_offset: 0,
            bytes_per_row: 8,
            dst: t,
            mip: 0,
            width: 8,
            height: 8
        }],
        "an R8 coverage-atlas upload must lower with bytes_per_row = width*1, not width*4"
    );
}

#[test]
fn copy_image_to_buffer_r8_uses_one_byte_per_texel() {
    // The reverse (glyph atlas readback) path shares the same bytes-per-texel helper; an R8 image → tight
    // buffer copy must likewise use 1 byte/texel.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_image(
        &mut d,
        &mut sink,
        16,
        16,
        vk_format::R8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let (s, t) = (img_ir(&d, src), buf_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image_to_buffer(d, cb, src, dst, 0, 0, 0, 8, 8).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToBuffer {
            src: s,
            mip: 0,
            width: 8,
            height: 8,
            dst: t,
            dst_offset: 0,
            bytes_per_row: 8
        }]
    );
}

#[test]
fn copy_image_to_buffer_lowers_to_copy_texture_to_buffer() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_image(
        &mut d,
        &mut sink,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let (s, t) = (img_ir(&d, src), buf_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image_to_buffer(d, cb, src, dst, 0, 0, 0, 4, 4).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToBuffer {
            src: s,
            mip: 0,
            width: 4,
            height: 4,
            dst: t,
            dst_offset: 0,
            bytes_per_row: 16
        }]
    );
}

#[test]
fn cube_array_mip_layers_lower_to_explicit_buffer_texture_regions() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let upload =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 32).unwrap();
    let readback =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 32).unwrap();
    let image = create::create_image_layers(
        &mut d,
        &mut sink,
        8,
        8,
        12,
        4,
        true,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC | vk_image_usage::TRANSFER_DST | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let (upload_ir, image_ir, readback_ir) =
        (buf_ir(&d, upload), img_ir(&d, image), buf_ir(&d, readback));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer_to_image_region(
            d, cb, upload, image, 0, 0, 0, 2, 6, 0, 0, 0, 2, 2, 2,
        )
        .unwrap();
        record::cmd_copy_image_to_buffer_region(
            d, cb, image, readback, 0, 0, 0, 2, 6, 0, 0, 0, 2, 2, 2,
        )
        .unwrap();
    });
    let subresource = TextureSubresource {
        mip: 2,
        layer: 6,
        aspect: TextureAspect::All,
    };
    assert_eq!(
        enc,
        vec![
            Enc::CopyBufferToTextureRegion {
                src: upload_ir,
                src_offset: 0,
                bytes_per_row: 8,
                rows_per_image: 2,
                dst: image_ir,
                dst_sub: subresource,
                dst_origin: Origin3d::default(),
                extent: Extent3d {
                    width: 2,
                    height: 2,
                    depth: 2,
                },
            },
            Enc::CopyTextureToBufferRegion {
                src: image_ir,
                src_sub: subresource,
                src_origin: Origin3d::default(),
                extent: Extent3d {
                    width: 2,
                    height: 2,
                    depth: 2,
                },
                dst: readback_ir,
                dst_offset: 0,
                bytes_per_row: 8,
                rows_per_image: 2,
            },
        ]
    );
}

#[test]
fn three_dimensional_image_preserves_depth_and_z_copy_geometry() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let upload =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 64).unwrap();
    let readback =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let image = create::create_image_geometry(
        &mut d,
        &mut sink,
        8,
        4,
        4,
        1,
        3,
        TextureDim::D3,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC | vk_image_usage::TRANSFER_DST | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let image_record = d.images.get(&image).unwrap();
    assert_eq!(image_record.dim, TextureDim::D3);
    assert_eq!(image_record.depth, 4);
    assert_eq!(image_record.layers, 1);
    let create = sink
        .batches
        .iter()
        .flatten()
        .find_map(|command| match command {
            Cmd::CreateTexture(_, descriptor) if descriptor.label.starts_with("vkimg") => {
                Some(descriptor)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(create.dim, TextureDim::D3);
    assert_eq!(create.depth, 4);

    let upload_ir = buf_ir(&d, upload);
    let readback_ir = buf_ir(&d, readback);
    let image_ir = img_ir(&d, image);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer_to_image_region(
            d, cb, upload, image, 0, 0, 0, 0, 0, 0, 0, 1, 2, 1, 1,
        )
        .unwrap();
        record::cmd_copy_image_to_buffer_region(
            d, cb, image, readback, 0, 0, 0, 0, 0, 0, 0, 1, 2, 1, 1,
        )
        .unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::CopyBufferToTextureRegion {
                src: upload_ir,
                src_offset: 0,
                bytes_per_row: 8,
                rows_per_image: 1,
                dst: image_ir,
                dst_sub: TextureSubresource {
                    mip: 0,
                    layer: 0,
                    aspect: TextureAspect::All,
                },
                dst_origin: Origin3d { x: 0, y: 0, z: 1 },
                extent: Extent3d {
                    width: 2,
                    height: 1,
                    depth: 1,
                },
            },
            Enc::CopyTextureToBufferRegion {
                src: image_ir,
                src_sub: TextureSubresource {
                    mip: 0,
                    layer: 0,
                    aspect: TextureAspect::All,
                },
                src_origin: Origin3d { x: 0, y: 0, z: 1 },
                extent: Extent3d {
                    width: 2,
                    height: 1,
                    depth: 1,
                },
                dst: readback_ir,
                dst_offset: 0,
                bytes_per_row: 8,
                rows_per_image: 1,
            },
        ]
    );
}

#[test]
fn copy_image_lowers_to_copy_texture_to_texture() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let (s, t) = (img_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image(d, cb, src, dst, (1, 2), (3, 4), (4, 4)).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToTexture {
            src: s,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d { x: 1, y: 2, z: 0 },
            dst: t,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 3, y: 4, z: 0 },
            extent: Extent3d {
                width: 4,
                height: 4,
                depth: 1
            },
        }]
    );
    // Copy-compatible-format rejection: differing formats are a typed error, not a silent mis-copy.
    let other = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::B8G8R8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_copy_image(&mut d, cb, src, other, (0, 0), (0, 0), (4, 4)).is_err());
}

#[test]
fn resolve_image_lowers_to_copy_texture_to_texture() {
    // hl images are single-sample, so a multisample resolve is exactly a same-extent image COPY: it must
    // MOVE the source content into the resolve target (the old body recorded nothing → a blank target).
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let (s, t) = (img_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_resolve_image(d, cb, src, dst, (0, 0), (0, 0), (8, 8)).unwrap();
    });
    // A resolve lowers to the byte-identical op a same-region vkCmdCopyImage would emit (resolve == copy).
    let copy = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image(d, cb, src, dst, (0, 0), (0, 0), (8, 8)).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToTexture {
            src: s,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d { x: 0, y: 0, z: 0 },
            dst: t,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 0, y: 0, z: 0 },
            extent: Extent3d {
                width: 8,
                height: 8,
                depth: 1
            },
        }]
    );
    assert_eq!(
        enc, copy,
        "a single-sample resolve must lower to its copy twin"
    );
    // Truthful failure paths are inherited from cmd_copy_image: a missing-usage / format-mismatch target.
    let bad = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::B8G8R8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_resolve_image(&mut d, cb, src, bad, (0, 0), (0, 0), (8, 8)).is_err());
}
