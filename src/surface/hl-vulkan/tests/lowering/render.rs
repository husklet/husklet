use super::*;

#[test]
fn draw_rebases_cts_vertex_stream_and_preserves_the_vulkan_binding_offset() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 256).unwrap();
    let mut bases = [0; 31];
    bases[1] = 64;
    let pipeline = 0x777;
    d.pipelines.insert(
        pipeline,
        hl_vulkan::model::pipeline::PipelineRec {
            ir_id: 99,
            kind: hl_vulkan::model::pipeline::PipelineKind::Graphics,
            vertex_buffer_bases: bases,
        },
    );
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_pipeline(&mut d, cb, pipeline).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 1, vbuf, 12).unwrap();
    record::cmd_draw(&mut d, cb, 4, 3, 2, 1).unwrap();

    let enc = &d.command_buffers.get(&cb).unwrap().enc;
    assert_eq!(
        enc,
        &[
            Enc::SetVertexBuffer {
                slot: 1,
                buffer: 1,
                offset: 12
            },
            Enc::SetPipeline(99),
            Enc::SetVertexBuffer {
                slot: 1,
                buffer: 1,
                offset: 76
            },
            Enc::Draw {
                vertex_count: 4,
                instance_count: 3,
                first_vertex: 2,
                first_instance: 1,
            },
        ]
    );
}

#[test]
fn graphics_render_pass_draw_lowers_to_expected_encoder_stream() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // a render-target image (ir 1), vertex + fragment shaders (ir 2, 3), graphics pipeline (ir 4).
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
    let pipe = create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![pos_color_layout()],
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

    // a vertex buffer (ir 5).
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 24 * 3).unwrap();

    // record the render pass: begin (clear) → bind pipeline → bind vertex buffer → draw → end.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, target, [0.0, 0.0, 1.0, 1.0], true, None).unwrap();
    record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, vbuf, 0).unwrap();
    record::cmd_draw(&mut d, cb, 3, 1, 0, 0).unwrap();
    d.end_render_pass(cb).unwrap();
    d.end_command_buffer(cb).unwrap();

    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // the submitted encoder is the exact render-pass draw stream.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => {
            assert_eq!(
                cbuf.encoder,
                vec![
                    Enc::BeginRenderPass {
                        color: vec![hl_gpu::protocol::model::descriptor::ColorAttachment {
                            texture: 1,
                            load: hl_gpu::protocol::model::enums::LoadOp::Clear,
                            clear: [0.0, 0.0, 1.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    // SetVertexBuffer is recorded eagerly by vkCmdBindVertexBuffers; the pipeline is
                    // replayed lazily by vkCmdDraw — hence vbuf precedes the pipeline in the stream.
                    Enc::SetVertexBuffer {
                        slot: 0,
                        buffer: 5,
                        offset: 0
                    },
                    Enc::SetPipeline(4),
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0
                    },
                    Enc::EndRenderPass,
                ]
            );
        }
        other => panic!("expected Submit, got {other:?}"),
    }
}

#[test]
fn buffer_bindings_before_render_pass_are_replayed_inside_it() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let target = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 64).unwrap();
    let ibuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDEX_BUFFER, 64).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 3, vbuf, 12).unwrap();
    record::cmd_bind_index_buffer(&mut d, cb, ibuf, 4, 1).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, target, [0.0; 4], false, None).unwrap();

    let enc = &d.command_buffers.get(&cb).unwrap().enc;
    let begin = enc
        .iter()
        .position(|op| matches!(op, Enc::BeginRenderPass { .. }))
        .unwrap();
    assert_eq!(
        &enc[begin + 1..],
        &[
            Enc::SetVertexBuffer {
                slot: 3,
                buffer: 2,
                offset: 12,
            },
            Enc::SetIndexBuffer {
                buffer: 3,
                offset: 4,
                format: IndexFormat::U32,
            },
        ]
    );
}

#[test]
fn classic_render_pass_preserves_multiple_color_attachments() {
    use hl_vulkan::service::record::RenderingColorAttachment;

    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let first = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    let second = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UINT,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_begin_render_pass_multi(
        &mut d,
        cb,
        &[
            RenderingColorAttachment {
                image: first,
                clear: [0.1, 0.2, 0.3, 1.0],
                load_clear: true,
                store: true,
            },
            RenderingColorAttachment {
                image: second,
                clear: [1.0, 2.0, 3.0, 4.0],
                load_clear: false,
                store: true,
            },
        ],
        None,
    )
    .unwrap();

    let Enc::BeginRenderPass { color, depth: None } = &d.command_buffers.get(&cb).unwrap().enc[0]
    else {
        panic!("expected a color render pass");
    };
    assert_eq!(color.len(), 2);
    assert_eq!(color[0].texture, img_ir(&d, first));
    assert_eq!(color[0].load, LoadOp::Clear);
    assert_eq!(color[1].texture, img_ir(&d, second));
    assert_eq!(color[1].load, LoadOp::Load);
}

