use super::*;

#[test]
fn submit_with_fence_signals_and_wait_lowers_to_command_sink_wait() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let fence = create::create_fence(&mut d, &mut sink, false).unwrap(); // CreateFence(ir 1)
    assert!(matches!(sink.batches[0][0], Cmd::CreateFence(1)));

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], Some(fence)).unwrap();

    // the (empty) command buffer's Submit signals the fence at timeline value 1.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => assert_eq!(cbuf.signal, Some((1, 1))),
        other => panic!("expected signalling Submit, got {other:?}"),
    }
    // the fence wait lowers to a real CommandSink::wait on that timeline value.
    Device::wait_for_fence(&mut d, &mut sink, fence).unwrap();
    assert_eq!(sink.waits, vec![(FenceId(1), 1)]);
}

#[test]
fn mapped_memory_flushes_as_write_buffer_at_submit() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::UNIFORM_BUFFER, 256).unwrap();
    let mem = d.allocate_memory(256).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    d.map_memory(mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[1, 2, 3, 4]).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // the persistently-mapped buffer flushes as a WriteBuffer immediately before the Submit.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset, data }, Cmd::Submit(_)] => {
            assert_eq!((*id, *offset), (1, 0));
            assert_eq!(data.len(), 256);
            assert_eq!(&data[..4], &[1, 2, 3, 4]);
        }
        other => panic!("expected [WriteBuffer, Submit], got {other:?}"),
    }
}

#[test]
fn arena_memory_flushes_every_bound_buffer_at_submit() {
    // Regression: a single allocation sub-allocated into MANY buffers (the gpu-alloc/VMA arena pattern
    // that blade/GPUI uses — hundreds of uniform/storage/vertex buffers in one HOST_COHERENT block).
    // Tracking only the last-bound buffer silently dropped the host→device flush of every OTHER buffer,
    // so their device bytes stayed zero — the vertex shader read a zero viewport/zero instance data,
    // every draw collapsed off-screen, and the target kept only its clear (a fully blank Zed frame).
    // Every bound buffer must now flush its own footprint.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // Three buffers packed into one 3072-byte allocation at distinct offsets (globals, instances, verts).
    let globals =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::UNIFORM_BUFFER, 16).unwrap();
    let instances =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 32).unwrap();
    let verts =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let g_ir = d.buffers.get(&globals).unwrap().ir_id;
    let i_ir = d.buffers.get(&instances).unwrap().ir_id;
    let v_ir = d.buffers.get(&verts).unwrap().ir_id;

    let mem = d.allocate_memory(3072).unwrap();
    create::bind_buffer_memory(&mut d, globals, mem, 0).unwrap();
    create::bind_buffer_memory(&mut d, instances, mem, 1024).unwrap();
    create::bind_buffer_memory(&mut d, verts, mem, 2048).unwrap(); // last-bound: the ONLY one the old model kept
    d.map_memory(mem).unwrap();

    // The app memcpys each buffer's data at its own offset in the mapped arena.
    create::write_mapped(&mut d, mem, 0, &[0xAA; 16]).unwrap(); // globals
    create::write_mapped(&mut d, mem, 1024, &[0xBB; 32]).unwrap(); // instances
    create::write_mapped(&mut d, mem, 2048, &[0xCC; 16]).unwrap(); // verts

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // Collect the (id, first-byte) of every WriteBuffer flushed this submit.
    let writes: Vec<(u32, u8)> = sink
        .batches
        .last()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            Cmd::WriteBuffer { id, offset, data } => {
                assert_eq!(*offset, 0, "each arena buffer flushes to its own offset 0");
                Some((*id, data[0]))
            }
            _ => None,
        })
        .collect();

    // All THREE buffers flush — not just the last-bound `verts` — each carrying its own bytes.
    assert!(
        writes.contains(&(g_ir, 0xAA)),
        "globals buffer must flush its own bytes, got {writes:?}"
    );
    assert!(
        writes.contains(&(i_ir, 0xBB)),
        "instances buffer must flush its own bytes, got {writes:?}"
    );
    assert!(
        writes.contains(&(v_ir, 0xCC)),
        "verts buffer must flush its own bytes, got {writes:?}"
    );
    assert_eq!(
        writes.len(),
        3,
        "exactly one flush per bound buffer (no drops, no double-writes)"
    );

    // Sizes: each WriteBuffer carries exactly its buffer's footprint.
    for c in sink.batches.last().unwrap() {
        if let Cmd::WriteBuffer { id, data, .. } = c {
            let want = if *id == g_ir || *id == v_ir { 16 } else { 32 };
            assert_eq!(
                data.len(),
                want,
                "buffer {id} flushes its own footprint length"
            );
        }
    }
}

