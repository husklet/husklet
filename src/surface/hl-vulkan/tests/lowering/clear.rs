use super::*;
use hl_gpu::protocol::model::descriptor::Mirror;

#[test]
fn blit_image_lowers_to_blit_texture_with_filter() {
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
        16,
        16,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let (s, t) = (img_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        // Upscale the 8x8 source into a 16x16 region with a linear filter.
        record::cmd_blit_image(
            d,
            cb,
            src,
            dst,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            Origin3d { x: 0, y: 0, z: 0 },
            Extent3d { width: 8, height: 8, depth: 1 },
            Origin3d { x: 0, y: 0, z: 0 },
            Extent3d { width: 16, height: 16, depth: 1 },
            true,
            Mirror::NONE,
        )
        .unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::BlitTexture {
            src: s,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d { x: 0, y: 0, z: 0 },
            src_extent: Extent3d {
                width: 8,
                height: 8,
                depth: 1
            },
            dst: t,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 0, y: 0, z: 0 },
            dst_extent: Extent3d {
                width: 16,
                height: 16,
                depth: 1
            },
            filter: Filter::Linear,
            mirror: Mirror::NONE,
        }]
    );
}

#[test]
fn blit_image_allows_disjoint_mips_of_one_image_and_rejects_overlap() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let image = create::create_image_layers(
        &mut d,
        &mut sink,
        8,
        8,
        1,
        4,
        false,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC | vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let ir = img_ir(&d, image);
    let mut mip1 = SubresourceLayers::base();
    mip1.mip_level = 1;
    let mut mip2 = SubresourceLayers::base();
    mip2.mip_level = 2;
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_blit_image(
            d,
            cb,
            image,
            image,
            mip1,
            mip2,
            Origin3d::default(),
            Extent3d { width: 4, height: 4, depth: 1 },
            Origin3d::default(),
            Extent3d { width: 2, height: 2, depth: 1 },
            false,
            Mirror::NONE,
        )
        .unwrap();
    });
    assert!(matches!(
        enc.as_slice(),
        [Enc::BlitTexture { src, src_sub, dst, dst_sub, .. }]
            if *src == ir && *dst == ir && src_sub.mip == 1 && dst_sub.mip == 2
    ));

    let cb = recording_cb(&mut d);
    let error = record::cmd_blit_image(
        &mut d,
        cb,
        image,
        image,
        mip1,
        mip1,
        Origin3d::default(),
        Extent3d { width: 4, height: 4, depth: 1 },
        Origin3d::default(),
        Extent3d { width: 4, height: 4, depth: 1 },
        false,
        Mirror::NONE,
    )
    .unwrap_err();
    assert_eq!(
        error,
        GpuError::Invalid(
            "vkCmdBlitImage: overlapping source and destination subresources of one image"
        )
    );
}

/// A MIRRORED region reaches the IR as a mirror, on the axes the caller inverted.
///
/// `vkCmdBlitImage` expresses a flip by putting `offsets[1]` before `offsets[0]` on an axis, and it is
/// legal. The origin and extent this recorder takes are already normalized — an unsigned pair cannot say
/// "flipped" — so the surface used to have nowhere to put the intent and refused the whole region as
/// unsupported (and before that dropped it with no error at all, leaving the destination stale).
///
/// The four states are asserted together, and the control is that they produce four DIFFERENT encodings:
/// a recorder that dropped `mirror` on the floor would record `Mirror::NONE` four times.
#[test]
fn blit_image_carries_a_mirror_per_axis() {
    for mirror in [
        Mirror::NONE,
        Mirror {
            x: true,
            ..Mirror::NONE
        },
        Mirror {
            y: true,
            ..Mirror::NONE
        },
        Mirror {
            x: true,
            y: true,
            ..Mirror::NONE
        },
    ] {
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
        let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
            record::cmd_blit_image(
                d,
                cb,
                src,
                dst,
                SubresourceLayers::base(),
                SubresourceLayers::base(),
                Origin3d { x: 0, y: 0, z: 0 },
                Extent3d { width: 8, height: 8, depth: 1 },
                Origin3d { x: 0, y: 0, z: 0 },
                Extent3d { width: 8, height: 8, depth: 1 },
                false,
                mirror,
            )
            .expect("a mirrored region is legal Vulkan and must record, not be refused");
        });
        let [Enc::BlitTexture {
            mirror: recorded, ..
        }] = enc.as_slice()
        else {
            panic!("expected exactly one BlitTexture, got {enc:?}");
        };
        assert_eq!(
            *recorded, mirror,
            "the recorded blit must carry the mirror the caller asked for"
        );
    }
}