#[test]
fn begin_rendering_lowers_to_begin_render_pass_with_clear_attachment() {
    // vkCmdBeginRendering (VK_KHR_dynamic_rendering) lowers to the SAME Enc::BeginRenderPass a classic
    // render pass does — the color target + CLEAR come from the inline VkRenderingInfo, with no
    // VkRenderPass/VkFramebuffer object. vkCmdEndRendering reuses cmd_end_render_pass (Enc::EndRenderPass).
    use hl_vulkan::service::record::RenderingColorAttachment;

    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let target = create::create_image(
        &mut d,
        &mut sink,
        128,
        128,
        vk_format::B8G8R8A8_UNORM,
        vk_image_usage::COLOR_ATTACHMENT,
        1,
    )
    .unwrap();
    let ir = img_ir(&d, target);

    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_begin_rendering(
            d,
            cb,
            &[RenderingColorAttachment {
                image: target,
                clear: [0.1, 0.2, 0.3, 1.0],
                load_clear: true,
                store: true,
            }],
            None,
        )
        .unwrap();
        d.end_render_pass(cb).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![hl_gpu::protocol::model::descriptor::ColorAttachment {
                    texture: ir,
                    load: hl_gpu::protocol::model::enums::LoadOp::Clear,
                    clear: [0.1, 0.2, 0.3, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::EndRenderPass,
        ]
    );
    // The active clear target is set, so a vkCmdClearAttachments inside the dynamic pass resolves.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_begin_rendering(
        &mut d,
        cb,
        &[RenderingColorAttachment {
            image: target,
            clear: [0.0; 4],
            load_clear: false,
            store: true,
        }],
        None,
    )
    .unwrap();
    assert!(
        record::cmd_clear_attachment_rect(&mut d, cb, 0, 0, 4, 4, [1.0, 0.0, 0.0, 1.0]).is_ok()
    );
    // An unknown attachment image is a typed error, not a silent skip.
    let cb2 = d.allocate_command_buffer();
    d.begin_command_buffer(cb2, false).unwrap();
    assert!(record::cmd_begin_rendering(
        &mut d,
        cb2,
        &[RenderingColorAttachment {
            image: 0xdead,
            clear: [0.0; 4],
            load_clear: true,
            store: true
        }],
        None,
    )
    .is_err());
}

#[test]
fn indexed_draw_lowers_set_index_buffer_and_draw_indexed() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let ibuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDEX_BUFFER, 6).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // VK_INDEX_TYPE_UINT16 = 0.
    record::cmd_bind_index_buffer(&mut d, cb, ibuf, 0, 0).unwrap();
    record::cmd_draw_indexed(&mut d, cb, 3, 1, 0, 0, 0).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => {
            assert_eq!(
                cbuf.encoder,
                vec![
                    Enc::SetIndexBuffer {
                        buffer: 1,
                        offset: 0,
                        format: IndexFormat::U16
                    },
                    Enc::DrawIndexed {
                        index_count: 3,
                        instance_count: 1,
                        first_index: 0,
                        base_vertex: 0,
                        first_instance: 0,
                    },
                ]
            );
        }
        other => panic!("expected Submit, got {other:?}"),
    }
}

#[test]
fn uint32_index_buffer_preserves_the_full_index_format() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let ibuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDEX_BUFFER, 12).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();

    // VK_INDEX_TYPE_UINT32 = 1. This is the behavior behind
    // VkPhysicalDeviceFeatures::fullDrawIndexUint32.
    record::cmd_bind_index_buffer(&mut d, cb, ibuf, 0, 1).unwrap();
    record::cmd_draw_indexed(&mut d, cb, 3, 1, 0, 0, 0).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    let [Cmd::Submit(commands)] = sink.batches.last().unwrap().as_slice() else {
        panic!("expected one submitted command buffer");
    };
    assert_eq!(
        commands.encoder.first(),
        Some(&Enc::SetIndexBuffer {
            buffer: 1,
            offset: 0,
            format: IndexFormat::U32,
        })
    );
}
