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
        record::cmd_copy_image(
            d,
            cb,
            src,
            dst,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (1, 2),
            (3, 4),
            (4, 4),
        )
        .unwrap();
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
    // Size-compatibility rejection. This used to use `B8G8R8A8_UNORM` and expect a refusal, which was
    // the surface's own rule rather than the specification's: a copy reinterprets, so four bytes into
    // four bytes is legal and is asserted as such in
    // `copy_image_accepts_size_compatible_formats_and_refuses_a_size_change`. What must still fail is a
    // genuine texel-size change, which would move the wrong number of bytes per texel.
    let other = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_copy_image(
        &mut d,
        cb,
        src,
        other,
        SubresourceLayers::base(),
        SubresourceLayers::base(),
        (0, 0),
        (0, 0),
        (4, 4)
    )
    .is_err());
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
        record::cmd_resolve_image(
            d,
            cb,
            src,
            dst,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (0, 0),
            (8, 8),
        )
        .unwrap();
    });
    // A resolve lowers to the byte-identical op a same-region vkCmdCopyImage would emit (resolve == copy).
    let copy = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image(
            d,
            cb,
            src,
            dst,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (0, 0),
            (8, 8),
        )
        .unwrap();
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
    assert!(record::cmd_resolve_image(
        &mut d,
        cb,
        src,
        bad,
        SubresourceLayers::base(),
        SubresourceLayers::base(),
        (0, 0),
        (0, 0),
        (8, 8)
    )
    .is_err());
}

/// A `w x h` image with `layers` array layers and `mips` mip levels, for the subresource tests below.
fn layered(
    d: &mut Device,
    sink: &mut RecordingSink,
    w: u32,
    h: u32,
    layers: u32,
    mips: u32,
    usage: u32,
) -> u64 {
    create::create_image_layers(
        d,
        sink,
        w,
        h,
        layers,
        mips,
        false,
        vk_format::R8G8B8A8_UNORM,
        usage,
        1,
    )
    .unwrap()
}

/// A copy naming mip level 2 must address level 2 on BOTH sides. The subresource of each `VkImageCopy`
/// region used to be discarded and every copy recorded against mip 0 / layer 0, so a copy of a higher
/// level silently read and wrote the base level's texels instead.
#[test]
fn copy_image_addresses_the_mip_level_the_region_names() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = layered(
        &mut d,
        &mut sink,
        32,
        32,
        1,
        4,
        vk_image_usage::TRANSFER_SRC,
    );
    let dst = layered(
        &mut d,
        &mut sink,
        32,
        32,
        1,
        4,
        vk_image_usage::TRANSFER_DST,
    );
    let (s, t) = (img_ir(&d, src), img_ir(&d, dst));
    let level = SubresourceLayers {
        mip_level: 2,
        base_array_layer: 0,
        layer_count: 1,
    };
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image(d, cb, src, dst, level, level, (0, 0), (0, 0), (8, 8)).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToTexture {
            src: s,
            src_sub: TextureSubresource {
                mip: 2,
                layer: 0,
                aspect: TextureAspect::All
            },
            src_origin: Origin3d { x: 0, y: 0, z: 0 },
            dst: t,
            dst_sub: TextureSubresource {
                mip: 2,
                layer: 0,
                aspect: TextureAspect::All
            },
            dst_origin: Origin3d { x: 0, y: 0, z: 0 },
            extent: Extent3d {
                width: 8,
                height: 8,
                depth: 1
            },
        }]
    );
}

