//! End-to-end graphics: a clear + a solid-color triangle driven through the REAL hl-vulkan lowering
//! services, an in-process [`InProcessCommandSink`] over the reference [`CpuExecutor`], and the whole
//! host runtime pipeline — then the RASTERIZED render target is read back and its pixels are asserted.
//!
//! This mirrors `hl-cuda`'s `tests/e2e.rs` (which reads back a COMPUTED vecadd), but for the graphics
//! path:
//!
//!   vkCreateImage(RENDER_TARGET) → vkCreateShaderModule ×2 (SPIR-V) → vkCreateGraphicsPipelines
//!   (with a slot-0 vertex layout + one color target) → vkCreateBuffer(VERTEX) + map/write the triangle
//!   → vkBeginCommandBuffer → vkCmdBeginRenderPass(clear) → vkCmdBindPipeline → vkCmdBindVertexBuffers
//!   → vkCmdDraw(3) → vkCmdEndRenderPass → vkQueueSubmit
//!        └─lowers to─▶ protocol Cmds ─submit─▶ InProcessCommandSink
//!             └▶ runtime validate → account → dispatch → CpuExecutor (clears the target, then
//!                rasterizes the triangle from the vertex buffer's pos+color) → read_texture → assert.
//!
//! HONEST LIMITATION (documented, not papered over): the reference [`CpuExecutor`] advertises only the
//! KERNEL shader payload and does NOT execute a SPIR-V/graphics shader — its render path is a fixed-
//! function rasterizer that fetches each vertex's NDC position (bytes 0..8) and straight-alpha color
//! (bytes 8..24) DIRECTLY from the bound slot-0 vertex buffer (see `hl_gpu` `cpu/service/raster.rs`).
//! So this test proves the full lowering seam + clear + triangle GEOMETRY COVERAGE + vertex color, which
//! is everything the CPU oracle can render; it does not (cannot) execute a real fragment shader. The
//! SPIR-V shader modules are created + forwarded verbatim (the seam keystone) and referenced by the
//! pipeline, but the CPU oracle never runs them. To let the permissive lowering create SPIR-V modules
//! against the KERNEL-only oracle, the sink is built with a full (`Capabilities::full`) capability set
//! rather than negotiating the executor's own narrow advertisement.

use hl_vulkan::adapter::spirv;
use hl_vulkan::model::memory::{vk_buffer_usage, vk_format, vk_image_usage};
use hl_vulkan::result::HL_API_VERSION;
use hl_vulkan::service::{create, present, record, submit};

use hl_gpu::protocol::model::descriptor::{VertexAttr, VertexLayout};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::id::TextureId;
use hl_gpu::{
    Capabilities, CpuExecutor, FakeClock, GlobalLedger, InProcessCommandSink, Limits, Session,
};

/// Pack `[x, y, r, g, b, a]` (6 f32 = 24-byte stride) little-endian — one vertex the CPU rasterizer reads.
fn vertex(x: f32, y: f32, c: [f32; 4]) -> Vec<u8> {
    [x, y, c[0], c[1], c[2], c[3]].iter().flat_map(|f| f.to_le_bytes()).collect()
}

