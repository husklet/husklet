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