/// A region past the end of level 2 must be REFUSED. The bounds check used to measure every region
/// against the BASE level, where an 8x8 copy of a 32x32 image trivially fits, so a copy that overran the
/// level it actually named passed validation.
#[test]
fn copy_image_bounds_check_uses_the_named_levels_extent() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = layered(
        &mut d,
        &mut sink,
        32,
        32,
        1,
        4,
        vk_image_usage::TRANSFER_SRC,
    );
    let dst = layered(
        &mut d,
        &mut sink,
        32,
        32,
        1,
        4,
        vk_image_usage::TRANSFER_DST,
    );
    // Level 3 of a 32x32 image is 4x4. A 16x16 region fits inside level 0 and must not fit inside level 3.
    let level = SubresourceLayers {
        mip_level: 3,
        base_array_layer: 0,
        layer_count: 1,
    };
    let cb = begin(&mut d, &mut sink);
    assert!(
        record::cmd_copy_image(&mut d, cb, src, dst, level, level, (0, 0), (0, 0), (16, 16))
            .is_err(),
        "a 16x16 region does not fit in the 4x4 level 3 it names"
    );
    // The same region against level 0 is legal, so the refusal is about the level and not the extent.
    assert!(record::cmd_copy_image(
        &mut d,
        cb,
        src,
        dst,
        SubresourceLayers::base(),
        SubresourceLayers::base(),
        (0, 0),
        (0, 0),
        (16, 16)
    )
    .is_ok());
}

/// A multi-layer region becomes one op per layer pair, because the IR subresource addresses one layer.
#[test]
fn copy_image_expands_a_layer_run_into_one_op_per_layer() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = layered(&mut d, &mut sink, 8, 8, 6, 1, vk_image_usage::TRANSFER_SRC);
    let dst = layered(&mut d, &mut sink, 8, 8, 6, 1, vk_image_usage::TRANSFER_DST);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image(
            d,
            cb,
            src,
            dst,
            SubresourceLayers {
                mip_level: 0,
                base_array_layer: 1,
                layer_count: 3,
            },
            SubresourceLayers {
                mip_level: 0,
                base_array_layer: 2,
                layer_count: 3,
            },
            (0, 0),
            (0, 0),
            (8, 8),
        )
        .unwrap();
    });
    let pairs: Vec<(u32, u32)> = enc
        .iter()
        .map(|e| match e {
            Enc::CopyTextureToTexture {
                src_sub, dst_sub, ..
            } => (src_sub.layer, dst_sub.layer),
            other => panic!("unexpected op {other:?}"),
        })
        .collect();
    assert_eq!(pairs, vec![(1, 2), (2, 3), (3, 4)]);
}

/// `VK_REMAINING_ARRAY_LAYERS` resolves against the image, and a layer run past the end is refused
/// rather than clamped: a copy that silently returned fewer layers than asked would hand back the wrong
/// texels, which is the failure mode the whole subresource change exists to remove.
#[test]
fn copy_image_resolves_remaining_layers_and_refuses_an_overrun() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = layered(&mut d, &mut sink, 8, 8, 4, 1, vk_image_usage::TRANSFER_SRC);
    let dst = layered(&mut d, &mut sink, 8, 8, 4, 1, vk_image_usage::TRANSFER_DST);
    let remaining = SubresourceLayers {
        mip_level: 0,
        base_array_layer: 1,
        layer_count: u32::MAX,
    };
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image(
            d,
            cb,
            src,
            dst,
            remaining,
            remaining,
            (0, 0),
            (0, 0),
            (8, 8),
        )
        .unwrap();
    });
    assert_eq!(enc.len(), 3, "layers 1..4 remain");

    let overrun = SubresourceLayers {
        mip_level: 0,
        base_array_layer: 2,
        layer_count: 3,
    };
    let cb = begin(&mut d, &mut sink);
    assert!(
        record::cmd_copy_image(
            &mut d,
            cb,
            src,
            dst,
            overrun,
            overrun,
            (0, 0),
            (0, 0),
            (8, 8)
        )
        .is_err(),
        "layers 2..5 do not exist on a 4-layer image"
    );
}

