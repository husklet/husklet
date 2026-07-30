use super::*;

// =====================================================================================================
// dangling / never-created handles across destroy / bind / cmd entrypoints
// =====================================================================================================

#[test]
fn dangling_handles_across_entrypoints_are_typed_errors_or_safe_noops() {
    let mut d = dev();
    let mut s = sink();
    let cb = recording_cb(&mut d);
    // Bind / cmd calls against never-created handles → typed Invalid (no panic).
    assert!(matches!(
        record::cmd_bind_pipeline(&mut d, cb, 0xdead),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_bind_index_buffer(&mut d, cb, 0xdead, 0, 1),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        record::cmd_copy_buffer(&mut d, cb, 0xdead, 0xbeef, 0, 0, 4),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        create::create_compute_pipeline(&mut d, &mut s, 0xdead, "main"),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        d.image_subresource_layout(0xdead),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        present::create_swapchain(&mut d, &mut s, 0xdead, 2),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        d.swapchain_images(0xdead),
        Err(GpuError::Invalid(_))
    ));
    // Destroy of a never-created handle is a defined safe no-op (VK_NULL_HANDLE semantics).
    create::destroy_buffer(&mut d, &mut s, 0xdead).unwrap();
    create::PipelineCache::destroy(&mut d, 0xdead);
    d.destroy_event(0xdead);
    d.destroy_semaphore(0xdead);
    d.destroy_query_pool(0xdead);
    // A valid resource + command still works after the barrage.
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0).unwrap();
}

// =====================================================================================================
// out-of-range descriptor indices + vertex/index buffer offsets beyond the allocation (safe forward)
// =====================================================================================================

#[test]
fn out_of_range_indices_and_offsets_do_not_panic_then_valid() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(
        &mut d,
        &mut s,
        vk_buffer_usage::VERTEX_BUFFER | vk_buffer_usage::INDEX_BUFFER,
        16,
    )
    .unwrap();
    let ir = buf_ir(&d, buf);
    let cb = recording_cb(&mut d);
    // A vertex/index-buffer offset far beyond the 16-byte allocation is forwarded to the IR (the shim is a
    // thin lowering seam — the executor validates the fetch); it must NOT panic or corrupt the recording.
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, u64::MAX).unwrap();
    record::cmd_bind_index_buffer(&mut d, cb, buf, u64::MAX, 1).unwrap();
    // A huge descriptor binding index is just a map key (no panic); the write is retained.
    let layout = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: u32::MAX,
        descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
        descriptor_count: 1,
        stage_flags: 0,
    }]);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set, u32::MAX, buf, 0, 16).unwrap();
    assert_eq!(
        d.descriptor_sets
            .get(&set)
            .unwrap()
            .buffers
            .get(&(u32::MAX, 0)),
        Some(&(buf, 0, 16))
    );
    // A valid, in-range bind still records against the real ir id.
    record::cmd_bind_vertex_buffer(&mut d, cb, 1, buf, 0).unwrap();
    use hl_gpu::protocol::model::command::Enc;
    assert!(d
        .command_buffers
        .get(&cb)
        .unwrap()
        .enc
        .iter()
        .any(|e| matches!(e, Enc::SetVertexBuffer { buffer, offset: 0, .. } if *buffer == ir)));
}

// =====================================================================================================
// double-free / use-after-free of handles (survives; use-after-free is a typed error)
// =====================================================================================================

#[test]
fn double_free_and_use_after_free_survive() {
    let mut d = dev();
    // Event: double-destroy is a no-op; a use-after-free is a typed Invalid.
    let e = d.create_event();
    d.destroy_event(e);
    d.destroy_event(e); // double free — no panic
    assert!(matches!(d.set_event(e, true), Err(GpuError::Invalid(_))));
    assert!(matches!(d.event_status(e), Err(GpuError::Invalid(_))));
    // Pipeline cache: double-destroy no-op; use-after-free → Invalid.
    let c = create::PipelineCache::create(&mut d, &[]);
    create::PipelineCache::destroy(&mut d, c);
    create::PipelineCache::destroy(&mut d, c);
    assert!(matches!(
        create::PipelineCache::data(&d, c),
        Err(GpuError::Invalid(_))
    ));
    // Descriptor-update template: double-destroy no-op.
    let t = create::create_descriptor_update_template(&mut d, 0, vec![]).unwrap();
    d.destroy_descriptor_update_template(t);
    d.destroy_descriptor_update_template(t);
    assert!(matches!(
        create::update_descriptor_set_with_template(&mut d, 0, t, &[]),
        Err(GpuError::Invalid(_))
    ));
    // Fresh objects of each kind still work.
    let e2 = d.create_event();
    d.set_event(e2, true).unwrap();
    assert!(d.event_status(e2).unwrap());
}

// =====================================================================================================
// submit a command buffer that references a destroyed resource (survives; no fake state)
// =====================================================================================================

#[test]
fn submit_command_buffer_referencing_destroyed_resource_survives() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0).unwrap(); // encoder now holds buf's ir id
    d.end_command_buffer(cb).unwrap();
    // Destroy the referenced buffer AFTER recording but BEFORE submit — the encoder still names its ir.
    create::destroy_buffer(&mut d, &mut s, buf).unwrap();
    // The submit must not panic; the frame ships the (now dangling-ir) SetVertexBuffer for the executor
    // to reject — the shim survives and does not fabricate resource state.
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    // A fresh buffer + a fresh command buffer + submit still works.
    let b2 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let cb2 = d.allocate_command_buffer();
    d.begin_command_buffer(cb2, false).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb2, 0, b2, 0).unwrap();
    d.end_command_buffer(cb2).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb2], None).unwrap();
}
