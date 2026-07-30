use super::*;

/// The device→host mapped-memory readback, end-to-end: a device op produces bytes in a buffer bound to
/// host-visible memory, then `vkMapMemory`'s readback (via `create::read_mapped`) makes those DEVICE bytes
/// observable through the mapped pointer — the very bug this fixes. The host staging is never written, so
/// if the readback did nothing the map would still show zeros; asserting it equals the device-computed
/// result proves the pointer now reflects GPU output.
///
/// Device work: fill `src` with a pattern, then copy `src`→`dst` (`dst` is the mapped buffer). Both run on
/// the reference `CpuExecutor` through the full runtime pipeline, so the bytes in `dst` are genuinely
/// device-produced, not a host echo.
#[test]
fn map_memory_reflects_device_output_end_to_end() {
    use hl_gpu::BufferId;

    // Permissive caps so the lowering runs against the CPU oracle (as in the graphics test above).
    let exec = CpuExecutor::new();
    let session = Session::new(
        Limits::from_capabilities(Capabilities::permissive_fixture("hl-cpu-mapreadback")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = Instance::new(HL_API_VERSION);
    let mut d = inst.create_device();

    const N: u64 = 16; // 4 × u32
    const PATTERN: u32 = 0xDEAD_BEEF;

    // A transfer src (fillable) and a transfer dst bound to host-visible memory (its staging stays zero).
    let src = create::create_buffer(
        &mut d,
        &mut sink,
        vk_buffer_usage::TRANSFER_SRC | vk_buffer_usage::TRANSFER_DST,
        N,
    )
    .unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, N).unwrap();
    let dst_ir = d.buffers.get(&dst).unwrap().ir_id;
    let mem = d.allocate_memory(N).unwrap();
    create::bind_buffer_memory(&mut d, dst, mem, 0).unwrap();

    // Device work: fill src with the pattern, then copy src → dst. No host write to dst's staging.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_fill_buffer(&mut d, cb, src, 0, N, PATTERN).unwrap();
    record::cmd_copy_buffer(&mut d, cb, src, dst, 0, 0, N).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    let expected: Vec<u8> = PATTERN
        .to_le_bytes()
        .iter()
        .copied()
        .cycle()
        .take(N as usize)
        .collect();

    // The device really produced the result (read straight off the runtime resources).
    let device_bytes = sink.read_buffer(BufferId(dst_ir), 0, N as usize).unwrap();
    assert_eq!(
        device_bytes, expected,
        "the device op produced the pattern in dst"
    );

    // Before the readback the host staging is still zero — a plain map would hand back stale bytes.
    assert!(
        d.memories.get(&mem).unwrap().data.iter().all(|&b| b == 0),
        "staging is stale before map"
    );

    // vkMapMemory's device→host readback: refresh the whole mapped allocation with dst's device bytes.
    d.map_memory(mem).unwrap();
    create::read_mapped(&mut d, &mut sink, mem, 0, u64::MAX).unwrap();

    // The mapped staging now reflects the GPU's current contents — exactly the device-computed pattern.
    assert_eq!(
        d.memories.get(&mem).unwrap().data,
        expected,
        "mapped memory reflects device output after the readback"
    );
}

/// The host→device upload survives an UNMAP-before-submit, end-to-end: an app maps a buffer, writes a
/// staging pattern, `vkUnmapMemory`s, and only THEN `vkQueueSubmit`s — the classic real-app staging
/// sequence. The written bytes must land in the DEVICE buffer. Before the fix, `vkUnmapMemory` cleared
/// the mapped flag and the submit's flush skipped the memory, silently dropping the upload. Here we read
/// the device buffer straight off the runtime resources after submit and assert it holds the pattern.
#[test]
fn unmapped_host_write_reaches_the_device_end_to_end() {
    use hl_gpu::BufferId;

    let exec = CpuExecutor::new();
    let session = Session::new(
        Limits::from_capabilities(Capabilities::permissive_fixture("hl-cpu-unmapupload")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = Instance::new(HL_API_VERSION);
    let mut d = inst.create_device();

    const N: u64 = 16;
    let payload: Vec<u8> = (0..N as u8).collect();

    // A host-visible buffer the app stages into (TRANSFER_DST so the device accepts the write).
    let buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, N).unwrap();
    let buf_ir = d.buffers.get(&buf).unwrap().ir_id;
    let mem = d.allocate_memory(N).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();

    // map → write → UNMAP, all BEFORE any submit.
    d.map_memory(mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &payload).unwrap();
    d.unmap_memory(mem);

    // An empty submit — the only device traffic is the pending host→device flush of the unmapped write.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // The device buffer now holds exactly the bytes the app wrote before unmapping.
    let device_bytes = sink.read_buffer(BufferId(buf_ir), 0, N as usize).unwrap();
    assert_eq!(
        device_bytes, payload,
        "the pre-unmap host write reached the device buffer"
    );
}

/// `vkQueuePresentKHR`'s wl_shm marshalling readback, end-to-end: `present::read_presented_xrgb` reads a
/// presented swapchain image back through the REAL device→host port and converts it to the
/// `WL_SHM_FORMAT_XRGB8888` byte order (`[B,G,R,X]`, top-left origin) a `wl_surface` attach wants. Using an
/// R8G8B8A8 (Rgba8) swapchain proves the R↔B channel REORDER actually happens (a Bgra8 source would pass
/// through and hide the swap), and that the X byte is forced to 0xFF (never mistaken for the all-zero
/// readback-failed fill). This is the last untested WSI service entry point.
#[test]
fn present_xrgb_readback_reorders_channels_end_to_end() {
    const W: u32 = 4;
    const H: u32 = 4;

    let exec = CpuExecutor::new();
    let session = Session::new(
        Limits::from_capabilities(Capabilities::permissive_fixture("hl-cpu-xrgb")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = Instance::new(HL_API_VERSION);
    let mut d = inst.create_device();

    // An RGBA8 surface → the native readback is [R,G,B,A]; the XRGB convert must swap R↔B to [B,G,R,X].
    let surface =
        present::create_surface(&mut d, &mut sink, W, H, vk_format::R8G8B8A8_UNORM, None).unwrap();
    let swapchain = present::create_swapchain(&mut d, &mut sink, surface, 2).unwrap();
    let images = d.swapchain_images(swapchain).unwrap();

    // Clear to a color with three DISTINCT channels so the reorder is unambiguous: R=0.2, G=0.4, B=0.6.
    let clear = [0.2f32, 0.4, 0.6, 1.0];
    let r = (0.2f32 * 255.0).round() as u8;
    let g = (0.4f32 * 255.0).round() as u8;
    let b = (0.6f32 * 255.0).round() as u8;

    let idx = d.acquire_next_image(swapchain).unwrap();
    let acquired = images[idx as usize];
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, acquired, clear, true, None).unwrap();
    d.end_render_pass(cb).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
    present::queue_present(&mut d, &mut sink, swapchain, idx, None).unwrap();

    let (xrgb, w, h) = present::read_presented_xrgb(&mut d, &mut sink, swapchain, idx).unwrap();
    assert_eq!((w, h), (W, H));
    assert_eq!(xrgb.len(), (W * H * 4) as usize);
    // Every texel is XRGB little-endian [B, G, R, 0xFF] — the R↔B swap and forced-opaque X.
    for px in xrgb.chunks_exact(4) {
        assert_eq!(
            px,
            [b, g, r, 0xFF],
            "XRGB reorder [B,G,R,X] of the RGBA source"
        );
    }
}