/// A same-image copy between DIFFERENT layers is legal and must not be refused as an overlapping
/// self-copy: the same rectangle of two array layers is two disjoint regions of memory.
#[test]
fn copy_image_allows_a_same_image_copy_between_different_layers() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img = create::create_image_layers(
        &mut d,
        &mut sink,
        8,
        8,
        4,
        1,
        false,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC | vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = begin(&mut d, &mut sink);
    let layer = |n| SubresourceLayers {
        mip_level: 0,
        base_array_layer: n,
        layer_count: 1,
    };
    assert!(
        record::cmd_copy_image(
            &mut d,
            cb,
            img,
            img,
            layer(0),
            layer(1),
            (0, 0),
            (0, 0),
            (8, 8)
        )
        .is_ok(),
        "layer 0 to layer 1 is disjoint"
    );
    assert!(
        record::cmd_copy_image(
            &mut d,
            cb,
            img,
            img,
            layer(0),
            layer(0),
            (0, 0),
            (0, 0),
            (8, 8)
        )
        .is_err(),
        "the same rectangle of the same layer still overlaps"
    );
}

// ---- buffer creation size guards ----------------------------------------------------------------
//
// `allocate_memory` refuses a zero or over-heap size and `create_image` refuses a zero or over-limit
// extent; `create_buffer` guarded neither, which is what makes this an oversight rather than a policy.
// Three sibling creation paths cannot disagree about the same class of caller mistake on purpose.

/// The normal path FIRST. Without this, a `create_buffer` that refused everything would satisfy both
/// refusal assertions below while measuring nothing at all.
#[test]
fn create_buffer_accepts_an_ordinary_size() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let handle = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 4096)
        .expect("an ordinary buffer must be created");
    assert_eq!(
        d.buffers.get(&handle).map(|b| b.size),
        Some(4096),
        "the created buffer must carry the size it was asked for"
    );
    // And one texel below the advertised ceiling is still legal, so the bound is a ceiling and not a
    // blanket refusal of large buffers.
    let ceiling = d.physical_device.limits.max_buffer_size;
    assert!(
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, ceiling).is_ok(),
        "a buffer exactly at maxBufferSize is legal"
    );
}

/// A zero-size buffer used to be created successfully. Its reported memory requirement was then 0 —
/// the same answer `vkGetBufferMemoryRequirements` gives for a buffer it has never heard of, so a
/// caller could not tell an allocation of nothing from a handle that does not exist.
#[test]
fn create_buffer_refuses_a_zero_size() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    assert!(
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 0).is_err(),
        "a zero-size buffer is not a buffer"
    );
}

/// The driver advertises a 2 GiB maxBufferSize and used to create a buffer of u64::MAX anyway,
/// contradicting its own advertisement and leaving the contradiction to surface somewhere later.
#[test]
fn create_buffer_refuses_a_size_past_the_advertised_ceiling() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let ceiling = d.physical_device.limits.max_buffer_size;
    for size in [ceiling + 1, u64::MAX] {
        assert!(
            create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, size).is_err(),
            "size {size} exceeds the advertised maxBufferSize {ceiling}"
        );
    }
}

/// A 1D image must reach the executor as a real `TextureDim::D1`, not collapsed onto a one-row 2D
/// texture: a `sampler1D` binding expects a D1 view and rejects anything else at bind time. And the
/// creation path must enforce the same limits the format query advertises for 1D — one row, no mip
/// chain — so the two cannot disagree.
#[test]
fn a_one_dimensional_image_lowers_to_d1_and_enforces_its_limits() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let handle = create::create_image_geometry(
        &mut d,
        &mut sink,
        64,
        1,
        1,
        1,
        1,
        TextureDim::D1,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::SAMPLED,
        1,
    )
    .expect("a 1D image is creatable");
    let created = sink
        .batches
        .iter()
        .flatten()
        .find_map(|c| match c {
            Cmd::CreateTexture(_, desc) => Some(desc.clone()),
            _ => None,
        })
        .expect("a CreateTexture");
    assert_eq!(created.dim, TextureDim::D1, "must not collapse onto 2D");
    assert_eq!(created.width, 64);
    assert_eq!(created.height, 1);
    assert!(d.images.contains_key(&handle));

    // A second row or a mip chain is refused, matching the advertised 1D limits.
    assert!(
        create::create_image_geometry(
            &mut d,
            &mut sink,
            64,
            4,
            1,
            1,
            1,
            TextureDim::D1,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::SAMPLED,
            1,
        )
        .is_err(),
        "a 1D image has one row"
    );
    assert!(
        create::create_image_geometry(
            &mut d,
            &mut sink,
            64,
            1,
            1,
            1,
            4,
            TextureDim::D1,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::SAMPLED,
            1,
        )
        .is_err(),
        "the D1 path carries no mip chain"
    );
}