#[test]
fn graphics_triangle_renders_end_to_end_and_reads_back_the_cleared_target_and_coverage() {
    const W: u32 = 8;
    const H: u32 = 8;
    let clear = [0.0f32, 0.0, 1.0, 1.0]; // opaque blue background
    let tri = [1.0f32, 0.0, 0.0, 1.0]; // opaque red triangle

    // --- host side: the reference CPU executor + the in-process sink -------------------------------
    // Build the sink with a permissive capability set (rather than negotiating the executor's own
    // KERNEL-only advertisement) so the real vulkan lowering can create SPIR-V shader modules against
    // the CPU oracle. The oracle still only *rasterizes* (it never runs the shaders) — see the module doc.
    let exec = CpuExecutor::new();
    let session = Session::new(
        Limits::from_capabilities(Capabilities::full("hl-cpu-graphics")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    // --- guest side: the real hl-vulkan driver services -------------------------------------------
    let inst = create::create_instance(HL_API_VERSION);
    let mut d = create::create_device(&inst);

    // vkCreateImage: an RGBA8 color render target (COLOR_ATTACHMENT ⇒ RENDER_TARGET usage).
    let target = create::create_image(
        &mut d,
        &mut sink,
        W,
        H,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
    )
    .unwrap();
    let target_ir = d.images.get(&target).unwrap().ir_id;

    // vkCreateShaderModule ×2 — trivial but valid SPIR-V, forwarded verbatim (the seam keystone). The
    // CPU oracle creates the modules but never executes them.
    let vs =
        create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("vsmain")).unwrap();
    let fs =
        create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("fsmain")).unwrap();

    // vkCreateGraphicsPipelines — one color target matching the attachment, a slot-0 vertex layout the
    // rasterizer fetches positions/colors from (pos @ offset 0, color @ offset 8, stride 24).
    let layout = VertexLayout {
        stride: 24,
        step_mode: 0,
        attrs: vec![
            VertexAttr { location: 0, format: 0, offset: 0 },
            VertexAttr { location: 1, format: 0, offset: 8 },
        ],
    };
    let pipe = create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![layout],
        vec![TextureFormat::Rgba8Unorm],
        None,
        None,
    )
    .unwrap();

    // vkCreateBuffer(VERTEX) + vkAllocateMemory + vkBindBufferMemory + vkMapMemory + write the 3 verts.
    // The persistently-mapped bytes flush as a Cmd::WriteBuffer at vkQueueSubmit.
    let mut verts = Vec::new();
    verts.extend(vertex(0.0, 0.8, tri)); // apex, top-center
    verts.extend(vertex(-0.8, -0.8, tri)); // bottom-left
    verts.extend(vertex(0.8, -0.8, tri)); // bottom-right
    let vsize = verts.len() as u64;
    let vbuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, vsize).unwrap();
    let mem = create::allocate_memory(&mut d, vsize).unwrap();
    create::bind_buffer_memory(&mut d, vbuf, mem, 0).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &verts).unwrap();

    // record: begin render pass (clear) → bind pipeline → bind vertex buffer → draw 3 → end pass.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, target, clear, true, None).unwrap();
    record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, vbuf, 0).unwrap();
    record::cmd_draw(&mut d, cb, 3, 1, 0, 0).unwrap();
    record::cmd_end_render_pass(&mut d, cb).unwrap();
    record::end(&mut d, cb).unwrap();

    // vkQueueSubmit — the whole frame (WriteBuffer flush + the render-pass Submit) goes to the executor.
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // --- readback: pull the rasterized render target's pixels straight off the runtime resources ----
    let mut pixels = vec![0u8; (W * H * 4) as usize];
    sink.executor()
        .read_texture(sink.resources(), TextureId(target_ir), &mut pixels)
        .expect("read back the render target");

    let texel = |x: u32, y: u32| -> [u8; 4] {
        let o = ((y * W + x) * 4) as usize;
        [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
    };

    // The draw actually reached the executor (not a silently-skipped no-op).
    assert_eq!(sink.executor().draws, 1, "exactly one draw rasterized");

    // The center pixel is covered by the triangle → the red vertex color.
    assert_eq!(texel(W / 2, H / 2), [255, 0, 0, 255], "triangle covers the center (red)");
    // The top-left corner is outside the triangle → still the blue clear color.
    assert_eq!(texel(0, 0), [0, 0, 255, 255], "corner keeps the clear color (blue)");
    // The bottom-left corner (below-left of the triangle's left edge) is also uncovered.
    assert_eq!(texel(0, H - 1), [0, 0, 255, 255], "bottom-left corner keeps the clear color");
}

