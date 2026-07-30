use super::*;

// =====================================================================================================
// vkCmdCopyImage / vkCmdBlitImage coordinate overflow (REGRESSION: `origin + extent` u32 add-overflow)
// =====================================================================================================

#[test]
fn copy_image_origin_overflow_is_out_of_bounds_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let a = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC | vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let b = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = recording_cb(&mut d);
    // An origin near `u32::MAX` previously overflow-panicked the `origin + extent > dim` bounds check.
    assert!(matches!(
        record::cmd_copy_image(&mut d, cb, a, b, (u32::MAX, 0), (0, 0), (4, 4)),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_copy_image(&mut d, cb, a, b, (0, 0), (u32::MAX, 0), (4, 4)),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_copy_image(&mut d, cb, a, b, (0, u32::MAX), (0, 0), (4, 4)),
        Err(GpuError::OutOfBounds)
    ));
    // A valid in-bounds copy still records.
    assert!(record::cmd_copy_image(&mut d, cb, a, b, (0, 0), (0, 0), (4, 4)).is_ok());
}

#[test]
fn blit_image_extent_overflow_is_out_of_bounds_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let a = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let b = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = recording_cb(&mut d);
    // Origin/extent near `u32::MAX` previously overflow-panicked the src/dst bounds checks.
    assert!(matches!(
        record::cmd_blit_image(
            &mut d,
            cb,
            a,
            b,
            (u32::MAX, 0),
            (4, 4),
            (0, 0),
            (4, 4),
            false
        ),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_blit_image(
            &mut d,
            cb,
            a,
            b,
            (0, 0),
            (4, 4),
            (0, u32::MAX),
            (4, 4),
            true
        ),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_blit_image(
            &mut d,
            cb,
            a,
            b,
            (4, 4),
            (u32::MAX, 1),
            (0, 0),
            (4, 4),
            false
        ),
        Err(GpuError::OutOfBounds)
    ));
    // A valid blit still records.
    assert!(record::cmd_blit_image(&mut d, cb, a, b, (0, 0), (4, 4), (0, 0), (8, 8), true).is_ok());
}

// =====================================================================================================
// invalid VkCreateInfo — zero / oversized image extent, bad format, garbage usage
// =====================================================================================================

#[test]
fn create_image_zero_oversized_extent_bad_format_and_usage() {
    let mut d = dev();
    let mut s = sink();
    // Zero extent is a spec violation → Invalid.
    assert!(matches!(
        create::create_image(
            &mut d,
            &mut s,
            0,
            4,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::SAMPLED,
            1
        ),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        create::create_image(
            &mut d,
            &mut s,
            4,
            0,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::SAMPLED,
            1
        ),
        Err(GpuError::Invalid(_))
    ));
    // An extent past maxImageDimension2D (16384) cannot be created → Invalid, never a fake success.
    let big = d.physical_device.limits.max_image_dimension_2d + 1;
    assert!(matches!(
        create::create_image(
            &mut d,
            &mut s,
            big,
            4,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::SAMPLED,
            1
        ),
        Err(GpuError::Invalid(_))
    ));
    // An unsupported format is rejected without allocating an image or emitting an IR command.
    let images_before = d.images.len();
    let batches_before = s.batches.len();
    assert!(matches!(
        create::create_image(
            &mut d,
            &mut s,
            4,
            4,
            0xDEAD_BEEF,
            vk_image_usage::SAMPLED,
            1,
        ),
        Err(GpuError::Invalid("vkCreateImage: unsupported VkFormat"))
    ));
    assert_eq!(d.images.len(), images_before);
    assert_eq!(s.batches.len(), batches_before);
    // Garbage usage bits: unknown bits are ignored (known bits translated), no panic.
    let img2 = create::create_image(
        &mut d,
        &mut s,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        0xFFFF_FFFF,
        1,
    )
    .unwrap();
    assert!(d.images.contains_key(&img2));
}

// =====================================================================================================
// invalid render-pass attachments at begin (unknown color/depth image → rejected, records nothing)
// =====================================================================================================

#[test]
fn begin_render_pass_bad_attachments_reject_and_record_nothing() {
    let mut d = dev();
    let mut s = sink();
    let good = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    // Classic path: an unknown color image is rejected up front.
    let cb = recording_cb(&mut d);
    assert!(matches!(
        record::cmd_begin_render_pass(&mut d, cb, 0xdead, [0.0; 4], true, None),
        Err(GpuError::Invalid(_))
    ));
    // Classic path: a valid color but an unknown depth attachment is rejected (attachment mismatch), and
    // nothing is recorded (the resolve fails BEFORE the encoder push).
    let depth = record::RenderingDepthAttachment {
        image: 0xdead,
        clear_depth: 1.0,
        load_clear: true,
    };
    assert!(matches!(
        record::cmd_begin_render_pass(&mut d, cb, good, [0.0; 4], true, Some(depth)),
        Err(GpuError::Invalid(_))
    ));
    assert!(d.command_buffers.get(&cb).unwrap().enc.is_empty());
    // Dynamic-rendering path: a mix of a valid + an unknown color attachment is rejected atomically.
    let mix = [
        record::RenderingColorAttachment {
            image: good,
            clear: [0.0; 4],
            load_clear: true,
            store: true,
        },
        record::RenderingColorAttachment {
            image: 0xdead,
            clear: [0.0; 4],
            load_clear: true,
            store: true,
        },
    ];
    assert!(matches!(
        record::cmd_begin_rendering(&mut d, cb, &mix, None),
        Err(GpuError::Invalid(_))
    ));
    assert!(d.command_buffers.get(&cb).unwrap().enc.is_empty());
    // A valid begin still records exactly one BeginRenderPass.
    record::cmd_begin_render_pass(&mut d, cb, good, [1.0; 4], true, None).unwrap();
    assert!(!d.command_buffers.get(&cb).unwrap().enc.is_empty());
}