// ---- recording errors and render-pass scope ------------------------------------------------------

/// A `vkCmd*` that fails while recording must make the command buffer invalid, and
/// `vkEndCommandBuffer` is where the specification says that is reported. Every recording call site in
/// the shim used to discard its `Result`, so a buffer that had silently dropped work still ended
/// successfully and was submitted.
#[test]
fn a_failed_recording_command_is_reported_by_end_command_buffer() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let buffer =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 256).unwrap();

    // Positive control FIRST: a clean recording still ends successfully. Without it, an `end()` that
    // failed unconditionally would satisfy the assertion below while meaning nothing.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    let ok = record::cmd_fill_buffer(&mut d, cb, buffer, 0, 4, 0);
    d.latch(cb, ok);
    assert!(
        d.end_command_buffer(cb).is_ok(),
        "a clean recording must still end successfully"
    );

    // Now a failing command: an unknown buffer handle.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    let failed = record::cmd_fill_buffer(&mut d, cb, 0xDEAD_BEEF, 0, 4, 0);
    assert!(failed.is_err(), "an unknown VkBuffer must fail to record");
    d.latch(cb, failed);
    assert!(
        d.end_command_buffer(cb).is_err(),
        "vkEndCommandBuffer must report a command buffer invalidated during recording"
    );
}

/// The FIRST recording error is the one reported, because the later ones are usually its consequences.
#[test]
fn the_first_recording_error_is_the_one_reported() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // TRANSFER_SRC only: a fill needs COPY_DST, so the second command below really fails, with a
    // different message from the first.
    let buffer =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 256).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();

    let first = record::cmd_fill_buffer(&mut d, cb, 0xDEAD_BEEF, 0, 4, 0);
    assert!(first.is_err());
    d.latch(cb, first);
    // A DIFFERENT, later failure must not displace it. Both must really fail and their messages must
    // differ, or this test cannot tell first from last — an earlier draft used a second command that
    // returned Ok, so the assertion held no matter which the code kept.
    let second = record::cmd_fill_buffer(&mut d, cb, buffer, 0, 4, 0);
    assert!(
        second.is_err(),
        "the second command must also fail or this test proves nothing"
    );
    assert_ne!(
        format!("{:?}", first_message(&second)),
        "unknown VkBuffer",
        "the two errors must be distinguishable"
    );
    d.latch(cb, second);

    let reported = d.end_command_buffer(cb).expect_err("still invalid");
    assert!(
        format!("{reported:?}").contains("unknown VkBuffer"),
        "expected the FIRST error (the unknown buffer), got {reported:?}"
    );
}

fn first_message<T>(r: &Result<T, GpuError>) -> String {
    match r {
        Err(e) => format!("{e:?}"),
        Ok(_) => String::new(),
    }
}

