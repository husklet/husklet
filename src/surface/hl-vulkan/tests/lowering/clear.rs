use super::*;

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
        record::cmd_blit_image(d, cb, src, dst, (0, 0), (8, 8), (0, 0), (16, 16), true).unwrap();
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
        }]
    );
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
        record::cmd_clear_color_image(d, cb, img, [0.25, 0.5, 0.75, 1.0]).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::ClearRect {
            texture: ir,
            x: 0,
            y: 0,
            w: 32,
            h: 16,
            color: [0.25, 0.5, 0.75, 1.0]
        }]
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
            Enc::ClearRect {
                texture: ir,
                x: 8,
                y: 8,
                w: 16,
                h: 16,
                color: [1.0, 0.0, 0.0, 1.0]
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
