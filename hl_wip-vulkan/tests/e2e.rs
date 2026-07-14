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
use hl_vulkan::service::{create, record, submit};

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
    let mem = create::allocate_memory(&mut d, vsize);
    create::bind_buffer_memory(&mut d, vbuf, mem, 0).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &verts).unwrap();

    // record: begin render pass (clear) → bind pipeline → bind vertex buffer → draw 3 → end pass.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, target, clear, true).unwrap();
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
    let mem = create::allocate_memory(&mut d, N);
    create::bind_buffer_memory(&mut d, dst, mem, 0).unwrap();

    // Device work: fill src with the pattern, then copy src → dst. No host write to dst's staging.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
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
