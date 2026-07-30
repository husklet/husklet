use super::*;

/// A RED-only `colorWriteMask` (0x1) must write ONLY the R channel: a red full-screen quad drawn over a
/// GREEN clear leaves G (and B) exactly as the clear left them — the center pixel is YELLOW. Before the fix
/// the write mask was hardcoded `0xF` (write all channels), so the quad overwrote every channel — the pixel
/// was RED. The color flip proves the guest's `VkPipelineColorBlendAttachmentState::colorWriteMask` now
/// threads to the pipeline and the executor honors it.
#[test]
fn color_write_mask_red_only_leaves_green_and_blue_untouched_end_to_end() {
    const W: u32 = 8;
    const H: u32 = 8;
    let clear = [0.0f32, 1.0, 0.0, 1.0]; // opaque green
    let quad = [1.0f32, 0.0, 0.0, 1.0]; // opaque red

    let render = |write_mask: u32| -> [u8; 4] {
        let exec = CpuExecutor::new();
        let session = Session::new(
            Limits::from_capabilities(Capabilities::permissive_fixture("hl-cpu-writemask")),
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        let mut sink = InProcessCommandSink::with_session(session, exec);
        let inst = Instance::new(HL_API_VERSION);
        let mut d = inst.create_device();
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
            write_mask,
        )
        .unwrap();
        // A full-screen quad (two triangles) covering the whole target.
        let mut verts = Vec::new();
        for v in [
            (-1.0f32, -1.0f32),
            (1.0, -1.0),
            (1.0, 1.0),
            (-1.0, -1.0),
            (1.0, 1.0),
            (-1.0, 1.0),
        ] {
            verts.extend(vertex(v.0, v.1, quad));
        }
        let vsize = verts.len() as u64;
        let vbuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, vsize)
            .unwrap();
        let mem = d.allocate_memory(vsize).unwrap();
        create::bind_buffer_memory(&mut d, vbuf, mem, 0).unwrap();
        d.map_memory(mem).unwrap();
        create::write_mapped(&mut d, mem, 0, &verts).unwrap();
        let cb = d.allocate_command_buffer();
        d.begin_command_buffer(cb, false).unwrap();
        record::cmd_begin_render_pass(&mut d, cb, target, clear, true, None).unwrap();
        record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
        record::cmd_bind_vertex_buffer(&mut d, cb, 0, vbuf, 0).unwrap();
        record::cmd_draw(&mut d, cb, 6, 1, 0, 0).unwrap();
        d.end_render_pass(cb).unwrap();
        d.end_command_buffer(cb).unwrap();
        submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
        let mut pixels = vec![0u8; (W * H * 4) as usize];
        sink.executor()
            .read_texture(sink.resources(), TextureId(target_ir), &mut pixels)
            .unwrap();
        let o = ((H / 2 * W + W / 2) * 4) as usize;
        [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
    };

    assert_eq!(
        render(0xF),
        [255, 0, 0, 255],
        "write-all mask overwrites every channel -> red"
    );
    assert_eq!(
        render(0x1),
        [255, 255, 0, 255],
        "RED-only mask keeps the green clear's G/B -> yellow"
    );
}

/// `cullMode` + `frontFace` select which triangle facing is rasterized, end to end. A single triangle has
/// ONE facing, so under a fixed winding exactly one of {cull FRONT, cull BACK} removes it (the pixel falls
/// back to the blue clear) while the other keeps it (red); and flipping `frontFace` inverts the winding
/// classification, flipping which cull face removes it. Before the fix `cull`/`front_face` were hardcoded 0
/// (ignored), so every variant drew the triangle red — the discriminating flips below would all collapse.
#[test]
fn cull_mode_and_front_face_select_the_triangle_facing_end_to_end() {
    const W: u32 = 8;
    const H: u32 = 8;
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    let clear = [0.0f32, 0.0, 1.0, 1.0]; // opaque blue
    let tri = [1.0f32, 0.0, 0.0, 1.0]; // opaque red

    let render = |cull: u32, front_face: u32| -> [u8; 4] {
        let exec = CpuExecutor::new();
        let session = Session::new(
            Limits::from_capabilities(Capabilities::permissive_fixture("hl-cpu-cull")),
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        let mut sink = InProcessCommandSink::with_session(session, exec);
        let inst = Instance::new(HL_API_VERSION);
        let mut d = inst.create_device();
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
            cull,
            front_face,
            0xf,
        )
        .unwrap();
        // One triangle covering the center pixel (apex top-center, base along the bottom).
        let mut verts = Vec::new();
        verts.extend(vertex(0.0, 0.8, tri));
        verts.extend(vertex(-0.8, -0.8, tri));
        verts.extend(vertex(0.8, -0.8, tri));
        let vsize = verts.len() as u64;
        let vbuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, vsize)
            .unwrap();
        let mem = d.allocate_memory(vsize).unwrap();
        create::bind_buffer_memory(&mut d, vbuf, mem, 0).unwrap();
        d.map_memory(mem).unwrap();
        create::write_mapped(&mut d, mem, 0, &verts).unwrap();
        let cb = d.allocate_command_buffer();
        d.begin_command_buffer(cb, false).unwrap();
        record::cmd_begin_render_pass(&mut d, cb, target, clear, true, None).unwrap();
        record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
        record::cmd_bind_vertex_buffer(&mut d, cb, 0, vbuf, 0).unwrap();
        record::cmd_draw(&mut d, cb, 3, 1, 0, 0).unwrap();
        d.end_render_pass(cb).unwrap();
        d.end_command_buffer(cb).unwrap();
        submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
        let mut pixels = vec![0u8; (W * H * 4) as usize];
        sink.executor()
            .read_texture(sink.resources(), TextureId(target_ir), &mut pixels)
            .unwrap();
        let o = ((H / 2 * W + W / 2) * 4) as usize;
        [pixels[o], pixels[o + 1], pixels[o + 2], pixels[o + 3]]
    };

    // cull NONE always draws the triangle.
    assert_eq!(render(0, 0), RED, "cull=none draws the triangle");
    // Fixed winding: exactly one of {cull front, cull back} removes the triangle.
    let cf = render(1, 0);
    let cb = render(2, 0);
    assert!(
        (cf == BLUE) ^ (cb == BLUE),
        "exactly one cull face removes the triangle (front={cf:?} back={cb:?})"
    );
    // Flipping frontFace inverts the facing, flipping which cull face removes the triangle.
    assert_ne!(
        render(2, 0),
        render(2, 1),
        "frontFace flips which winding cull=BACK removes"
    );
}