#[test]
fn unmapped_memory_still_flushes_its_write_at_submit() {
    // The data-loss edge: a real app stages into a mapped buffer, then vkUnmapMemory BEFORE submitting.
    // The upload must survive the unmap and still reach the device as a WriteBuffer at the next submit.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::UNIFORM_BUFFER, 256).unwrap();
    let buf_ir = d.buffers.get(&buf).unwrap().ir_id;
    let mem = d.allocate_memory(256).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    d.map_memory(mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[9, 8, 7, 6]).unwrap();
    d.unmap_memory(mem); // <-- unmap before submit; the write must not be dropped

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // Exactly one WriteBuffer carrying the written bytes flushes despite the unmap.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset, data }, Cmd::Submit(_)] => {
            assert_eq!((*id, *offset), (buf_ir, 0));
            assert_eq!(data.len(), 256);
            assert_eq!(
                &data[..4],
                &[9, 8, 7, 6],
                "the unmapped write reached the device"
            );
        }
        other => panic!("expected [WriteBuffer, Submit] after unmap, got {other:?}"),
    }

    // The pending upload is one-shot: a SECOND submit (no re-map/re-write) flushes nothing more.
    let cb2 = d.allocate_command_buffer();
    d.begin_command_buffer(cb2, false).unwrap();
    d.end_command_buffer(cb2).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb2], None).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(_)] => {}
        other => panic!("expected a bare [Submit] on the second frame, got {other:?}"),
    }
}

#[test]
fn mapped_write_without_unmap_flushes_exactly_once() {
    // No-regression / no-double-write: map → write → submit WITHOUT unmapping must still upload the bytes,
    // and exactly once (the still-mapped path and the pending path are coalesced — a mapped memory yields
    // a single WriteBuffer even if a flush also captured a pending range).
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::UNIFORM_BUFFER, 256).unwrap();
    let mem = d.allocate_memory(256).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    d.map_memory(mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[1, 2, 3, 4]).unwrap();
    // A flush of a sub-range while still mapped captures a pending record too — it must NOT double the write.
    create::capture_pending_upload(&mut d, mem, 0, 4);

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    let writes = sink
        .batches
        .last()
        .unwrap()
        .iter()
        .filter(|c| matches!(c, Cmd::WriteBuffer { .. }))
        .count();
    assert_eq!(
        writes, 1,
        "still-mapped + pending coalesce to a single WriteBuffer (no double-write)"
    );
}

#[test]
fn unmapped_unbound_host_staging_flushes_nothing() {
    // Host-only staging with no buffer bound has no device buffer to upload to; unmapping it must capture
    // nothing (a truthful no-op) so the submit emits no WriteBuffer.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let mem = d.allocate_memory(128).unwrap();
    d.map_memory(mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[1, 2, 3, 4]).unwrap();
    d.unmap_memory(mem);
    assert!(
        d.memories.get(&mem).unwrap().pending_flush.is_none(),
        "unbound staging captures nothing"
    );

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(_)] => {}
        other => panic!("expected a bare [Submit] (no upload), got {other:?}"),
    }
}

#[test]
fn map_memory_reads_bound_buffer_back_over_the_sink() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // A host-visible allocation bound to a real device buffer (ir id 1). The staging bytes are the app's
    // own upload, so reading device output requires a device→host readback.
    let buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let buf_ir = d.buffers.get(&buf).unwrap().ir_id;
    let mem = d.allocate_memory(256).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    d.map_memory(mem).unwrap();

    // Invalidating / mapping a whole bound allocation reads the buffer back over the sink's device→host
    // port — the SAME `read_buffer` cuda's `cuMemcpyDtoH` and GL's `glReadPixels` issue.
    create::read_mapped(&mut d, &mut sink, mem, 0, u64::MAX).unwrap();
    assert_eq!(
        sink.reads,
        vec![(hl_gpu::BufferId(buf_ir), 0, 256)],
        "one whole-buffer readback"
    );

    // A bounded sub-range honours the mapped offset/size (buffer offset = mem offset − bound_offset = 64).
    create::read_mapped(&mut d, &mut sink, mem, 64, 32).unwrap();
    assert_eq!(
        sink.reads.last().copied(),
        Some((hl_gpu::BufferId(buf_ir), 64, 32))
    );
}

#[test]
fn map_memory_of_unbound_host_staging_issues_no_readback() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // Host-only staging: no buffer bound, so there is no readable device source. The readback must be a
    // truthful no-op (never a faked/zero read), leaving the staging as-is.
    let mem = d.allocate_memory(128).unwrap();
    d.map_memory(mem).unwrap();
    create::read_mapped(&mut d, &mut sink, mem, 0, u64::MAX).unwrap();
    assert!(sink.reads.is_empty(), "unbound staging must not read back");
}