/// The WSI swapchain full loop, end-to-end at the DRIVER level — the Vulkan analog of the
/// weston-simple-egl GL milestone, proving a presented swapchain image is a REAL, readable-back render
/// target through our IR + the reference `CpuExecutor`:
///
///   vkCreateSurfaceKHR → vkCreateSwapchainKHR (emits a real `CreateTexture(RENDER_TARGET | COPY_SRC)`
///   per presentable image) → vkGetSwapchainImagesKHR (the images' `VkImage` handles) →
///   vkAcquireNextImageKHR (an image index) → record a render pass that CLEARS the acquired image to a
///   known color → vkQueueSubmit (the executor clears the real texture) → vkQueuePresentKHR (`Cmd::Present`
///   naming that texture) → read the PRESENTED image back (`CopyTextureToBuffer` + `read_buffer`) and
///   assert the known pixels.
///
/// This proves the swapchain image is genuinely allocated + rendered into + presented + read back (not a
/// reserved host-owned alias): the app rendered into it and the exact bytes come back. The clear color's
/// channels are all distinct so the readback also proves the surface's BGRA texel order. What remains for
/// a LIVE vkcube on-screen is the wayland present-marshalling (attaching the presented image to a
/// compositor buffer) — a separate follow-up, deliberately NOT exercised here.
#[test]
fn swapchain_present_loop_reads_back_the_presented_image_end_to_end() {
    const W: u32 = 8;
    const H: u32 = 8;
    // A known clear color with 4 DISTINCT channels (r,g,b,a) chosen to land on exact bytes.
    let clear = [51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0, 1.0];
    // Surface format is B8G8R8A8_UNORM ⇒ the readback plane is BGRA: bytes [b, g, r, a].
    let expected = [153u8, 102, 51, 255];

    let exec = CpuExecutor::new();
    let session = Session::new(
        Limits::from_capabilities(Capabilities::full("hl-cpu-swapchain")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = create::create_instance(HL_API_VERSION);
    let mut d = create::create_device(&inst);

    // vkCreateSurfaceKHR + vkCreateSwapchainKHR (2 presentable images, real render-target textures).
    let surface = present::create_surface(&mut d, &mut sink, W, H, vk_format::B8G8R8A8_UNORM, 0).unwrap();
    let swapchain = present::create_swapchain(&mut d, &mut sink, surface, 2).unwrap();

    // vkGetSwapchainImagesKHR → the presentable images' VkImage handles.
    let images = present::get_swapchain_images(&d, swapchain).unwrap();
    assert_eq!(images.len(), 2, "the swapchain has its two presentable images");

    // Run a CONTINUOUS present loop for MORE iterations than there are images (a real app's acquire →
    // render → present, repeat). This proves acquisition genuinely round-robins: the returned index must
    // cycle 0,1,0,1,... (NOT stay pinned at image 0, which aborted vkcube's demo_draw after one frame), and
    // each frame's readback must return exactly what THAT frame cleared into THAT acquired image.
    const FRAMES: usize = 5; // > the 2 images, so the cycle wraps twice
    let mut acquired_indices = Vec::new();
    for frame in 0..FRAMES {
        // A per-frame clear color with 4 distinct channels; frame index rides in the blue channel so each
        // frame's presented pixels are provably the ones this frame rendered into the acquired image.
        let clear = [51.0 / 255.0, 102.0 / 255.0, (10 + frame as u32) as f32 / 255.0, 1.0];
        let expected = [(10 + frame) as u8, 102, 51, 255]; // BGRA readback of the above

        // vkAcquireNextImageKHR — the next image in round-robin order.
        let idx = present::acquire_next_image(&mut d, swapchain).unwrap();
        acquired_indices.push(idx);
        let acquired = images[idx as usize];

        // Record a render pass that CLEARS the acquired image to this frame's color, then submit — the
        // executor clears the image's REAL backing texture (fails if the image were not a real texture).
        let cb = record::allocate_command_buffer(&mut d);
        record::begin(&mut d, cb, false).unwrap();
        record::cmd_begin_render_pass(&mut d, cb, acquired, clear, true, None).unwrap();
        record::cmd_end_render_pass(&mut d, cb).unwrap();
        record::end(&mut d, cb).unwrap();
        submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

        // vkQueuePresentKHR — presents the rendered image (Cmd::Present names its real texture id) and
        // returns it to the pool so the next acquire can advance.
        present::queue_present(&mut d, &mut sink, swapchain, idx).unwrap();

        // Read the PRESENTED image back and assert it is exactly what THIS frame cleared into THIS image.
        let pixels = present::read_presented_image(&mut d, &mut sink, swapchain, idx).unwrap();
        assert_eq!(pixels.len(), (W * H * 4) as usize, "the whole presented image plane came back");
        let texel = |x: u32, y: u32| -> [u8; 4] {
            let o = ((y * W + x) * 4) as usize;
            [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
        };
        for y in 0..H {
            for x in 0..W {
                assert_eq!(texel(x, y), expected, "frame {frame} presented image (idx {idx}) texel ({x},{y})");
            }
        }
    }

    // The acquire indices genuinely cycled through the 2 images (0,1,0,1,0) — NOT pinned at 0.
    assert_eq!(acquired_indices, vec![0, 1, 0, 1, 0], "acquire round-robins across the swapchain images");
    assert!(acquired_indices.contains(&1), "more than image 0 was acquired (the round-robin fix)");
}

/// vkcube's exact per-frame draw loop: each swapchain image has ONE command buffer recorded ONCE (a
/// reusable buffer, no `ONE_TIME_SUBMIT`) that is re-submitted every frame, and a per-frame fence the
/// submit signals and the next frame waits+resets. This proves the continuous multi-frame present loop:
///
///   vkWaitForFences(per-frame fence) → vkResetFences → vkAcquireNextImageKHR → vkQueueSubmit(re-submit
///   the pre-recorded buffer, signal the fence) → vkWaitForFences(fence is now signaled) → vkQueuePresentKHR
///
/// FAIL-BEFORE / PASS-AFTER: before the fix, `vkQueueSubmit` left a submitted command buffer stuck in
/// `Pending` forever, so the SECOND (and every later) re-submit of a pre-recorded per-image buffer failed
/// with `VK_ERROR_INITIALIZATION_FAILED` — exactly vkcube's `demo_draw` abort at cube.c:1093 after ~1
/// presented frame. The reusable buffer must return to `Executable` once its (synchronous) submit
/// completes so the loop keeps running.
#[test]
fn vkcube_style_multiframe_fence_and_resubmit_loop() {
    const W: u32 = 8;
    const H: u32 = 8;

    let exec = CpuExecutor::new();
    let session = Session::new(
        Limits::from_capabilities(Capabilities::full("hl-cpu-vkcube-loop")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = create::create_instance(HL_API_VERSION);
    let mut d = create::create_device(&inst);

    let surface = present::create_surface(&mut d, &mut sink, W, H, vk_format::B8G8R8A8_UNORM, 0).unwrap();
    let swapchain = present::create_swapchain(&mut d, &mut sink, surface, 2).unwrap();
    let images = present::get_swapchain_images(&d, swapchain).unwrap();

    // Record ONE reusable command buffer PER swapchain image, ONCE, up front — the vkcube setup pattern.
    let clear = [51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0, 1.0];
    let per_image_cbs: Vec<_> = images
        .iter()
        .map(|&img| {
            let cb = record::allocate_command_buffer(&mut d);
            record::begin(&mut d, cb, false).unwrap(); // NOT one-time-submit → re-submittable every frame
            record::cmd_begin_render_pass(&mut d, cb, img, clear, true, None).unwrap();
            record::cmd_end_render_pass(&mut d, cb).unwrap();
            record::end(&mut d, cb).unwrap();
            cb
        })
        .collect();

    // Two per-frame fences (FRAME_LAG = 2), created SIGNALED so the first wait passes — vkcube's model.
    let fences =
        [create::create_fence(&mut d, &mut sink, true).unwrap(), create::create_fence(&mut d, &mut sink, true).unwrap()];

    // Run more frames than there are images OR fences, so both cycles wrap several times.
    const FRAMES: usize = 8;
    for frame in 0..FRAMES {
        let fence = fences[frame % fences.len()];

        // vkWaitForFences → the per-frame fence (signaled from its prior frame, or created signaled).
        submit::wait_for_fence(&mut d, &mut sink, fence).unwrap();
        assert!(submit::fence_status(&d, fence).unwrap(), "frame {frame}: fence is signaled after wait");
        // vkResetFences → back to unsignaled before this frame's submit re-arms it.
        submit::reset_fence(&mut d, fence).unwrap();
        assert!(!submit::fence_status(&d, fence).unwrap(), "frame {frame}: fence unsignaled after reset");

        // vkAcquireNextImageKHR → the next image round-robin.
        let idx = present::acquire_next_image(&mut d, swapchain).unwrap();
        let cb = per_image_cbs[idx as usize];

        // vkQueueSubmit → RE-SUBMIT that image's pre-recorded buffer, signaling this frame's fence. Before
        // the fix this errored on every frame after the buffer's first submit.
        submit::queue_submit(&mut d, &mut sink, &[cb], Some(fence)).unwrap();

        // vkWaitForFences → the submit's signal is now observable (synchronous executor).
        submit::wait_for_fence(&mut d, &mut sink, fence).unwrap();
        assert!(submit::fence_status(&d, fence).unwrap(), "frame {frame}: fence signaled by its submit");

        // vkQueuePresentKHR → present the rendered image; returns it to the pool for the next acquire.
        present::queue_present(&mut d, &mut sink, swapchain, idx).unwrap();
    }
}

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
        Limits::from_capabilities(Capabilities::full("hl-cpu-mapreadback")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = create::create_instance(HL_API_VERSION);
    let mut d = create::create_device(&inst);

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
    let mem = create::allocate_memory(&mut d, N).unwrap();
    create::bind_buffer_memory(&mut d, dst, mem, 0).unwrap();

    // Device work: fill src with the pattern, then copy src → dst. No host write to dst's staging.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_fill_buffer(&mut d, cb, src, 0, N, PATTERN).unwrap();
    record::cmd_copy_buffer(&mut d, cb, src, dst, 0, 0, N).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    let expected: Vec<u8> = PATTERN.to_le_bytes().iter().copied().cycle().take(N as usize).collect();

    // The device really produced the result (read straight off the runtime resources).
    let device_bytes = sink.read_buffer(BufferId(dst_ir), 0, N as usize).unwrap();
    assert_eq!(device_bytes, expected, "the device op produced the pattern in dst");

    // Before the readback the host staging is still zero — a plain map would hand back stale bytes.
    assert!(d.memories.get(&mem).unwrap().data.iter().all(|&b| b == 0), "staging is stale before map");

    // vkMapMemory's device→host readback: refresh the whole mapped allocation with dst's device bytes.
    create::map_memory(&mut d, mem).unwrap();
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
        Limits::from_capabilities(Capabilities::full("hl-cpu-unmapupload")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = create::create_instance(HL_API_VERSION);
    let mut d = create::create_device(&inst);

    const N: u64 = 16;
    let payload: Vec<u8> = (0..N as u8).collect();

    // A host-visible buffer the app stages into (TRANSFER_DST so the device accepts the write).
    let buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, N).unwrap();
    let buf_ir = d.buffers.get(&buf).unwrap().ir_id;
    let mem = create::allocate_memory(&mut d, N).unwrap();
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();

    // map → write → UNMAP, all BEFORE any submit.
    create::map_memory(&mut d, mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &payload).unwrap();
    create::unmap_memory(&mut d, mem);

    // An empty submit — the only device traffic is the pending host→device flush of the unmapped write.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // The device buffer now holds exactly the bytes the app wrote before unmapping.
    let device_bytes = sink.read_buffer(BufferId(buf_ir), 0, N as usize).unwrap();
    assert_eq!(device_bytes, payload, "the pre-unmap host write reached the device buffer");
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
        Limits::from_capabilities(Capabilities::full("hl-cpu-xrgb")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    let inst = create::create_instance(HL_API_VERSION);
    let mut d = create::create_device(&inst);

    // An RGBA8 surface → the native readback is [R,G,B,A]; the XRGB convert must swap R↔B to [B,G,R,X].
    let surface = present::create_surface(&mut d, &mut sink, W, H, vk_format::R8G8B8A8_UNORM, 0).unwrap();
    let swapchain = present::create_swapchain(&mut d, &mut sink, surface, 2).unwrap();
    let images = present::get_swapchain_images(&d, swapchain).unwrap();

    // Clear to a color with three DISTINCT channels so the reorder is unambiguous: R=0.2, G=0.4, B=0.6.
    let clear = [0.2f32, 0.4, 0.6, 1.0];
    let r = (0.2f32 * 255.0).round() as u8;
    let g = (0.4f32 * 255.0).round() as u8;
    let b = (0.6f32 * 255.0).round() as u8;

    let idx = present::acquire_next_image(&mut d, swapchain).unwrap();
    let acquired = images[idx as usize];
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb, false).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, acquired, clear, true, None).unwrap();
    record::cmd_end_render_pass(&mut d, cb).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
    present::queue_present(&mut d, &mut sink, swapchain, idx).unwrap();

    let (xrgb, w, h) = present::read_presented_xrgb(&mut d, &mut sink, swapchain, idx).unwrap();
    assert_eq!((w, h), (W, H));
    assert_eq!(xrgb.len(), (W * H * 4) as usize);
    // Every texel is XRGB little-endian [B, G, R, 0xFF] — the R↔B swap and forced-opaque X.
    for px in xrgb.chunks_exact(4) {
        assert_eq!(px, [b, g, r, 0xFF], "XRGB reorder [B,G,R,X] of the RGBA source");
    }
}
