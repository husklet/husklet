use super::*;

#[test]
fn event_host_ops_and_device_set_resolves_at_submit() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let ev = d.create_event();
    assert!(!d.event_status(ev).unwrap()); // created unsignaled

    // Host set/reset mutate directly.
    d.set_event(ev, true).unwrap();
    assert!(d.event_status(ev).unwrap());
    d.set_event(ev, false).unwrap();
    assert!(!d.event_status(ev).unwrap());

    // A device vkCmdSetEvent resolves at (synchronous) submit completion — signaled once submit returns.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_set_event(&mut d, cb, ev, true).unwrap();
    d.end_command_buffer(cb).unwrap();
    assert!(
        !d.event_status(ev).unwrap(),
        "not signaled until the submit completes"
    );
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
    assert!(
        d.event_status(ev).unwrap(),
        "device-set event signaled after submit"
    );

    // An unknown event is a typed error, never a false success.
    assert!(d.set_event(0xdead, true).is_err());
}

#[test]
fn timeline_semaphore_signal_wait_roundtrips() {
    let mut d = dev();
    let sem = sync::create_semaphore(&mut d, true, 2); // timeline, initial 2
    assert_eq!(d.semaphore_counter(sem).unwrap(), 2);

    // Host signal advances the counter monotonically (a signal below the current value is a no-op).
    d.signal_semaphore(sem, 5).unwrap();
    assert_eq!(d.semaphore_counter(sem).unwrap(), 5);
    d.signal_semaphore(sem, 3).unwrap();
    assert_eq!(d.semaphore_counter(sem).unwrap(), 5);

    // A satisfied wait (counter >= value) is true; an unmet one is false (→ VK_TIMEOUT at the shim).
    assert!(sync::wait_semaphores(&d, &[sem], &[5], false));
    assert!(!sync::wait_semaphores(&d, &[sem], &[6], false));

    // A binary semaphore has no timeline counter — host counter ops are typed errors.
    let bin = sync::create_semaphore(&mut d, false, 0);
    assert!(d.semaphore_counter(bin).is_err());
    assert!(d.signal_semaphore(bin, 1).is_err());
}

#[test]
fn query_pool_timestamp_records_and_results_readable() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // A 2-slot TIMESTAMP pool (VkQueryType TIMESTAMP = 2).
    let pool = sync::create_query_pool(&mut d, 2, 2).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_reset_query_pool(&mut d, cb, pool, 0, 2).unwrap();
    record::cmd_write_timestamp(&mut d, cb, pool, 0).unwrap();
    d.end_command_buffer(cb).unwrap();

    // Before submit the slot is unavailable → NOT_READY (no WAIT/PARTIAL).
    let mut out = [0u8; 4];
    assert!(
        !sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false)
            .unwrap()
    );

    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // After the (synchronous) submit the timestamp slot is available with a monotonic serial (1).
    let mut out = [0u8; 4];
    assert!(
        sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false)
            .unwrap()
    );
    assert_eq!(u32::from_le_bytes(out), 1);

    // A host reset clears availability again.
    sync::reset_query_pool(&mut d, pool, 0, 2);
    let mut out = [0u8; 4];
    assert!(
        !sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false)
            .unwrap()
    );
}

#[test]
fn occlusion_query_counts_scissor_clipped_coverage() {
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
    // A 2-slot OCCLUSION pool (VkQueryType OCCLUSION = 0).
    let pool = sync::create_query_pool(&mut d, 0, 2).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_reset_query_pool(&mut d, cb, pool, 0, 2).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, target, [0.0; 4], true, None).unwrap();
    // Query 0: a full-frame draw with no scissor covers the whole 64x64 = 4096 samples.
    record::cmd_begin_query(&mut d, cb, pool, 0).unwrap();
    record::cmd_draw(&mut d, cb, 6, 1, 0, 0).unwrap();
    record::cmd_end_query(&mut d, cb, pool, 0).unwrap();
    // Query 1: the same draw, scissored to the left half → 32x64 = 2048 samples.
    record::cmd_set_scissor(&mut d, cb, 0, 0, 32, 64).unwrap();
    record::cmd_begin_query(&mut d, cb, pool, 1).unwrap();
    record::cmd_draw(&mut d, cb, 6, 1, 0, 0).unwrap();
    record::cmd_end_query(&mut d, cb, pool, 1).unwrap();
    d.end_render_pass(cb).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    let mut out = [0u8; 8];
    assert!(
        sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 8, true, false, false, false)
            .unwrap()
    );
    assert_eq!(
        u64::from_le_bytes(out),
        4096,
        "a visible full-frame draw counts every sample"
    );
    let mut out = [0u8; 8];
    assert!(
        sync::get_query_pool_results(&d, pool, 1, 1, &mut out, 8, true, false, false, false)
            .unwrap()
    );
    assert_eq!(
        u64::from_le_bytes(out),
        2048,
        "a scissor-clipped draw counts only the admitted samples"
    );
}

#[test]
fn occlusion_query_zero_when_fully_scissored_or_no_draw() {
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
    let pool = sync::create_query_pool(&mut d, 0, 2).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_reset_query_pool(&mut d, cb, pool, 0, 2).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, target, [0.0; 4], true, None).unwrap();
    // Query 0: a draw fully scissored to an empty 0x0 rect passes zero samples (fully occluded).
    record::cmd_set_scissor(&mut d, cb, 0, 0, 0, 0).unwrap();
    record::cmd_begin_query(&mut d, cb, pool, 0).unwrap();
    record::cmd_draw(&mut d, cb, 6, 1, 0, 0).unwrap();
    record::cmd_end_query(&mut d, cb, pool, 0).unwrap();
    // Query 1: no draw at all in the scope → zero samples.
    record::cmd_begin_query(&mut d, cb, pool, 1).unwrap();
    record::cmd_end_query(&mut d, cb, pool, 1).unwrap();
    d.end_render_pass(cb).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    for q in 0..2 {
        let mut out = [0u8; 8];
        assert!(sync::get_query_pool_results(
            &d, pool, q, 1, &mut out, 8, true, false, false, false
        )
        .unwrap());
        assert_eq!(
            u64::from_le_bytes(out),
            0,
            "a fully-occluded / no-draw occlusion query reports 0"
        );
    }
}

#[test]
fn copy_query_pool_results_writes_dst_buffer_at_submit() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let pool = sync::create_query_pool(&mut d, 2, 1).unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let dst_ir = buf_ir(&d, dst);

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_reset_query_pool(&mut d, cb, pool, 0, 1).unwrap();
    record::cmd_write_timestamp(&mut d, cb, pool, 0).unwrap();
    // 32-bit results, no availability, stride 4.
    record::cmd_copy_query_pool_results(&mut d, cb, pool, 0, 1, dst, 0, 4, false, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // On completion the resolved timestamp is written into the destination buffer (trailing WriteBuffer).
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(_), Cmd::WriteBuffer {
            id,
            offset: 0,
            data,
        }] => {
            assert_eq!(*id, dst_ir);
            assert_eq!(u32::from_le_bytes([data[0], data[1], data[2], data[3]]), 1);
        }
        other => panic!("expected [Submit, WriteBuffer], got {other:?}"),
    }
}