#[test]
fn clear_color_image_lowers_to_full_extent_clear_rect() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img = create::create_image(
        &mut d,
        &mut sink,
        32,
        16,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let ir = img_ir(&d, img);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_color_image(d, cb, img, [0.25, 0.5, 0.75, 1.0], &[]).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::ClearRect {
            texture: ir,
            x: 0,
            y: 0,
            w: 32,
            h: 16,
            color: [0.25, 0.5, 0.75, 1.0],
            base_array_layer: 0,
            layer_count: 1,
            mip_level: 0,
        }]
    );
}

/// A layered image cleared over a SUBRANGE of its layers must clear those layers and no others. Before
/// the subresource range reached the IR, every clear addressed layer 0 of mip 0 whatever the caller asked
/// for, so this recorded a clear of layer 0 — writing the one layer the caller wanted preserved and
/// preserving the four it wanted cleared.
#[test]
fn clear_color_image_honours_the_array_layer_range() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img = create::create_image_layers(
        &mut d,
        &mut sink,
        32,
        16,
        6,
        1,
        false,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let ir = img_ir(&d, img);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_color_image(
            d,
            cb,
            img,
            [1.0, 0.0, 0.0, 1.0],
            &[SubresourceRange {
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 2,
                layer_count: 3,
            }],
        )
        .unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::ClearRect {
            texture: ir,
            x: 0,
            y: 0,
            w: 32,
            h: 16,
            color: [1.0, 0.0, 0.0, 1.0],
            base_array_layer: 2,
            layer_count: 3,
            mip_level: 0,
        }]
    );
}

/// Each mip level is a separate op carrying its OWN extent. Recording one op over the base extent would
/// overhang every smaller level, and the executor's clamp would then silently shrink the clear.
#[test]
fn clear_color_image_records_one_op_per_mip_level_at_that_levels_extent() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img = create::create_image_layers(
        &mut d,
        &mut sink,
        32,
        16,
        1,
        4,
        false,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let ir = img_ir(&d, img);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_color_image(
            d,
            cb,
            img,
            [0.0, 1.0, 0.0, 1.0],
            &[SubresourceRange {
                base_mip_level: 1,
                level_count: 2,
                base_array_layer: 0,
                layer_count: 1,
            }],
        )
        .unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::ClearRect {
                texture: ir,
                x: 0,
                y: 0,
                w: 16,
                h: 8,
                color: [0.0, 1.0, 0.0, 1.0],
                base_array_layer: 0,
                layer_count: 1,
                mip_level: 1,
            },
            Enc::ClearRect {
                texture: ir,
                x: 0,
                y: 0,
                w: 8,
                h: 4,
                color: [0.0, 1.0, 0.0, 1.0],
                base_array_layer: 0,
                layer_count: 1,
                mip_level: 2,
            },
        ]
    );
}

/// `VK_REMAINING_ARRAY_LAYERS`/`VK_REMAINING_MIP_LEVELS` mean "to the end of the image", and a range that
/// runs past the end is clamped rather than recorded as an out-of-bounds clear the executor would refuse.
#[test]
fn clear_color_image_resolves_the_remaining_sentinels_and_clamps() {
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
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_color_image(
            d,
            cb,
            img,
            [1.0; 4],
            &[
                SubresourceRange {
                    base_mip_level: 0,
                    level_count: SubresourceRange::REMAINING,
                    base_array_layer: 1,
                    layer_count: SubresourceRange::REMAINING,
                },
                // Entirely past the end: nothing exists to clear, so nothing is recorded.
                SubresourceRange {
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 9,
                    layer_count: 2,
                },
            ],
        )
        .unwrap();
    });
    let layers: Vec<(u32, u32)> = enc
        .iter()
        .filter_map(|e| match e {
            Enc::ClearRect {
                base_array_layer,
                layer_count,
                ..
            } => Some((*base_array_layer, *layer_count)),
            _ => None,
        })
        .collect();
    assert_eq!(
        layers,
        vec![(1, 3)],
        "REMAINING must resolve to layers 1..4"
    );
}

