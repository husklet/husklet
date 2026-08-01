use super::*;
#[test]
fn copy_buffer_to_image_usage_and_bounds_errors() {
    let mut d = dev();
    let mut s = sink();
    // src lacks TRANSFER_SRC.
    let bad_src =
        create::create_buffer(&mut d, &mut s, vk_buffer_usage::UNIFORM_BUFFER, 4096).unwrap();
    let img = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(matches!(
        record::cmd_copy_buffer_to_image(&mut d, cb, bad_src, img, 0, 0, 0, 8, 8),
        Err(GpuError::Invalid(_))
    ));
    // A good src but an oversized region (width > image width) is out of bounds.
    let src = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_SRC, 4096).unwrap();
    assert!(matches!(
        record::cmd_copy_buffer_to_image(&mut d, cb, src, img, 0, 0, 0, 16, 8),
        Err(GpuError::OutOfBounds)
    ));
}

#[test]
fn copy_image_size_incompatibility_and_self_overlap_rejected() {
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
        vk_format::B8G8R8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    // A one-byte format, for the size incompatibility a copy must still refuse.
    let narrow = create::create_image(
        &mut d,
        &mut s,
        8,
        8,
        vk_format::R8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // A copy REINTERPRETS rather than converts, so the specification asks for size-compatible formats and
    // not identical ones: RGBA8 into BGRA8 moves four bytes into four bytes and is legal. This asserted
    // the opposite, which was correct about the driver of the time and wrong about the specification —
    // the surface demanded equality where the IR beneath it requires only equal texel sizes.
    assert!(
        record::cmd_copy_image(
            &mut d,
            cb,
            a,
            b,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (0, 0),
            (4, 4)
        )
        .is_ok(),
        "RGBA8 into BGRA8 is a size-compatible copy"
    );
    // The refusal that remains, and the control for the acceptance above: a genuine size change.
    assert!(matches!(
        record::cmd_copy_image(
            &mut d,
            cb,
            a,
            narrow,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (0, 0),
            (4, 4)
        ),
        Err(GpuError::Invalid(_))
    ));
    // Overlapping same-image self-copy.
    assert!(matches!(
        record::cmd_copy_image(
            &mut d,
            cb,
            a,
            a,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (2, 2),
            (4, 4)
        ),
        Err(GpuError::Invalid(_))
    ));
    // A non-overlapping same-image copy is allowed.
    assert!(record::cmd_copy_image(
        &mut d,
        cb,
        a,
        a,
        SubresourceLayers::base(),
        SubresourceLayers::base(),
        (0, 0),
        (4, 0),
        (4, 4)
    )
    .is_ok());
}

#[test]
fn blit_same_image_rejected_and_zero_extent_rejected() {
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
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(matches!(
        record::cmd_blit_image(
            &mut d,
            cb,
            a,
            a,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (4, 4),
            (0, 0),
            (4, 4),
            true
        ),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_blit_image(
            &mut d,
            cb,
            a,
            b,
            SubresourceLayers::base(),
            SubresourceLayers::base(),
            (0, 0),
            (0, 4),
            (0, 0),
            (4, 4),
            false
        ),
        Err(GpuError::OutOfBounds)
    ));
}

#[test]
fn fill_buffer_alignment_usage_and_whole_size() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let ir = buf_ir(&d, buf);
    let no_dst =
        create::create_buffer(&mut d, &mut s, vk_buffer_usage::UNIFORM_BUFFER, 64).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Misaligned dstOffset.
    assert!(matches!(
        record::cmd_fill_buffer(&mut d, cb, buf, 3, 4, 0),
        Err(GpuError::Invalid(_))
    ));
    // Missing COPY_DST usage.
    assert!(matches!(
        record::cmd_fill_buffer(&mut d, cb, no_dst, 0, 4, 0),
        Err(GpuError::Invalid(_))
    ));
    // Out-of-bounds size.
    assert!(matches!(
        record::cmd_fill_buffer(&mut d, cb, buf, 0, 128, 0),
        Err(GpuError::OutOfBounds)
    ));
    // VK_WHOLE_SIZE fills to the end and flushes as a WriteBuffer of the whole buffer (64 bytes = 16 words).
    record::cmd_fill_buffer(&mut d, cb, buf, 0, u64::MAX, 0xAA).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    let (off, data) = s
        .batches
        .last()
        .unwrap()
        .iter()
        .find_map(|c| match c {
            Cmd::WriteBuffer { id, offset, data } if *id == ir => Some((*offset, data.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(off, 0);
    assert_eq!(data.len(), 64);
    // vkCmdFillBuffer replicates the 32-bit `data` word (0x000000AA) across the range, little-endian.
    assert!(data.chunks_exact(4).all(|w| w == [0xAA, 0, 0, 0]));
}

#[test]
fn update_buffer_size_limits() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Empty data.
    assert!(matches!(
        record::cmd_update_buffer(&mut d, cb, buf, 0, &[]),
        Err(GpuError::Invalid(_))
    ));
    // Not a multiple of 4.
    assert!(matches!(
        record::cmd_update_buffer(&mut d, cb, buf, 0, &[1, 2, 3]),
        Err(GpuError::Invalid(_))
    ));
    // Out of bounds.
    assert!(matches!(
        record::cmd_update_buffer(&mut d, cb, buf, 60, &[0u8; 8]),
        Err(GpuError::OutOfBounds)
    ));
}

#[test]
fn clear_color_image_requires_copy_dst() {
    let mut d = dev();
    let mut s = sink();
    let img = create::create_image(
        &mut d,
        &mut s,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(matches!(
        record::cmd_clear_color_image(&mut d, cb, img, [1.0; 4], &[]),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn clear_attachments_outside_render_pass_errors() {
    let mut d = dev();
    let mut s = sink();
    let _ = &mut s;
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Non-empty rect, no active render pass → error.
    assert!(matches!(
        record::cmd_clear_attachment_rect(&mut d, cb, 0, 0, 4, 4, [1.0; 4]),
        Err(GpuError::Invalid(_))
    ));
    // A zero-area rect is a spec-valid no-op even outside a pass.
    assert!(record::cmd_clear_attachment_rect(&mut d, cb, 0, 0, 0, 0, [1.0; 4]).is_ok());
}

// =====================================================================================================
// indirect draws / dispatch validation
// =====================================================================================================

#[test]
fn indirect_validation_missing_usage_and_out_of_range() {
    let mut d = dev();
    let mut s = sink();
    // Missing INDIRECT usage.
    let plain =
        create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(matches!(
        record::cmd_draw_indirect(&mut d, cb, plain, 0, 1, 16),
        Err(GpuError::Invalid(_))
    ));
    // Proper INDIRECT buffer but the argument span runs past the end.
    let ind = create::create_buffer(&mut d, &mut s, vk_buffer_usage::INDIRECT_BUFFER, 16).unwrap();
    assert!(matches!(
        record::cmd_draw_indirect(&mut d, cb, ind, 0, 2, 16),
        Err(GpuError::OutOfBounds)
    ));
    // A zero-count indirect draw is a valid no-op that records nothing.
    assert!(record::cmd_draw_indirect(&mut d, cb, ind, 0, 0, 16).is_ok());
    // Dispatch-indirect needs 12 bytes; a 8-byte buffer is out of range.
    let small = create::create_buffer(&mut d, &mut s, vk_buffer_usage::INDIRECT_BUFFER, 8).unwrap();
    assert!(matches!(
        record::cmd_dispatch_indirect(&mut d, cb, small, 0),
        Err(GpuError::OutOfBounds)
    ));
}

// =====================================================================================================
// push constants
// =====================================================================================================

#[test]
fn push_constants_alignment_rules() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(matches!(
        record::cmd_push_constants(&mut d, cb, 2, &[0u8; 4]),
        Err(GpuError::Invalid(_))
    )); // offset misaligned
    assert!(matches!(
        record::cmd_push_constants(&mut d, cb, 0, &[0u8; 3]),
        Err(GpuError::Invalid(_))
    )); // size misaligned
    assert!(matches!(
        record::cmd_push_constants(&mut d, cb, 0, &[]),
        Err(GpuError::Invalid(_))
    )); // empty
    assert!(record::cmd_push_constants(&mut d, cb, 4, &[1, 2, 3, 4]).is_ok());
    // Recorded at the offset (grown on demand): bytes [0..4) stay zero, [4..8) hold the write.
    assert_eq!(
        d.command_buffers.get(&cb).unwrap().push_constants,
        vec![0, 0, 0, 0, 1, 2, 3, 4]
    );
}

// =====================================================================================================
// pipeline barriers
// =====================================================================================================

#[test]
fn pipeline_barrier_records_known_and_skips_unknown() {
    let mut d = dev();
    let mut s = sink();
    let img = create::create_image(
        &mut d,
        &mut s,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Known image: layout recorded; unknown image: skipped (no panic, no entry).
    record::cmd_pipeline_barrier(&mut d, cb, &[(img, 0, 7), (0xdead, 0, 7)]).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    assert_eq!(d.image_layouts.get(&img), Some(&7));
    assert!(!d.image_layouts.contains_key(&0xdead));
    // No IR is emitted by the barrier — the Submit encoder is empty.
    match s.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => assert!(cbuf.encoder.is_empty()),
        other => panic!("{other:?}"),
    }
}

// =====================================================================================================
// events / semaphores
// =====================================================================================================

#[test]
fn event_host_lifecycle_and_unknown_errors() {
    let mut d = dev();
    let e = d.create_event();
    assert!(!d.event_status(e).unwrap());
    d.set_event(e, true).unwrap();
    assert!(d.event_status(e).unwrap());
    d.set_event(e, false).unwrap();
    assert!(!d.event_status(e).unwrap());
    assert!(matches!(
        d.set_event(0xdead, true),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(d.event_status(0xdead), Err(GpuError::Invalid(_))));
    d.destroy_event(e);
    assert!(matches!(d.event_status(e), Err(GpuError::Invalid(_))));
}

#[test]
fn cmd_set_event_unknown_is_rejected() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(matches!(
        record::cmd_set_event(&mut d, cb, 0xdead, true),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_wait_events(&mut d, cb, &[0xdead]),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn timeline_semaphore_monotonic_and_binary_rejected() {
    let mut d = dev();
    let t = sync::create_semaphore(&mut d, true, 5);
    assert_eq!(d.semaphore_counter(t).unwrap(), 5);
    d.signal_semaphore(t, 10).unwrap();
    assert_eq!(d.semaphore_counter(t).unwrap(), 10);
    // A signal to a lower value never regresses the counter.
    d.signal_semaphore(t, 3).unwrap();
    assert_eq!(d.semaphore_counter(t).unwrap(), 10);
    // A binary semaphore has no counter / cannot be host-signalled by value.
    let b = sync::create_semaphore(&mut d, false, 0);
    assert!(matches!(
        d.signal_semaphore(b, 1),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(d.semaphore_counter(b), Err(GpuError::Invalid(_))));
}

#[test]
fn wait_semaphores_any_all_and_empty() {
    let mut d = dev();
    let a = sync::create_semaphore(&mut d, true, 5);
    let b = sync::create_semaphore(&mut d, true, 0);
    // wait-all: a reached (>=5), b not (>=1) → false. wait-any → true.
    assert!(!sync::wait_semaphores(&d, &[a, b], &[5, 1], false));
    assert!(sync::wait_semaphores(&d, &[a, b], &[5, 1], true));
    // An empty wait is trivially satisfied.
    assert!(sync::wait_semaphores(&d, &[], &[], false));
    // An unknown semaphore counts as unreached.
    assert!(!sync::wait_semaphores(&d, &[0xdead], &[0], false));
}

// =====================================================================================================
// query pools
// =====================================================================================================

#[test]
fn query_pool_zero_count_rejected_and_span_bounds() {
    let mut d = dev();
    assert!(matches!(
        sync::create_query_pool(&mut d, 2, 0),
        Err(GpuError::Invalid(_))
    ));
    let pool = sync::create_query_pool(&mut d, 2, 4).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Out-of-range reset span.
    assert!(matches!(
        record::cmd_reset_query_pool(&mut d, cb, pool, 2, 4),
        Err(GpuError::Invalid(_))
    ));
    // Out-of-range write index.
    assert!(matches!(
        record::cmd_write_timestamp(&mut d, cb, pool, 4),
        Err(GpuError::Invalid(_))
    ));
    // Unknown pool.
    assert!(matches!(
        record::cmd_write_timestamp(&mut d, cb, 0xdead, 0),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn get_query_pool_results_availability_wait_partial() {
    let mut d = dev();
    let mut s = sink();
    let pool = sync::create_query_pool(&mut d, 2, 1).unwrap();
    // Unknown pool + out-of-range are typed errors.
    assert!(matches!(
        sync::get_query_pool_results(
            &d,
            0xdead,
            0,
            1,
            &mut [0u8; 8],
            8,
            true,
            false,
            false,
            false
        ),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        sync::get_query_pool_results(&d, pool, 0, 2, &mut [0u8; 8], 8, true, false, false, false),
        Err(GpuError::Invalid(_))
    ));
    // Unavailable slot, no WAIT/PARTIAL → NOT_READY (Ok(false)).
    let mut out = [0u8; 4];
    assert!(
        !sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false)
            .unwrap()
    );
    // WAIT forces a write even while unavailable → Ok(true) in the synchronous model.
    assert!(
        sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, true, false, false)
            .unwrap()
    );
    // Availability query: the availability word reports 0 (unavailable) for an untouched slot.
    let mut wide = [0u8; 8];
    sync::get_query_pool_results(&d, pool, 0, 1, &mut wide, 8, false, false, true, false).unwrap();
    assert_eq!(u32::from_le_bytes([wide[4], wide[5], wide[6], wide[7]]), 0);
    // After a device write-timestamp submit, the slot is available with a monotonic serial.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_write_timestamp(&mut d, cb, pool, 0).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    let mut out = [0u8; 4];
    assert!(
        sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false)
            .unwrap()
    );
    assert_eq!(u32::from_le_bytes(out), 1);
}

// =====================================================================================================
// fences
// =====================================================================================================

#[test]
fn fence_status_reset_and_fence_only_submit_signals() {
    let mut d = dev();
    let mut s = sink();
    let signaled = create::create_fence(&mut d, &mut s, true).unwrap();
    assert!(d.is_fence_signaled(signaled).unwrap());
    d.reset_fence(signaled).unwrap();
    assert!(!d.is_fence_signaled(signaled).unwrap());
    assert!(matches!(
        d.is_fence_signaled(0xdead),
        Err(GpuError::Invalid(_))
    ));

    // A fence-only submit (no command buffers) still emits one empty Submit that signals the fence.
    let fence = create::create_fence(&mut d, &mut s, false).unwrap();
    let fence_ir = d.fences.get(&fence).unwrap().ir_id;
    submit::queue_submit(&mut d, &mut s, &[], Some(fence)).unwrap();
    match s.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => {
            assert!(cbuf.encoder.is_empty());
            assert_eq!(cbuf.signal.map(|(ir, _)| ir), Some(fence_ir));
        }
        other => panic!("expected a fence-only Submit, got {other:?}"),
    }
    // Waiting the fence blocks on the sink at the signalled value and marks it signaled.
    submit::wait_for_fence(&mut d, &mut s, fence).unwrap();
    assert!(d.is_fence_signaled(fence).unwrap());
    assert!(!s.waits.is_empty());
}
