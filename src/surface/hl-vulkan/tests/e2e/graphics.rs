use super::*;

/// `vkCmdClearDepthStencilImage` clears depth OUTSIDE a render pass, end-to-end through the reference
/// `CpuExecutor`. Proof it took effect: clear the depth image to `D`, then run a depth-tested (compare
/// LESS, depth-write) render pass — with a `LoadOp::Load` depth attachment that PRESERVES the standalone
/// clear — that draws a full-screen quad at z = 0.6 over a blue clear. The depth test is `0.6 < D`:
///   * D = 0.5 ⇒ 0.6 < 0.5 is FALSE ⇒ the quad is occluded by the cleared depth ⇒ the pixel stays BLUE.
///   * D = 1.0 ⇒ 0.6 < 1.0 is TRUE  ⇒ the quad passes ⇒ the pixel is RED.
///
/// The two runs differ ONLY in the depth value the standalone clear wrote, so the color flip proves the
/// clear reached the depth buffer (the former no-op left it untouched — the quad would always draw).
#[test]
fn clear_depth_stencil_image_occludes_a_depth_tested_draw_end_to_end() {
    use hl_gpu::protocol::model::descriptor::DepthState;
    use hl_gpu::protocol::model::enums::compare;

    const W: u32 = 8;
    const H: u32 = 8;
    let clear = [0.0f32, 0.0, 1.0, 1.0]; // opaque blue background
    let quad_color = [1.0f32, 0.0, 0.0, 1.0]; // opaque red quad, drawn at z = 0.6

    // Render the frame with the depth image standalone-cleared to `clear_depth`; return the center pixel.
    let render_with_clear_depth = |clear_depth: f32| -> [u8; 4] {
        // `permissive_fixture` carries only color formats; add the depth format so the depth image
        // (and its depth-clear pass) validate against the runtime.
        let mut caps = Capabilities::permissive_fixture("hl-cpu-depthclear");
        caps.texture_formats |=
            TextureFormat::bits(hl_gpu::protocol::model::capability::DEPTH_FORMATS);
        let exec = CpuExecutor::new();
        let session = Session::new(
            Limits::from_capabilities(caps),
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        let mut sink = InProcessCommandSink::with_session(session, exec);

        let inst = Instance::new(HL_API_VERSION);
        let mut d = inst.create_device();

        // Color render target (RGBA8) + a D32 depth image that is BOTH a depth attachment and a
        // transfer-clear target (DEPTH_STENCIL_ATTACHMENT ⇒ RENDER_TARGET, TRANSFER_DST ⇒ COPY_DST).
        let target = create::create_image(
            &mut d,
            &mut sink,
            W,
            H,
            vk_format::R8G8B8A8_UNORM,
            vk_image_usage::COLOR_ATTACHMENT,
            1,
        )
        .unwrap();
        let target_ir = d.images.get(&target).unwrap().ir_id;
        let depth = create::create_image(
            &mut d,
            &mut sink,
            W,
            H,
            vk_format::D32_SFLOAT,
            vk_image_usage::DEPTH_STENCIL_ATTACHMENT | vk_image_usage::TRANSFER_DST,
            1,
        )
        .unwrap();

        let vs = create::create_shader_module_words(
            &mut d,
            &mut sink,
            spirv::Module::sample_compute("vsmain"),
        )
        .unwrap();
        let fs = create::create_shader_module_words(
            &mut d,
            &mut sink,
            spirv::Module::sample_compute("fsmain"),
        )
        .unwrap();

        // Vertex layout: pos(vec2)@0, z@8, color(vec4)@12, stride 28 — the ≥28 stride the rasterizer reads z from.
        let layout = VertexLayout {
            stride: 28,
            step_mode: 0,
            attrs: vec![
                VertexAttr {
                    location: 0,
                    format: 0,
                    offset: 0,
                },
                VertexAttr {
                    location: 1,
                    format: 0,
                    offset: 12,
                },
            ],
        };
        // Depth-tested pipeline: compare LESS, depth-write on, over a Depth32Float attachment.
        let depth_state = DepthState::depth_only(TextureFormat::Depth32Float, true, compare::LESS);
        let pipe = create::create_graphics_pipeline(
            &mut d,
            &mut sink,
            (vs, "vsmain"),
            Some((fs, "fsmain")),
            vec![layout],
            vec![TextureFormat::Rgba8Unorm],
            Some(depth_state),
            None,
            1,
            Topology::TriangleList,
            0,
            0,
            0xf,
        )
        .unwrap();

        // A full-screen quad (two triangles) at z = 0.6, red.
        let z = 0.6;
        let mut verts = Vec::new();
        for v in [
            (-1.0, -1.0),
            (1.0, -1.0),
            (1.0, 1.0), // tri 1
            (-1.0, -1.0),
            (1.0, 1.0),
            (-1.0, 1.0), // tri 2
        ] {
            verts.extend(depth_vertex(v.0, v.1, z, quad_color));
        }
        let vsize = verts.len() as u64;
        let vbuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, vsize)
            .unwrap();
        let mem = d.allocate_memory(vsize).unwrap();
        create::bind_buffer_memory(&mut d, vbuf, mem, 0).unwrap();
        d.map_memory(mem).unwrap();
        create::write_mapped(&mut d, mem, 0, &verts).unwrap();

        // Record: standalone depth clear → render pass (color CLEAR, depth LOAD to preserve it) → draw 6 → end.
        let cb = d.allocate_command_buffer();
        d.begin_command_buffer(cb, false).unwrap();
        record::cmd_clear_depth_stencil_image(&mut d, cb, depth, clear_depth, 0, false).unwrap();
        record::cmd_begin_render_pass(
            &mut d,
            cb,
            target,
            clear,
            true,
            Some(record::RenderingDepthAttachment {
                image: depth,
                clear_depth: 0.0,
                load_clear: false,
            }),
        )
        .unwrap();
        record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
        record::cmd_bind_vertex_buffer(&mut d, cb, 0, vbuf, 0).unwrap();
        record::cmd_draw(&mut d, cb, 6, 1, 0, 0).unwrap();
        d.end_render_pass(cb).unwrap();
        d.end_command_buffer(cb).unwrap();
        submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

        let mut pixels = vec![0u8; (W * H * 4) as usize];
        sink.executor()
            .read_texture(sink.resources(), TextureId(target_ir), &mut pixels)
            .expect("read back the color target");
        let o = ((H / 2 * W + W / 2) * 4) as usize;
        [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
    };

    // Cleared to 0.5: the z=0.6 quad fails `0.6 < 0.5` ⇒ occluded ⇒ blue clear survives.
    assert_eq!(
        render_with_clear_depth(0.5),
        [0, 0, 255, 255],
        "depth cleared to 0.5 occludes the z=0.6 quad"
    );
    // Cleared to 1.0: the z=0.6 quad passes `0.6 < 1.0` ⇒ red quad drawn. Proves the clear VALUE is honored.
    assert_eq!(
        render_with_clear_depth(1.0),
        [255, 0, 0, 255],
        "depth cleared to 1.0 lets the z=0.6 quad through"
    );
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
        Limits::from_capabilities(Capabilities::permissive_fixture("hl-cpu-graphics")),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    let mut sink = InProcessCommandSink::with_session(session, exec);

    // --- guest side: the real hl-vulkan driver services -------------------------------------------
    let inst = Instance::new(HL_API_VERSION);
    let mut d = inst.create_device();

    // vkCreateImage: an RGBA8 color render target (COLOR_ATTACHMENT ⇒ RENDER_TARGET usage).
    let target = create::create_image(
        &mut d,
        &mut sink,
        W,
        H,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    let target_ir = d.images.get(&target).unwrap().ir_id;

    // vkCreateShaderModule ×2 — trivial but valid SPIR-V, forwarded verbatim (the seam keystone). The
    // CPU oracle creates the modules but never executes them.
    let vs = create::create_shader_module_words(
        &mut d,
        &mut sink,
        spirv::Module::sample_compute("vsmain"),
    )
    .unwrap();
    let fs = create::create_shader_module_words(
        &mut d,
        &mut sink,
        spirv::Module::sample_compute("fsmain"),
    )
    .unwrap();

    // vkCreateGraphicsPipelines — one color target matching the attachment, a slot-0 vertex layout the
    // rasterizer fetches positions/colors from (pos @ offset 0, color @ offset 8, stride 24).
    let layout = VertexLayout {
        stride: 24,
        step_mode: 0,
        attrs: vec![
            VertexAttr {
                location: 0,
                format: 0,
                offset: 0,
            },
            VertexAttr {
                location: 1,
                format: 0,
                offset: 8,
            },
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
        1,
        Topology::TriangleList,
        0,
        0,
        0xf,
    )
    .unwrap();

    // vkCreateBuffer(VERTEX) + vkAllocateMemory + vkBindBufferMemory + vkMapMemory + write the 3 verts.
    // The persistently-mapped bytes flush as a Cmd::WriteBuffer at vkQueueSubmit.
    let mut verts = Vec::new();
    verts.extend(vertex(0.0, 0.8, tri)); // apex, top-center
    verts.extend(vertex(-0.8, -0.8, tri)); // bottom-left
    verts.extend(vertex(0.8, -0.8, tri)); // bottom-right
    let vsize = verts.len() as u64;
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, vsize).unwrap();
    let mem = d.allocate_memory(vsize).unwrap();
    create::bind_buffer_memory(&mut d, vbuf, mem, 0).unwrap();
    d.map_memory(mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &verts).unwrap();

    // record: begin render pass (clear) → bind pipeline → bind vertex buffer → draw 3 → end pass.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, target, clear, true, None).unwrap();
    record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, vbuf, 0).unwrap();
    record::cmd_draw(&mut d, cb, 3, 1, 0, 0).unwrap();
    d.end_render_pass(cb).unwrap();
    d.end_command_buffer(cb).unwrap();

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
    assert_eq!(
        texel(W / 2, H / 2),
        [255, 0, 0, 255],
        "triangle covers the center (red)"
    );
    // The top-left corner is outside the triangle → still the blue clear color.
    assert_eq!(
        texel(0, 0),
        [0, 0, 255, 255],
        "corner keeps the clear color (blue)"
    );
    // The bottom-left corner (below-left of the triangle's left edge) is also uncovered.
    assert_eq!(
        texel(0, H - 1),
        [0, 0, 255, 255],
        "bottom-left corner keeps the clear color"
    );
}