/// Transfer commands are confined to OUTSIDE a render pass by the specification, and the executor now
/// refuses any such operation encoded between Begin and End rather than dropping it. Catching the
/// misuse while recording keeps the blast radius at the one command instead of failing the whole
/// submit, and lets vkEndCommandBuffer name it.
#[test]
fn a_transfer_command_inside_a_render_pass_is_refused_while_recording() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let target = create::create_image(
        &mut d,
        &mut sink,
        32,
        32,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();

    // OUTSIDE the pass the very same call succeeds — the refusal is about scope, not the arguments.
    assert!(
        record::cmd_clear_color_image(&mut d, cb, target, [1.0; 4], &[]).is_ok(),
        "outside a pass this clear is legal"
    );

    record::cmd_begin_render_pass(&mut d, cb, target, [0.0; 4], true, None).unwrap();
    assert!(
        record::cmd_clear_color_image(&mut d, cb, target, [1.0; 4], &[]).is_err(),
        "vkCmdClearColorImage is an outside-render-pass command"
    );
    d.end_render_pass(cb).unwrap();

    // And after the pass closes it is legal again.
    assert!(record::cmd_clear_color_image(&mut d, cb, target, [1.0; 4], &[]).is_ok());
}

/// A multisampled image is a render target this backend draws into and resolves out of: one layer, one
/// mip. Creating a multisampled ARRAY image produced a host validation failure — "Multisampled texture
/// depth or array layers must be 1, got 8" — which reached the guest as a refused frame it could not
/// attribute to any particular call.
#[test]
fn a_multisampled_image_must_be_single_layer_and_single_mip() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let make = |d: &mut Device, sink: &mut RecordingSink, layers, mips, samples| {
        create::create_image_layers(
            d,
            sink,
            32,
            32,
            layers,
            mips,
            false,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::COLOR_ATTACHMENT,
            samples,
        )
    };
    // Positive controls FIRST, so the refusals below are about multisampling and not about the shape.
    assert!(
        make(&mut d, &mut sink, 1, 1, 4).is_ok(),
        "a plain multisampled 2D image is exactly what this backend supports"
    );
    assert!(
        make(&mut d, &mut sink, 8, 4, 1).is_ok(),
        "a single-sample layered, mipped image is still fine"
    );

    assert!(
        make(&mut d, &mut sink, 8, 1, 4).is_err(),
        "a multisampled ARRAY image is what the host refused"
    );
    assert!(
        make(&mut d, &mut sink, 1, 4, 4).is_err(),
        "a multisampled mip chain is forbidden by Vulkan itself"
    );
}

/// `vkCmdBlitImage` between DIFFERENT formats records, because a blit converts.
///
/// The surface required the two formats to be identical, which is this driver's own rule and not the
/// specification's: a blit is distinguished from a copy precisely by converting, and the formats may
/// differ freely except in numeric class. The host does exactly that — measured directly on lavapipe, it
/// resamples `Rgba8Unorm` into `Bgra8Unorm`, `Rgba8Srgb`, `R32Float` and `Rgba16Float`, and across a
/// texel-size change in both directions. The refusal turned the canonical case into an error: blitting an
/// image into a differently-formatted swapchain image.
///
/// Same shape as the reference defect closed in `hl-gpu` earlier — a layer refusing what the layer below
/// performs perfectly well — which is why the pairs here include a channel swap, a transfer function, a
/// widening and a narrowing rather than one representative case.
#[test]
fn blit_between_different_formats_records() {
    for (src_format, dst_format) in [
        (vk_format::R8G8B8A8_UNORM, vk_format::B8G8R8A8_UNORM),
        (vk_format::R8G8B8A8_UNORM, vk_format::R8G8B8A8_SRGB),
        (vk_format::R8_UNORM, vk_format::R8G8B8A8_UNORM),
        (vk_format::R8G8B8A8_UNORM, vk_format::R8_UNORM),
    ] {
        let mut d = dev();
        let mut s = RecordingSink::with_full_caps();
        let a = create::create_image(
            &mut d,
            &mut s,
            4,
            4,
            src_format,
            vk_image_usage::TRANSFER_SRC,
            1,
        )
        .unwrap();
        let b = create::create_image(
            &mut d,
            &mut s,
            4,
            4,
            dst_format,
            vk_image_usage::TRANSFER_DST,
            1,
        )
        .unwrap();
        let cb = recording_cb(&mut d);
        assert!(
            record::cmd_blit_image(
                &mut d,
                cb,
                a,
                b,
                SubresourceLayers::base(),
                SubresourceLayers::base(),
                (0, 0),
                (4, 4),
                (0, 0),
                (4, 4),
                true,
            )
            .is_ok(),
            "{src_format:#x} -> {dst_format:#x} is a legal converting blit"
        );
    }
}

