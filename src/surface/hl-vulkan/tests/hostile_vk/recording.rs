use super::*;

// =====================================================================================================
// push-constant offset/size overflow (REGRESSION: `resize` to multiple GiB aborts the host)
// =====================================================================================================

#[test]
fn push_constants_out_of_range_rejected_then_valid() {
    let mut d = dev();
    let cb = recording_cb(&mut d);
    // offset+size past `maxPushConstantsSize` (4096) previously resized the block to ~4 GiB and aborted.
    assert!(matches!(
        record::cmd_push_constants(&mut d, cb, 0xFFFF_FFF0, &[0u8; 16]),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_push_constants(&mut d, cb, 0, &[0u8; 8192]),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_push_constants(&mut d, cb, 4096, &[0u8; 4]),
        Err(GpuError::Invalid(_))
    ));
    // A valid push within the range still records at its offset.
    record::cmd_push_constants(&mut d, cb, 0, &[7u8; 128]).unwrap();
    assert_eq!(
        d.command_buffers.get(&cb).unwrap().push_constants.len(),
        128
    );
}

// =====================================================================================================
// vkCmdBindDescriptorSets firstSet overflow (REGRESSION: `first_set + i` u32 add-overflow)
// =====================================================================================================

#[test]
fn bind_descriptor_sets_first_set_overflow_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let layout = d.create_descriptor_set_layout(vec![]);
    let pool = d.create_descriptor_pool(4);
    let sa = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    let sb = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    let cb = recording_cb(&mut d);
    // firstSet == u32::MAX with >1 set previously overflow-panicked `first_set + i as u32`. It now
    // saturates (a documented safe handling — a real firstSet is bounded by maxBoundDescriptorSets).
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, u32::MAX, &[sa, sb], &[]).unwrap();
    // An unknown set in the batch is skipped (not a panic); the valid `sa` at position 1 lands at set 1.
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[0xdead, sa], &[]).unwrap();
    let sets: Vec<u32> = s
        .commands()
        .filter_map(|c| match c {
            Cmd::CreateBindGroup(_, desc) => Some(desc.set),
            _ => None,
        })
        .collect();
    // The two u32::MAX-saturated binds, then the valid `sa` at set index 1 (the 0xdead at index 0 skipped).
    assert_eq!(sets, vec![u32::MAX, u32::MAX, 1]);
}

// =====================================================================================================
// vkCmdSet*EXT per-attachment array out-of-range (REGRESSION: multi-GiB `resize` aborts the host)
// =====================================================================================================

#[test]
fn dynamic_attachment_array_out_of_range_rejected_then_valid() {
    let mut d = dev();
    let cb = recording_cb(&mut d);
    // `first` near u32::MAX previously resized the state vector to multiple GiB and aborted the host.
    let r = record::set_dynamic_attachment_array(&mut d, cb, u32::MAX, &[1u32], |ds| {
        &mut ds.color_blend_enables
    });
    assert!(matches!(r, Err(GpuError::Invalid(_))));
    let r2 = record::set_dynamic_attachment_array(&mut d, cb, 4, &[1u32; 8], |ds| {
        &mut ds.color_write_masks
    });
    assert!(matches!(r2, Err(GpuError::Invalid(_))));
    // A valid attachment range (within maxColorAttachments == 8) still records.
    record::set_dynamic_attachment_array(&mut d, cb, 0, &[1, 0], |ds| &mut ds.color_blend_enables)
        .unwrap();
    assert_eq!(
        d.command_buffers
            .get(&cb)
            .unwrap()
            .dynamic
            .color_blend_enables,
        vec![1, 0]
    );
}

// =====================================================================================================
// query-result stride overflow (REGRESSION: `count*stride` u64 overflow → multi-EiB Vec / `i*stride` panic)
// =====================================================================================================

#[test]
fn copy_query_pool_results_hostile_stride_is_out_of_bounds_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let pool = sync::create_query_pool(&mut d, 2, 4).unwrap(); // TIMESTAMP-ish, 4 slots
    let dst = create::create_buffer(&mut d, &mut s, vk_buffer_usage::TRANSFER_DST, 256).unwrap();
    let cb = recording_cb(&mut d);
    // A hostile `stride` near u64::MAX previously made `count * stride.max(per)` overflow and later
    // aborted the host on a multi-EiB `vec![0u8; dst_size]`; now a truthful OutOfBounds.
    assert!(matches!(
        record::cmd_copy_query_pool_results(&mut d, cb, pool, 0, 4, dst, 0, u64::MAX, false, false),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        record::cmd_copy_query_pool_results(
            &mut d,
            cb,
            pool,
            0,
            2,
            dst,
            0,
            u64::MAX / 2,
            true,
            true
        ),
        Err(GpuError::OutOfBounds)
    ));
    // A valid copy (span fits the 256-byte dst) still records.
    record::cmd_copy_query_pool_results(&mut d, cb, pool, 0, 4, dst, 0, 4, false, false).unwrap();
}

#[test]
fn get_query_pool_results_hostile_stride_does_not_panic_then_valid() {
    let mut d = dev();
    let pool = sync::create_query_pool(&mut d, 2, 4).unwrap();
    let mut out = [0u8; 32];
    // A hostile `stride` near u64::MAX previously overflow-panicked `i * stride as usize` for count>1.
    // Elements that land outside `out` are simply skipped — no panic, returns a defined readiness bool.
    let _ = sync::get_query_pool_results(
        &d,
        pool,
        0,
        4,
        &mut out,
        u64::MAX,
        false,
        true,
        false,
        false,
    )
    .unwrap();
    let _ = sync::get_query_pool_results(
        &d,
        pool,
        0,
        3,
        &mut out,
        u64::MAX / 2,
        true,
        false,
        true,
        false,
    )
    .unwrap();
    // A valid readback (stride 8, availability) still succeeds.
    let mut ok = [0u8; 32];
    let _ =
        sync::get_query_pool_results(&d, pool, 0, 2, &mut ok, 8, false, true, true, false).unwrap();
}

// =====================================================================================================
// bad descriptor writes — type mismatch against the set layout (safe handling, no panic)
// =====================================================================================================

#[test]
fn descriptor_type_mismatch_writes_do_not_panic_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
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
    // A layout whose binding 0 is a COMBINED_IMAGE_SAMPLER — but the app writes a BUFFER to it.
    let layout = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: 0,
        descriptor_type: vk_descriptor_type::COMBINED_IMAGE_SAMPLER,
        descriptor_count: 1,
        stage_flags: 0,
    }]);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    // Mismatched type writes are recorded into their kind's map (the executor validates types); no panic.
    create::update_descriptor_buffer(&mut d, set, 0, buf, 0, 64).unwrap();
    create::update_descriptor_image(&mut d, set, 0, Some(img), None).unwrap();
    // Binding the mismatched set still produces a bind group without crashing.
    let cb = recording_cb(&mut d);
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[set], &[]).unwrap();
    assert!(s.commands().any(|c| matches!(c, Cmd::CreateBindGroup(..))));
    // A correctly-typed write to a matching binding still lowers to a buffer bind entry.
    let layout2 = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: 0,
        descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
        descriptor_count: 1,
        stage_flags: 0,
    }]);
    let pool2 = d.create_descriptor_pool(1);
    let set2 = create::allocate_descriptor_set(&mut d, pool2, layout2, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set2, 0, buf, 0, 64).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[set2], &[]).unwrap();
}