#[test]
fn clear_depth_stencil_image_lowers_to_depth_clear_render_pass() {
    use hl_gpu::protocol::model::descriptor::DepthAttachment;
    use hl_gpu::protocol::model::enums::LoadOp;

    // A depth-only image (D32) created as a DEPTH_STENCIL attachment + transfer-clear target.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let usage = vk_image_usage::DEPTH_STENCIL_ATTACHMENT | vk_image_usage::TRANSFER_DST;
    let img =
        create::create_image(&mut d, &mut sink, 16, 16, vk_format::D32_SFLOAT, usage, 1).unwrap();
    let ir = img_ir(&d, img);
    // Depth-only aspect (has_stencil = false): a zero-draw BeginRenderPass(depth CLEAR) / EndRenderPass.
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_depth_stencil_image(d, cb, img, 0.5, 7, false).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![],
                depth: Some(DepthAttachment {
                    texture: ir,
                    load: LoadOp::Clear,
                    clear_depth: 0.5,
                    clear_stencil: 0
                }),
            },
            Enc::EndRenderPass,
        ],
        "depth-only clear lowers to a depth-clear pass, stencil forced to 0"
    );

    // A combined depth+stencil image with the stencil aspect selected carries the stencil clear value.
    let ds = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::D24_UNORM_S8_UINT,
        usage,
        1,
    )
    .unwrap();
    let ds_ir = img_ir(&d, ds);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_depth_stencil_image(d, cb, ds, 1.0, 0x2a, true).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![],
                depth: Some(DepthAttachment {
                    texture: ds_ir,
                    load: LoadOp::Clear,
                    clear_depth: 1.0,
                    clear_stencil: 0x2a,
                }),
            },
            Enc::EndRenderPass,
        ],
        "combined depth+stencil clear carries the stencil value"
    );

    // Truthful errors: a color image is not a depth format; missing TRANSFER_DST usage is rejected.
    let color =
        create::create_image(&mut d, &mut sink, 8, 8, vk_format::R8G8B8A8_UNORM, usage, 1).unwrap();
    let no_dst = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::D32_SFLOAT,
        vk_image_usage::DEPTH_STENCIL_ATTACHMENT,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_clear_depth_stencil_image(&mut d, cb, color, 0.0, 0, false).is_err());
    assert!(record::cmd_clear_depth_stencil_image(&mut d, cb, no_dst, 0.0, 0, false).is_err());
    assert!(record::cmd_clear_depth_stencil_image(&mut d, cb, 0xdead, 0.0, 0, false).is_err());
}