/// An INTEGER blit is refused, and the two reasons are reported as two different errors.
///
/// A mixed integer/float pair violates the specification's numeric-class rule and is the APPLICATION's
/// mistake — `Invalid`. A matched integer pair is legal Vulkan that this driver cannot serve, because the
/// host blits by rendering and neither binds an integer view to a filterable sampler nor writes a float
/// shader output into an integer target — `Unsupported`. Collapsing those into one answer would tell an
/// application it had made an error when the limitation is ours.
///
/// It matters that this is refused HERE. Before, a matched integer pair passed the format-equality check,
/// lowered, and failed inside the executor as a device-validation error the caller could not attribute.
#[test]
fn an_integer_blit_is_refused_and_says_whose_fault_it_is() {
    let cases = [
        (
            vk_format::R8G8B8A8_UINT,
            vk_format::R8G8B8A8_UNORM,
            "mixed numeric class",
            false,
        ),
        (
            vk_format::R8G8B8A8_UINT,
            vk_format::R8G8B8A8_UINT,
            "matched integer pair",
            true,
        ),
    ];
    for (src_format, dst_format, what, unsupported) in cases {
        let mut d = dev();
        let mut s = RecordingSink::with_full_caps();
        let a = create::create_image(
            &mut d,
            &mut s,
            4,
            4,
            src_format,
            vk_image_usage::TRANSFER_SRC,
            1,
        )
        .unwrap();
        let b = create::create_image(
            &mut d,
            &mut s,
            4,
            4,
            dst_format,
            vk_image_usage::TRANSFER_DST,
            1,
        )
        .unwrap();
        let cb = recording_cb(&mut d);
        let result = record::cmd_blit_image(
            &mut d,
            cb,
            a,
            b,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (4, 4),
            (0, 0),
            (4, 4),
            false,
        );
        match (unsupported, result) {
            (true, Err(GpuError::Unsupported(_))) => {}
            (false, Err(GpuError::Invalid(_))) => {}
            (_, other) => panic!("{what}: unexpected {other:?}"),
        }
    }
}

/// `vkCmdCopyImage` requires IDENTICAL formats, which is stricter than Vulkan and deliberately so.
///
/// The specification asks only for size-compatible formats, because a copy reinterprets: RGBA8 into BGRA8
/// moves four bytes unchanged and reads back channel-swapped. This surface could express that — I relaxed
/// it to exactly that, having measured the executor ACCEPTING such a copy — and it was wrong, because
/// "accepted without error" is not "produced the right bytes" and I did not check the second thing.
///
/// A differential program written to pin the relaxed behaviour immediately caught it: the two backends
/// produce DIFFERENT pixels for one command. `Enc::CopyTextureToTexture` is reinterpreted by the software
/// oracle, which moves the bytes, and converted by the wgpu executor, which deliberately routes a
/// mismatched pair through a blit so GL's converting copy paths work. A Vulkan copy must reinterpret, so
/// it cannot ride an operation that might convert.
///
/// This test therefore pins the RESTRICTION and its reason, so the next person to notice that the
/// specification is looser finds out why before relaxing it again.
#[test]
fn copy_image_requires_identical_formats_until_the_ir_contract_is_settled() {
    let attempt = |src_format: u32, dst_format: u32| {
        let mut d = dev();
        let mut s = RecordingSink::with_full_caps();
        let a = create::create_image(
            &mut d,
            &mut s,
            4,
            4,
            src_format,
            vk_image_usage::TRANSFER_SRC,
            1,
        )
        .unwrap();
        let b = create::create_image(
            &mut d,
            &mut s,
            4,
            4,
            dst_format,
            vk_image_usage::TRANSFER_DST,
            1,
        )
        .unwrap();
        let cb = recording_cb(&mut d);
        record::cmd_copy_image(
            &mut d,
            cb,
            a,
            b,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (0, 0),
            (4, 4),
        )
    };

    // The control, and the case that must never regress: identical formats copy.
    assert!(
        attempt(vk_format::R8G8B8A8_UNORM, vk_format::R8G8B8A8_UNORM).is_ok(),
        "the identical-format copy is the whole supported surface and must keep working"
    );
    // Size-compatible but different meaning: legal Vulkan, refused here, for the reason above.
    assert!(
        matches!(
            attempt(vk_format::R8G8B8A8_UNORM, vk_format::B8G8R8A8_UNORM),
            Err(GpuError::Invalid(_))
        ),
        "a channel-order change would convert on one backend and reinterpret on the other"
    );
    // And a genuine size change stays refused by the same rule.
    assert!(
        matches!(
            attempt(vk_format::R8_UNORM, vk_format::R8G8B8A8_UNORM),
            Err(GpuError::Invalid(_))
        ),
        "one byte into four is not a copy under any reading"
    );
}

/// `VK_FILTER_LINEAR` from a source format that cannot be linearly filtered is refused at RECORD time,
/// and the nearest blit from the same format still records.
///
/// The 32-bit float formats are non-filterable in WebGPU without an optional feature, and Vulkan
/// independently requires a source format to advertise linear filtering. The host was measured refusing
/// exactly these two — and refusing them for BOTH filters until the blit's bind-group layout stopped
/// declaring filterability unconditionally, which is a separate fix in the executor that this refusal is
/// paired with: nearest now works there, so it must not be refused here.
///
/// The pairing is the whole test. Refusing linear while also refusing nearest would look identical from
/// the outside and would be wrong, so the nearest arm is what distinguishes "this filter is unsupported"
/// from "this format is unsupported".
#[test]
fn a_linear_blit_from_a_non_filterable_format_is_refused_but_nearest_records() {
    for src_format in [vk_format::R32_SFLOAT, vk_format::R32G32B32A32_SFLOAT] {
        let attempt = |linear: bool| {
            let mut d = dev();
            let mut s = RecordingSink::with_full_caps();
            let a = create::create_image(
                &mut d,
                &mut s,
                4,
                4,
                src_format,
                vk_image_usage::TRANSFER_SRC,
                1,
            )
            .unwrap();
            let b = create::create_image(
                &mut d,
                &mut s,
                4,
                4,
                vk_format::R8G8B8A8_UNORM,
                vk_image_usage::TRANSFER_DST,
                1,
            )
            .unwrap();
            let cb = recording_cb(&mut d);
            record::cmd_blit_image(
                &mut d,
                cb,
                a,
                b,
                SubresourceLayers::base(),
                SubresourceLayers::base(),
                (0, 0),
                (4, 4),
                (0, 0),
                (4, 4),
                linear,
            )
        };
        assert!(
            attempt(false).is_ok(),
            "{src_format:#x} records under VK_FILTER_NEAREST — no filtering is required"
        );
        assert!(
            matches!(attempt(true), Err(GpuError::Unsupported(_))),
            "{src_format:#x} cannot be linearly filtered, and that is OUR limit, not the caller's error"
        );
    }

    // The control that keeps the refusal about the FORMAT: a filterable source takes linear happily.
    let mut d = dev();
    let mut s = RecordingSink::with_full_caps();
    let a = create::create_image(
        &mut d,
        &mut s,
        4,
        4,
        vk_format::R16G16B16A16_SFLOAT,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let b = create::create_image(
        &mut d,
        &mut s,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = recording_cb(&mut d);
    assert!(
        record::cmd_blit_image(
            &mut d,
            cb,
            a,
            b,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (4, 4),
            (0, 0),
            (4, 4),
            true,
        )
        .is_ok(),
        "a half-float source IS filterable and must keep taking a linear blit"
    );
}