/// `vkCmdClearAttachments` must lower to a PASS BOUNDARY, not to a `ClearRect` recorded inside the pass.
/// The executor refuses a transfer-shaped op between `BeginRenderPass` and `EndRenderPass` — it used to
/// drop them silently — so an op emitted there would fail the whole submit rather than clear anything.
///
/// The reopened pass must LOAD. Reopening with the original load operations would re-run a
/// `LoadOp::Clear` and erase everything drawn before the clear, which is the failure this shape is most
/// likely to regress into.
#[test]
fn clear_attachments_lowers_to_clear_rect_on_active_target() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let target = create::create_image(
        &mut d,
        &mut sink,
        64,
        64,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    let ir = img_ir(&d, target);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_begin_render_pass(d, cb, target, [0.0, 0.0, 0.0, 1.0], false, None).unwrap();
        record::cmd_clear_attachment_rect(d, cb, 8, 8, 16, 16, [1.0, 0.0, 0.0, 1.0]).unwrap();
        d.end_render_pass(cb).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![hl_gpu::protocol::model::descriptor::ColorAttachment {
                    texture: ir,
                    load: hl_gpu::protocol::model::enums::LoadOp::Load,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::EndRenderPass,
            Enc::ClearRect {
                texture: ir,
                x: 8,
                y: 8,
                w: 16,
                h: 16,
                color: [1.0, 0.0, 0.0, 1.0],
                base_array_layer: 0,
                layer_count: 1,
                mip_level: 0,
            },
            Enc::BeginRenderPass {
                color: vec![hl_gpu::protocol::model::descriptor::ColorAttachment {
                    texture: ir,
                    load: hl_gpu::protocol::model::enums::LoadOp::Load,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::EndRenderPass,
        ]
    );
    // A clear-attachments outside a render pass is a typed error (no active target to clear).
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_clear_attachment_rect(&mut d, cb, 0, 0, 4, 4, [0.0; 4]).is_err());
}

#[test]
fn fill_and_update_buffer_flush_as_write_buffer_at_submit() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let ir = buf_ir(&d, buf);
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Fill [0,8) with 0x01010101 (two words), then update [16,20) with explicit bytes.
    record::cmd_fill_buffer(&mut d, cb, buf, 0, 8, 0x0101_0101).unwrap();
    record::cmd_update_buffer(&mut d, cb, buf, 16, &[9, 8, 7, 6]).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // The two buffer writes flush (in record order) as WriteBuffers before the (empty) Submit.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer {
            id: i1,
            offset: 0,
            data: d1,
        }, Cmd::WriteBuffer {
            id: i2,
            offset: 16,
            data: d2,
        }, Cmd::Submit(_)] => {
            assert_eq!((*i1, *i2), (ir, ir));
            assert_eq!(d1, &vec![1u8; 8]);
            assert_eq!(d2, &vec![9u8, 8, 7, 6]);
        }
        other => panic!("expected [WriteBuffer, WriteBuffer, Submit], got {other:?}"),
    }
    // fill rejects a non-COPY_DST buffer and a misaligned offset.
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 64).unwrap();
    let cb2 = d.allocate_command_buffer();
    d.begin_command_buffer(cb2, false).unwrap();
    assert!(record::cmd_fill_buffer(&mut d, cb2, vbuf, 0, 8, 0).is_err());
    assert!(record::cmd_fill_buffer(&mut d, cb2, buf, 2, 8, 0).is_err());
}

/// A pass opened with `LoadOp::Clear` and then interrupted by `vkCmdClearAttachments` must reopen
/// LOADING. Reopening with the original `Clear` would wipe everything drawn between the pass start and
/// the attachment clear — the whole reason the clear was scissored in the first place.
///
/// A depth attachment is present so the depth load operation is checked too: it is a separate field and
/// carrying `Clear` through on it would silently reset the depth buffer mid-pass.
#[test]
fn an_interrupted_clearing_pass_reopens_loading_on_every_attachment() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let target = create::create_image(
        &mut d,
        &mut sink,
        32,
        32,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    let depth = create::create_image(
        &mut d,
        &mut sink,
        32,
        32,
        vk_format::D32_SFLOAT,
        vk_image_usage::DEPTH_STENCIL_ATTACHMENT,
        1,
    )
    .unwrap();
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_begin_render_pass(
            d,
            cb,
            target,
            [0.0, 0.0, 1.0, 1.0],
            // Open CLEARING, on both planes.
            true,
            Some(record::RenderingDepthAttachment {
                image: depth,
                load_clear: true,
                clear_depth: 1.0,
            }),
        )
        .unwrap();
        record::cmd_clear_attachment_rect(d, cb, 0, 0, 8, 8, [1.0, 0.0, 0.0, 1.0]).unwrap();
        d.end_render_pass(cb).unwrap();
    });

    let loads: Vec<(LoadOp, Option<LoadOp>)> = enc
        .iter()
        .filter_map(|e| match e {
            Enc::BeginRenderPass { color, depth } => {
                Some((color[0].load, depth.as_ref().map(|dep| dep.load)))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        loads.len(),
        2,
        "the clear must split the pass in two: {enc:?}"
    );
    assert_eq!(
        loads[0],
        (LoadOp::Clear, Some(LoadOp::Clear)),
        "the pass still opens clearing"
    );
    assert_eq!(
        loads[1],
        (LoadOp::Load, Some(LoadOp::Load)),
        "the reopened pass must LOAD both planes, or the clear erases the pass so far"
    );
    // And the fill lands BETWEEN the two passes, never inside one.
    let rect = enc
        .iter()
        .position(|e| matches!(e, Enc::ClearRect { .. }))
        .expect("a ClearRect");
    let ends = enc
        .iter()
        .position(|e| matches!(e, Enc::EndRenderPass))
        .expect("an EndRenderPass");
    assert!(
        ends < rect,
        "the rect fill must be outside the pass, not inside it: {enc:?}"
    );
}
