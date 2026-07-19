//! Lowering tests: drive each Vulkan service against a `hl_gpu::RecordingSink` and assert the exact
//! protocol `Cmd`/`Enc` sequence the operation lowers to (plus the SPIR-V passthrough adapter).
//!
//! This is the acceptance gate for the Vulkan→IR lowering layer: no loader, no socket, no GPU — just
//! the recorded command stream, which is wire-identical to what the shipping ICD emits.

use hl_vulkan::adapter::spirv;
use hl_vulkan::model::descriptor::{
    vk_descriptor_type, DescriptorTemplateEntry, LayoutBinding,
    VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
};
use hl_vulkan::model::memory::{vk_buffer_usage, vk_format, vk_image_usage};
use hl_vulkan::result;
use hl_vulkan::service::{create, present, record, submit, sync};
use hl_vulkan::{Device, Instance};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindResource, Extent3d, Origin3d, TextureSubresource, VertexAttr, VertexLayout,
};
use hl_gpu::protocol::model::enums::{buffer_usage, Filter, IndexFormat, TextureFormat, Topology};
use hl_gpu::{Cmd, FenceId, GpuError, RecordingSink, ShaderPayloadKind};

/// A slot-0 vertex layout carrying interleaved position (offset 0) + color (offset 8), stride 24 — the
/// layout the host rasterizer fetches `pos`/`color` from.
fn pos_color_layout() -> VertexLayout {
    VertexLayout {
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
    }
}

fn dev() -> Device {
    let inst = Instance::new(result::HL_API_VERSION);
    inst.create_device()
}

// ---------------------------------------------------------------------------------------------------
// instance / physical device
// ---------------------------------------------------------------------------------------------------

#[test]
fn physical_device_reports_metal_class_props() {
    let inst = Instance::new(result::HL_API_VERSION);
    let pd = &inst.physical_device;
    assert_eq!(pd.name, "hl Metal (Vulkan)");
    assert_eq!(pd.api_version, result::HL_API_VERSION); // Vulkan 1.4.0
    assert_eq!(pd.vendor_id, 0x106b); // Apple
    assert_eq!(pd.device_type, 1); // INTEGRATED_GPU (unified memory)
    assert_eq!(pd.limits.max_image_dimension_2d, 16384);
    assert_eq!(pd.limits.max_bound_descriptor_sets, 8);
    assert_eq!(pd.queue_family.queue_flags, 0b111); // graphics | compute | transfer
}

// ---------------------------------------------------------------------------------------------------
// buffers / memory
// ---------------------------------------------------------------------------------------------------

#[test]
fn create_buffer_emits_create_buffer_with_translated_usage() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let _buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 4096).unwrap();

    assert_eq!(sink.batches.len(), 1);
    match &sink.batches[0][0] {
        Cmd::CreateBuffer(id, desc) => {
            assert_eq!(*id, 1);
            assert_eq!(desc.size, 4096);
            // VkBufferUsage STORAGE_BUFFER → hl STORAGE, and every hl device buffer is MAP-able.
            assert_ne!(desc.usage & buffer_usage::STORAGE, 0);
            assert_ne!(desc.usage & buffer_usage::MAP, 0);
        }
        other => panic!("expected CreateBuffer, got {other:?}"),
    }
}

#[test]
fn second_buffer_gets_distinct_ir_id() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 64).unwrap();
    create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDEX_BUFFER, 64).unwrap();
    assert!(matches!(sink.batches[0][0], Cmd::CreateBuffer(1, _)));
    assert!(matches!(sink.batches[1][0], Cmd::CreateBuffer(2, _)));
}

#[test]
fn destroy_buffer_emits_destroy_and_ignores_null() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let b = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
    create::destroy_buffer(&mut d, &mut sink, b).unwrap();
    assert!(matches!(
        sink.batches.last().unwrap()[0],
        Cmd::DestroyBuffer(1)
    ));
    // destroying VK_NULL_HANDLE / an already-destroyed buffer is a no-op, not an error.
    let before = sink.batches.len();
    create::destroy_buffer(&mut d, &mut sink, b).unwrap();
    assert_eq!(sink.batches.len(), before);
}

#[test]
fn bind_memory_rejects_unknown_handles() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let b = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
    let err = create::bind_buffer_memory(&mut d, b, 0xdead, 0).unwrap_err();
    assert!(matches!(err, GpuError::Invalid(_)));
}

// ---------------------------------------------------------------------------------------------------
// images / samplers
// ---------------------------------------------------------------------------------------------------

#[test]
fn create_image_emits_create_texture() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let usage = vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::SAMPLED;
    create::create_image(
        &mut d,
        &mut sink,
        256,
        128,
        vk_format::B8G8R8A8_UNORM,
        usage,
        1,
    )
    .unwrap();
    match &sink.batches[0][0] {
        Cmd::CreateTexture(id, desc) => {
            assert_eq!(*id, 1);
            assert_eq!((desc.width, desc.height), (256, 128));
            assert_eq!(desc.format, TextureFormat::Bgra8Unorm);
            assert_eq!(
                desc.sample_count, 1,
                "a samples=_1_BIT image is single-sample"
            );
        }
        other => panic!("expected CreateTexture, got {other:?}"),
    }
}

#[test]
fn create_image_threads_multisample_count() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // VkSampleCountFlagBits value IS the count: _4_BIT == 4.
    let usage = vk_image_usage::COLOR_ATTACHMENT | vk_image_usage::TRANSFER_SRC;
    create::create_image(
        &mut d,
        &mut sink,
        64,
        64,
        vk_format::R8G8B8A8_UNORM,
        usage,
        4,
    )
    .unwrap();
    match &sink.batches[0][0] {
        Cmd::CreateTexture(_, desc) => {
            assert_eq!(
                desc.sample_count, 4,
                "VkImageCreateInfo::samples=_4_BIT threads to sample_count == 4"
            );
        }
        other => panic!("expected CreateTexture, got {other:?}"),
    }
}

#[test]
fn create_sampler_emits_create_sampler() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // min=LINEAR(1) mag=LINEAR(1) mip=LINEAR(1) address=REPEAT(0)
    create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0]);
    assert!(matches!(sink.batches[0][0], Cmd::CreateSampler(1, _)));
}

// ---------------------------------------------------------------------------------------------------
// shader modules — the SPIR-V passthrough keystone
// ---------------------------------------------------------------------------------------------------

#[test]
fn shader_module_forwards_spirv_verbatim() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let words = spirv::Module::sample_compute("main");
    let _sh = create::create_shader_module_words(&mut d, &mut sink, words.clone()).unwrap();

    match &sink.batches[0][0] {
        Cmd::CreateShader { id, kind, spirv } => {
            assert_eq!(*id, 1);
            assert_eq!(*kind, ShaderPayloadKind::SpirV);
            // THE KEYSTONE: the SPIR-V words survive the seam byte-for-byte (no translation).
            assert_eq!(spirv, &words);
        }
        other => panic!("expected CreateShader, got {other:?}"),
    }
}

#[test]
fn spirv_adapter_parses_entry_points_and_rejects_garbage() {
    let words = spirv::Module::sample_compute("computeMain");
    let module = spirv::Module::from_words(words.clone()).unwrap();
    assert_eq!(module.entry_points(), vec!["computeMain".to_string()]);

    // a byte image that is not a SPIR-V module is a typed error, not a panic.
    assert!(matches!(
        spirv::Module::from_bytes(b"not spirv at all!!!!"),
        Err(GpuError::Invalid(_))
    ));
    // a 3-byte (non word-multiple) image is rejected on size.
    assert!(spirv::Module::from_bytes(&[1, 2, 3]).is_err());
}

#[test]
fn compute_pipeline_rejects_missing_entry() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let sh = create::create_shader_module_words(
        &mut d,
        &mut sink,
        spirv::Module::sample_compute("main"),
    )
    .unwrap();
    // a good entry succeeds; a missing one is a typed error (no id-zero default pipeline).
    assert!(create::create_compute_pipeline(&mut d, &mut sink, sh, "main").is_ok());
    assert!(create::create_compute_pipeline(&mut d, &mut sink, sh, "nope").is_err());
}

// ---------------------------------------------------------------------------------------------------
// pipelines
// ---------------------------------------------------------------------------------------------------

#[test]
fn graphics_pipeline_emits_create_render_pipeline() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
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
    create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![pos_color_layout()],
        vec![TextureFormat::Bgra8Unorm],
        None,
        None,
        1,
        Topology::TriangleList,
        0,
        0,
        0xf,
    )
    .unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateRenderPipeline(_, desc)] => {
            assert!(desc.fragment.is_some());
            assert_eq!(desc.color_targets[0].format, TextureFormat::Bgra8Unorm);
            // the VkPipelineVertexInputState layout is forwarded (slot 0, stride 24).
            assert_eq!(desc.vertex_buffers.len(), 1);
            assert_eq!(desc.vertex_buffers[0].stride, 24);
            assert_eq!(desc.vertex_buffers[0].attrs.len(), 2);
            assert_eq!(
                desc.sample_count, 1,
                "a rasterizationSamples=_1_BIT pipeline is single-sample"
            );
        }
        other => panic!("expected CreateRenderPipeline, got {other:?}"),
    }
}

#[test]
fn graphics_pipeline_threads_multisample_count() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
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
    // A 4x-MSAA pipeline (VkPipelineMultisampleStateCreateInfo::rasterizationSamples == _4_BIT).
    create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![pos_color_layout()],
        vec![TextureFormat::Bgra8Unorm],
        None,
        None,
        4,
        Topology::TriangleList,
        0,
        0,
        0xf,
    )
    .unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateRenderPipeline(_, desc)] => {
            assert_eq!(
                desc.sample_count, 4,
                "rasterizationSamples=_4_BIT threads to sample_count == 4"
            );
        }
        other => panic!("expected CreateRenderPipeline, got {other:?}"),
    }
}

#[test]
fn graphics_pipeline_threads_cull_front_face_and_color_write_mask() {
    // The rasterization cull state (VkPipelineRasterizationStateCreateInfo::cullMode/frontFace) and the
    // first color attachment's colorWriteMask were previously HARDCODED in `create.rs` (`cull: 0`,
    // `front_face: 0`, `write_mask: 0xF`), silently dropping the guest's real values. Prove each threads
    // into the emitted RenderPipelineDesc.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
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
    // cull BACK (2), front-face CW (1), RED-only write mask (0x1) — every field non-default.
    create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![pos_color_layout()],
        vec![TextureFormat::Bgra8Unorm, TextureFormat::Rgba8Unorm],
        None,
        None,
        1,
        Topology::TriangleList,
        2,
        1,
        0x1,
    )
    .unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateRenderPipeline(_, desc)] => {
            assert_eq!(
                desc.cull, 2,
                "VkCullMode BACK threads to cull == 2 (was hardcoded 0)"
            );
            assert_eq!(
                desc.front_face, 1,
                "VkFrontFace CW threads to front_face == 1 (was hardcoded 0)"
            );
            for t in &desc.color_targets {
                assert_eq!(
                    t.write_mask, 0x1,
                    "RED-only colorWriteMask threads to every target (was hardcoded 0xF)"
                );
            }
        }
        other => panic!("expected CreateRenderPipeline, got {other:?}"),
    }
}

#[test]
fn graphics_pipeline_preserves_stencil_state_into_the_ir() {
    // A stencil-enabled VkPipelineDepthStencilStateCreateInfo is now translated to a neutral DepthState
    // carrying per-face stencil ops + masks (the shim's `parse_depth_stencil_state`, replacing the old
    // `DepthState::depth_only` that FORCED the inert `DISABLED` faces). Prove `create_graphics_pipeline`
    // carries that stencil state through to the IR untouched.
    use hl_gpu::protocol::model::descriptor::{DepthState, StencilFaceState};
    use hl_gpu::protocol::model::enums::{compare, stencil_op};
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
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
    let face = StencilFaceState {
        compare: compare::EQUAL,
        fail_op: stencil_op::KEEP,
        depth_fail_op: stencil_op::KEEP,
        pass_op: stencil_op::REPLACE,
    };
    let depth = DepthState {
        format: TextureFormat::Depth24PlusStencil8,
        depth_write: false,
        depth_compare: compare::ALWAYS,
        stencil_front: face,
        stencil_back: face,
        stencil_read_mask: 0xff,
        stencil_write_mask: 0xff,
    };
    create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![pos_color_layout()],
        vec![TextureFormat::Rgba8Unorm],
        Some(depth),
        None,
        1,
        Topology::TriangleList,
        0,
        0,
        0xf,
    )
    .unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateRenderPipeline(_, desc)] => {
            let ds = desc
                .depth
                .as_ref()
                .expect("a stencil pipeline carries a depth-stencil state");
            assert_eq!(ds.stencil_front.compare, compare::EQUAL);
            assert_eq!(ds.stencil_front.pass_op, stencil_op::REPLACE);
            assert_eq!(ds.stencil_back.compare, compare::EQUAL);
            assert_eq!(ds.stencil_read_mask, 0xff);
            assert_eq!(ds.stencil_write_mask, 0xff);
        }
        other => panic!("expected CreateRenderPipeline, got {other:?}"),
    }
}

#[test]
fn dynamic_rendering_pipeline_takes_color_formats_from_pnext_no_render_pass() {
    // A VK_KHR_dynamic_rendering graphics pipeline has NO VkRenderPass — its color-target formats come
    // from VkPipelineRenderingCreateInfo::pColorAttachmentFormats (passed here as the format list). It
    // still lowers to a real Cmd::CreateRenderPipeline with those color targets.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
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
    // Two color attachment formats from the pNext, and no render pass object at all.
    create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![pos_color_layout()],
        vec![TextureFormat::Bgra8Unorm, TextureFormat::Rgba8Unorm],
        None,
        None,
        1,
        Topology::TriangleList,
        0,
        0,
        0xf,
    )
    .unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateRenderPipeline(_, desc)] => {
            assert_eq!(desc.color_targets.len(), 2);
            assert_eq!(desc.color_targets[0].format, TextureFormat::Bgra8Unorm);
            assert_eq!(desc.color_targets[1].format, TextureFormat::Rgba8Unorm);
        }
        other => panic!("expected CreateRenderPipeline, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------------
// the graphics render-pass lowering: pipeline → begin pass → bind vbuf → draw → end pass → submit
// ---------------------------------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------------------------------
// the core compute lowering: create → descriptor → dispatch → submit
// ---------------------------------------------------------------------------------------------------

#[test]
fn full_compute_dispatch_lowers_to_expected_stream() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // buffers (ir 1, 2), shader (ir 3), compute pipeline (ir 4).
    let in_buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();
    let out_buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();
    let sh = create::create_shader_module_words(
        &mut d,
        &mut sink,
        spirv::Module::sample_compute("main"),
    )
    .unwrap();
    let pipe = create::create_compute_pipeline(&mut d, &mut sink, sh, "main").unwrap();

    // descriptor set: two storage-buffer bindings (0 = in, 1 = out).
    let layout = d.create_descriptor_set_layout(vec![
        LayoutBinding {
            binding: 0,
            descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: 0,
        },
        LayoutBinding {
            binding: 1,
            descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: 0,
        },
    ]);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set, 0, in_buf, 0, 1024).unwrap();
    create::update_descriptor_buffer(&mut d, set, 1, out_buf, 0, 1024).unwrap();

    // record: bind pipeline + descriptor set (→ CreateBindGroup ir 5) + dispatch.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut sink, cb, 0, &[set], &[]).unwrap();
    record::cmd_dispatch(&mut d, cb, 64, 1, 1).unwrap();
    d.end_command_buffer(cb).unwrap();

    // submit → one Cmd::Submit carrying the recorded compute encoder.
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // batches: CreateBuffer, CreateBuffer, CreateShader, CreateComputePipeline, CreateBindGroup, Submit.
    assert_eq!(sink.batches.len(), 6, "batches = {:#?}", sink.batches);

    // the bind group resolves both storage buffers to their ir ids, ascending by binding.
    match &sink.batches[4][0] {
        Cmd::CreateBindGroup(id, desc) => {
            assert_eq!(*id, 5);
            assert_eq!(desc.set, 0);
            assert_eq!(desc.entries.len(), 2);
            assert_eq!(desc.entries[0].binding, 0);
            assert!(matches!(
                desc.entries[0].resource,
                BindResource::Buffer {
                    id: 1,
                    offset: 0,
                    ..
                }
            ));
            assert!(matches!(
                desc.entries[1].resource,
                BindResource::Buffer {
                    id: 2,
                    offset: 0,
                    ..
                }
            ));
        }
        other => panic!("expected CreateBindGroup, got {other:?}"),
    }

    // the dispatch command buffer: pipeline ir 4, bind group ir 5.
    match &sink.batches[5][0] {
        Cmd::Submit(cbuf) => {
            assert_eq!(
                cbuf.encoder,
                vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(4),
                    Enc::SetBindGroup { index: 0, group: 5 },
                    Enc::Dispatch { x: 64, y: 1, z: 1 },
                    Enc::EndComputePass,
                ]
            );
            assert_eq!(cbuf.signal, None);
        }
        other => panic!("expected Submit, got {other:?}"),
    }
}

/// The ir sampler id behind a `VkSampler` handle.
fn samp_ir(d: &Device, h: u64) -> u32 {
    d.samplers.get(&h).unwrap().ir_id
}

#[test]
fn combined_image_sampler_descriptor_lowers_to_texture_and_sampler_binds() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // A sampled image (ir 1) + a sampler (ir 2).
    let image = create::create_image(
        &mut d,
        &mut sink,
        64,
        64,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let sampler = create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0]);

    // A set with a single COMBINED_IMAGE_SAMPLER binding at binding 0.
    let layout = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: 0,
        descriptor_type: vk_descriptor_type::COMBINED_IMAGE_SAMPLER,
        descriptor_count: 1,
        stage_flags: 0,
    }]);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    // `vkUpdateDescriptorSets`: the shim resolves the write's imageView → this VkImage; drive the
    // driver directly with (image, sampler) — the same tables the shim populates.
    create::update_descriptor_image(&mut d, set, 0, Some(image), Some(sampler)).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut sink, cb, 0, &[set], &[]).unwrap();
    d.end_command_buffer(cb).unwrap();

    // The bind emits one CreateBindGroup (the last batch): a Texture(image) at the descriptor's binding 0
    // and a Sampler(sampler) at binding 0 + 16, the split the wgpu executor's `spirv_split` performs on
    // glslang's combined `sampler2D` (naga rejects the combined image-sampler model, so the image and its
    // sampler must occupy DISTINCT bind-group bindings).
    match &sink.batches.last().unwrap()[0] {
        Cmd::CreateBindGroup(_, desc) => {
            assert_eq!(desc.set, 0);
            assert_eq!(desc.entries.len(), 2);
            assert_eq!(desc.entries[0].binding, 0);
            assert_eq!(
                desc.entries[0].resource,
                BindResource::Texture {
                    id: img_ir(&d, image)
                }
            );
            assert_eq!(desc.entries[1].binding, 16);
            assert_eq!(
                desc.entries[1].resource,
                BindResource::Sampler {
                    id: samp_ir(&d, sampler)
                }
            );
        }
        other => panic!("expected CreateBindGroup, got {other:?}"),
    }
}

#[test]
fn separate_sampled_image_and_sampler_descriptors_lower_at_their_own_bindings() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    let image = create::create_image(
        &mut d,
        &mut sink,
        32,
        32,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let sampler = create::create_sampler(&mut d, &mut sink, 0, 0, 0, [0, 0, 0]);

    // A SAMPLED_IMAGE at binding 0 and a separate SAMPLER at binding 1.
    let layout = d.create_descriptor_set_layout(vec![
        LayoutBinding {
            binding: 0,
            descriptor_type: vk_descriptor_type::SAMPLED_IMAGE,
            descriptor_count: 1,
            stage_flags: 0,
        },
        LayoutBinding {
            binding: 1,
            descriptor_type: vk_descriptor_type::SAMPLER,
            descriptor_count: 1,
            stage_flags: 0,
        },
    ]);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_image(&mut d, set, 0, Some(image), None).unwrap();
    create::update_descriptor_image(&mut d, set, 1, None, Some(sampler)).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut sink, cb, 0, &[set], &[]).unwrap();
    d.end_command_buffer(cb).unwrap();

    // Two entries: a Texture at binding 0 and a Sampler at binding 1 (binding-ascending resolution).
    match &sink.batches.last().unwrap()[0] {
        Cmd::CreateBindGroup(_, desc) => {
            assert_eq!(desc.entries.len(), 2);
            assert_eq!(desc.entries[0].binding, 0);
            assert_eq!(
                desc.entries[0].resource,
                BindResource::Texture {
                    id: img_ir(&d, image)
                }
            );
            assert_eq!(desc.entries[1].binding, 1);
            assert_eq!(
                desc.entries[1].resource,
                BindResource::Sampler {
                    id: samp_ir(&d, sampler)
                }
            );
        }
        other => panic!("expected CreateBindGroup, got {other:?}"),
    }
}

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
    submit::wait_for_fence(&mut d, &mut sink, fence).unwrap();
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

// ---------------------------------------------------------------------------------------------------
// present
// ---------------------------------------------------------------------------------------------------

#[test]
fn present_path_lowers_surface_and_present() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let surface =
        present::create_surface(&mut d, &mut sink, 1920, 1080, vk_format::B8G8R8A8_UNORM, 7)
            .unwrap();
    match &sink.batches[0][0] {
        Cmd::CreateSurface(id, desc) => {
            assert_eq!(*id, 1);
            assert_eq!((desc.width, desc.height), (1920, 1080));
            assert_eq!(desc.format, TextureFormat::Bgra8Unorm);
            assert_eq!(desc.hlp_surface, 7);
        }
        other => panic!("expected CreateSurface, got {other:?}"),
    }

    let sc = present::create_swapchain(&mut d, &mut sink, surface, 2).unwrap();
    // create_swapchain emits one CreateTexture per presentable image (real render-target textures).
    let img0_ir = d.swapchains.get(&sc).unwrap().images[0].ir_texture_id;
    assert!(sink.batches.iter().flatten().any(|c| matches!(
        c,
        Cmd::CreateTexture(id, desc)
            if *id == img0_ir
                && (desc.width, desc.height) == (1920, 1080)
                && desc.usage & hl_gpu::protocol::model::enums::texture_usage::RENDER_TARGET != 0
                && desc.usage & hl_gpu::protocol::model::enums::texture_usage::COPY_SRC != 0
    )));

    let idx = d.acquire_next_image(sc).unwrap();
    present::queue_present(&mut d, &mut sink, sc, idx).unwrap();

    // the present names the surface's ir id + the presented image's REAL backing texture id.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Present {
            surface: s,
            texture: t,
        }] => {
            assert_eq!(*s, 1); // the CreateSurface ir id
            assert_eq!(*t, img0_ir); // the presented swapchain image's real render-target texture
        }
        other => panic!("expected Present, got {other:?}"),
    }
}

/// `vkAcquireNextImageKHR` genuinely FIFO round-robins across a swapchain's images instead of pinning
/// image 0: over an acquire→present loop of MORE than `image_count` iterations the returned indices cycle
/// `0,1,..,N-1,0,..`, each present lowers a `Cmd::Present` naming the acquired image's own texture (so the
/// presented image is exactly the one acquired that iteration), and queue_present returns the image to the
/// pool. This is the driver-level proof of the fix for vkcube's one-frame `demo_draw` abort.
#[test]
fn acquire_round_robins_across_swapchain_images() {
    const N: u32 = 3;
    const ITERS: usize = 7; // > N, so the cycle wraps twice + one
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let surface =
        present::create_surface(&mut d, &mut sink, 64, 64, vk_format::B8G8R8A8_UNORM, 0).unwrap();
    let sc = present::create_swapchain(&mut d, &mut sink, surface, N).unwrap();

    // Each image's own backing texture id, so we can prove the present named the acquired image's texture.
    let img_texs: Vec<u32> = d
        .swapchains
        .get(&sc)
        .unwrap()
        .images
        .iter()
        .map(|i| i.ir_texture_id)
        .collect();

    let mut acquired = Vec::new();
    for _ in 0..ITERS {
        let idx = d.acquire_next_image(sc).unwrap();
        acquired.push(idx);
        present::queue_present(&mut d, &mut sink, sc, idx).unwrap();
        // The just-emitted Present names the acquired image's OWN texture (present == the acquired image).
        match sink.batches.last().unwrap().as_slice() {
            [Cmd::Present { texture: t, .. }] => {
                assert_eq!(
                    *t, img_texs[idx as usize],
                    "present targets the acquired image's texture"
                )
            }
            other => panic!("expected Present, got {other:?}"),
        }
    }

    // The indices cycle 0,1,2,0,1,2,0 — not stuck at 0 (the bug), and every image is used.
    assert_eq!(
        acquired,
        vec![0, 1, 2, 0, 1, 2, 0],
        "acquire cycles through all {N} images in FIFO order"
    );
    // Back in the pool after the loop: a fresh acquire continues the cycle rather than failing.
    assert_eq!(
        d.acquire_next_image(sc).unwrap(),
        1,
        "the cursor persists across the loop"
    );
}

// ---------------------------------------------------------------------------------------------------
// result mapping
// ---------------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------------
// transfer path: buffer/image copies, blits, clears, fills/updates, barriers
// ---------------------------------------------------------------------------------------------------

/// The ir buffer/image id behind a handle (the id the emitted encoder op references).
fn buf_ir(d: &Device, h: u64) -> u32 {
    d.buffers.get(&h).unwrap().ir_id
}
fn img_ir(d: &Device, h: u64) -> u32 {
    d.images.get(&h).unwrap().ir_id
}

/// Record `record_fn` into a fresh command buffer and return the single submitted encoder stream.
fn record_and_submit(
    d: &mut Device,
    sink: &mut RecordingSink,
    record_fn: impl FnOnce(&mut Device, u64),
) -> Vec<Enc> {
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record_fn(d, cb);
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(d, sink, &[cb], None).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => cbuf.encoder.clone(),
        other => panic!("expected a single Submit, got {other:?}"),
    }
}

#[test]
fn copy_buffer_lowers_to_copy_buffer_to_buffer() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 256).unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 256).unwrap();
    let (s, t) = (buf_ir(&d, src), buf_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer(d, cb, src, dst, 16, 32, 64).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToBuffer {
            src: s,
            src_offset: 16,
            dst: t,
            dst_offset: 32,
            size: 64
        }]
    );
}

#[test]
fn copy_buffer_to_image_lowers_to_copy_buffer_to_texture() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // A 4x4 RGBA8 target: tight-packed bytes_per_row = 4*4 = 16; span = 16*3 + 16 = 64.
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 64).unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let (s, t) = (buf_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer_to_image(d, cb, src, dst, 0, 0, 0, 4, 4).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToTexture {
            src: s,
            src_offset: 0,
            bytes_per_row: 16,
            dst: t,
            mip: 0,
            width: 4,
            height: 4
        }]
    );
}

#[test]
fn copy_buffer_to_image_r8_coverage_atlas_uses_one_byte_per_texel() {
    // GPUI's glyph-coverage atlas is `R8Unorm` (1 byte/texel), uploaded region-by-region via
    // `queue.write_texture` → `vkCmdCopyBufferToImage` from a TIGHTLY-PACKED staging buffer. Regression
    // guard: the lowering must use the image's real bytes-per-texel (1 for R8), not a hardcoded 4. With the
    // old `* 4` assumption the implied span was 4x oversized, FAILED the `end <= buf_size` bounds check, and
    // the copy was rejected — silently, because `vkCmdCopyBufferToImage` returns void — so every glyph
    // upload was dropped and text never rasterized.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // An 8x8 R8 region: tight bytes_per_row = 8*1 = 8; total staging = 8*8 = 64 bytes. (Under the old bug
    // the implied span was 8*4*7 + 8*4 = 256 > 64, so this exact copy errored.)
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 64).unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        16,
        16,
        vk_format::R8_UNORM,
        vk_image_usage::TRANSFER_DST | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let (s, t) = (buf_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer_to_image(d, cb, src, dst, 0, 0, 0, 8, 8).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToTexture {
            src: s,
            src_offset: 0,
            bytes_per_row: 8,
            dst: t,
            mip: 0,
            width: 8,
            height: 8
        }],
        "an R8 coverage-atlas upload must lower with bytes_per_row = width*1, not width*4"
    );
}

#[test]
fn copy_image_to_buffer_r8_uses_one_byte_per_texel() {
    // The reverse (glyph atlas readback) path shares the same bytes-per-texel helper; an R8 image → tight
    // buffer copy must likewise use 1 byte/texel.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_image(
        &mut d,
        &mut sink,
        16,
        16,
        vk_format::R8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let (s, t) = (img_ir(&d, src), buf_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image_to_buffer(d, cb, src, dst, 0, 0, 0, 8, 8).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToBuffer {
            src: s,
            mip: 0,
            width: 8,
            height: 8,
            dst: t,
            dst_offset: 0,
            bytes_per_row: 8
        }]
    );
}

#[test]
fn copy_image_to_buffer_lowers_to_copy_texture_to_buffer() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_image(
        &mut d,
        &mut sink,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let (s, t) = (img_ir(&d, src), buf_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image_to_buffer(d, cb, src, dst, 0, 0, 0, 4, 4).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToBuffer {
            src: s,
            mip: 0,
            width: 4,
            height: 4,
            dst: t,
            dst_offset: 0,
            bytes_per_row: 16
        }]
    );
}

#[test]
fn copy_image_lowers_to_copy_texture_to_texture() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let (s, t) = (img_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image(d, cb, src, dst, (1, 2), (3, 4), (4, 4)).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToTexture {
            src: s,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d { x: 1, y: 2, z: 0 },
            dst: t,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 3, y: 4, z: 0 },
            extent: Extent3d {
                width: 4,
                height: 4,
                depth: 1
            },
        }]
    );
    // Copy-compatible-format rejection: differing formats are a typed error, not a silent mis-copy.
    let other = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::B8G8R8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_copy_image(&mut d, cb, src, other, (0, 0), (0, 0), (4, 4)).is_err());
}

#[test]
fn resolve_image_lowers_to_copy_texture_to_texture() {
    // hl images are single-sample, so a multisample resolve is exactly a same-extent image COPY: it must
    // MOVE the source content into the resolve target (the old body recorded nothing → a blank target).
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let (s, t) = (img_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_resolve_image(d, cb, src, dst, (0, 0), (0, 0), (8, 8)).unwrap();
    });
    // A resolve lowers to the byte-identical op a same-region vkCmdCopyImage would emit (resolve == copy).
    let copy = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image(d, cb, src, dst, (0, 0), (0, 0), (8, 8)).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToTexture {
            src: s,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d { x: 0, y: 0, z: 0 },
            dst: t,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 0, y: 0, z: 0 },
            extent: Extent3d {
                width: 8,
                height: 8,
                depth: 1
            },
        }]
    );
    assert_eq!(
        enc, copy,
        "a single-sample resolve must lower to its copy twin"
    );
    // Truthful failure paths are inherited from cmd_copy_image: a missing-usage / format-mismatch target.
    let bad = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::B8G8R8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_resolve_image(&mut d, cb, src, bad, (0, 0), (0, 0), (8, 8)).is_err());
}

#[test]
fn blit_image_lowers_to_blit_texture_with_filter() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_SRC,
        1,
    )
    .unwrap();
    let dst = create::create_image(
        &mut d,
        &mut sink,
        16,
        16,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let (s, t) = (img_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        // Upscale the 8x8 source into a 16x16 region with a linear filter.
        record::cmd_blit_image(d, cb, src, dst, (0, 0), (8, 8), (0, 0), (16, 16), true).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::BlitTexture {
            src: s,
            src_sub: TextureSubresource::base(),
            src_origin: Origin3d { x: 0, y: 0, z: 0 },
            src_extent: Extent3d {
                width: 8,
                height: 8,
                depth: 1
            },
            dst: t,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 0, y: 0, z: 0 },
            dst_extent: Extent3d {
                width: 16,
                height: 16,
                depth: 1
            },
            filter: Filter::Linear,
        }]
    );
}

#[test]
fn clear_color_image_lowers_to_full_extent_clear_rect() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img = create::create_image(
        &mut d,
        &mut sink,
        32,
        16,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let ir = img_ir(&d, img);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_color_image(d, cb, img, [0.25, 0.5, 0.75, 1.0]).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::ClearRect {
            texture: ir,
            x: 0,
            y: 0,
            w: 32,
            h: 16,
            color: [0.25, 0.5, 0.75, 1.0]
        }]
    );
}

#[test]
fn clear_depth_stencil_image_lowers_to_depth_clear_render_pass() {
    use hl_gpu::protocol::model::descriptor::DepthAttachment;
    use hl_gpu::protocol::model::enums::LoadOp;

    // A depth-only image (D32) created as a DEPTH_STENCIL attachment + transfer-clear target.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let usage = vk_image_usage::DEPTH_STENCIL_ATTACHMENT | vk_image_usage::TRANSFER_DST;
    let img =
        create::create_image(&mut d, &mut sink, 16, 16, vk_format::D32_SFLOAT, usage, 1).unwrap();
    let ir = img_ir(&d, img);
    // Depth-only aspect (has_stencil = false): a zero-draw BeginRenderPass(depth CLEAR) / EndRenderPass.
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_depth_stencil_image(d, cb, img, 0.5, 7, false).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![],
                depth: Some(DepthAttachment {
                    texture: ir,
                    load: LoadOp::Clear,
                    clear_depth: 0.5,
                    clear_stencil: 0
                }),
            },
            Enc::EndRenderPass,
        ],
        "depth-only clear lowers to a depth-clear pass, stencil forced to 0"
    );

    // A combined depth+stencil image with the stencil aspect selected carries the stencil clear value.
    let ds = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::D24_UNORM_S8_UINT,
        usage,
        1,
    )
    .unwrap();
    let ds_ir = img_ir(&d, ds);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_depth_stencil_image(d, cb, ds, 1.0, 0x2a, true).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![],
                depth: Some(DepthAttachment {
                    texture: ds_ir,
                    load: LoadOp::Clear,
                    clear_depth: 1.0,
                    clear_stencil: 0x2a,
                }),
            },
            Enc::EndRenderPass,
        ],
        "combined depth+stencil clear carries the stencil value"
    );

    // Truthful errors: a color image is not a depth format; missing TRANSFER_DST usage is rejected.
    let color =
        create::create_image(&mut d, &mut sink, 8, 8, vk_format::R8G8B8A8_UNORM, usage, 1).unwrap();
    let no_dst = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::D32_SFLOAT,
        vk_image_usage::DEPTH_STENCIL_ATTACHMENT,
        1,
    )
    .unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_clear_depth_stencil_image(&mut d, cb, color, 0.0, 0, false).is_err());
    assert!(record::cmd_clear_depth_stencil_image(&mut d, cb, no_dst, 0.0, 0, false).is_err());
    assert!(record::cmd_clear_depth_stencil_image(&mut d, cb, 0xdead, 0.0, 0, false).is_err());
}

#[test]
fn clear_attachments_lowers_to_clear_rect_on_active_target() {
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
    let ir = img_ir(&d, target);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_begin_render_pass(d, cb, target, [0.0, 0.0, 0.0, 1.0], false, None).unwrap();
        record::cmd_clear_attachment_rect(d, cb, 8, 8, 16, 16, [1.0, 0.0, 0.0, 1.0]).unwrap();
        d.end_render_pass(cb).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::BeginRenderPass {
                color: vec![hl_gpu::protocol::model::descriptor::ColorAttachment {
                    texture: ir,
                    load: hl_gpu::protocol::model::enums::LoadOp::Load,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::ClearRect {
                texture: ir,
                x: 8,
                y: 8,
                w: 16,
                h: 16,
                color: [1.0, 0.0, 0.0, 1.0]
            },
            Enc::EndRenderPass,
        ]
    );
    // A clear-attachments outside a render pass is a typed error (no active target to clear).
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_clear_attachment_rect(&mut d, cb, 0, 0, 4, 4, [0.0; 4]).is_err());
}

#[test]
fn fill_and_update_buffer_flush_as_write_buffer_at_submit() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let ir = buf_ir(&d, buf);
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Fill [0,8) with 0x01010101 (two words), then update [16,20) with explicit bytes.
    record::cmd_fill_buffer(&mut d, cb, buf, 0, 8, 0x0101_0101).unwrap();
    record::cmd_update_buffer(&mut d, cb, buf, 16, &[9, 8, 7, 6]).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // The two buffer writes flush (in record order) as WriteBuffers before the (empty) Submit.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer {
            id: i1,
            offset: 0,
            data: d1,
        }, Cmd::WriteBuffer {
            id: i2,
            offset: 16,
            data: d2,
        }, Cmd::Submit(_)] => {
            assert_eq!((*i1, *i2), (ir, ir));
            assert_eq!(d1, &vec![1u8; 8]);
            assert_eq!(d2, &vec![9u8, 8, 7, 6]);
        }
        other => panic!("expected [WriteBuffer, WriteBuffer, Submit], got {other:?}"),
    }
    // fill rejects a non-COPY_DST buffer and a misaligned offset.
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 64).unwrap();
    let cb2 = d.allocate_command_buffer();
    d.begin_command_buffer(cb2, false).unwrap();
    assert!(record::cmd_fill_buffer(&mut d, cb2, vbuf, 0, 8, 0).is_err());
    assert!(record::cmd_fill_buffer(&mut d, cb2, buf, 2, 8, 0).is_err());
}

// ---------------------------------------------------------------------------------------------------
// per-frame dynamic state, push constants, and indirect draws
// ---------------------------------------------------------------------------------------------------

#[test]
fn set_viewport_and_scissor_lower_to_encoder_ops() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_set_viewport(d, cb, 0.0, 0.0, 640.0, 480.0, 0.0, 1.0).unwrap();
        // A negative scissor offset clamps to 0 (the IR scissor is unsigned).
        record::cmd_set_scissor(d, cb, 0, 0, 640, 480).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::SetViewport {
                x: 0.0,
                y: 0.0,
                w: 640.0,
                h: 480.0,
                min_depth: 0.0,
                max_depth: 1.0
            },
            Enc::SetScissor {
                x: 0,
                y: 0,
                w: 640,
                h: 480
            },
        ]
    );
}

#[test]
fn push_constants_reach_the_command_buffer_for_the_draw() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Write 8 bytes at offset 0, then overwrite 4 bytes at offset 4 (grows/patches the block in place).
    record::cmd_push_constants(&mut d, cb, 0, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    record::cmd_push_constants(&mut d, cb, 4, &[9, 9, 9, 9]).unwrap();
    // The recorded block is honest command state a draw reads (the IR has no push-constant channel yet).
    assert_eq!(
        d.command_buffers.get(&cb).unwrap().push_constants,
        vec![1, 2, 3, 4, 9, 9, 9, 9]
    );
    // Misaligned / zero-size pushes are typed errors, never a silent partial write.
    assert!(record::cmd_push_constants(&mut d, cb, 2, &[0, 0, 0, 0]).is_err());
    assert!(record::cmd_push_constants(&mut d, cb, 0, &[0, 0, 0]).is_err());
}

#[test]
fn dynamic_state_is_recorded_but_emits_no_encoder_op() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_set_line_width(&mut d, cb, 2.5).unwrap();
    record::cmd_set_depth_bias(&mut d, cb, 1.0, 0.0, 2.0).unwrap();
    record::cmd_set_blend_constants(&mut d, cb, [0.1, 0.2, 0.3, 0.4]).unwrap();
    // FRONT_AND_BACK = 0x3 sets both faces; FRONT = 0x1 sets only the front.
    record::cmd_set_stencil_reference(&mut d, cb, 0x3, 7).unwrap();
    record::cmd_set_stencil_compare_mask(&mut d, cb, 0x1, 0xff).unwrap();
    d.end_command_buffer(cb).unwrap();

    // The state is recorded (observable, honest) …
    let rec = d.command_buffers.get(&cb).unwrap();
    assert_eq!(rec.dynamic.line_width, 2.5);
    assert_eq!(rec.dynamic.depth_bias, (1.0, 0.0, 2.0));
    assert_eq!(rec.dynamic.blend_constants, [0.1, 0.2, 0.3, 0.4]);
    assert_eq!(rec.dynamic.stencil_reference, (7, 7));
    assert_eq!(rec.dynamic.stencil_compare_mask, (0xff, 0));
    // … but the software rasterizer models none of it, so no encoder op is emitted.
    assert!(
        rec.enc.is_empty(),
        "dynamic state emits no encoder op, got {:?}",
        rec.enc
    );
}

#[test]
fn indirect_draws_read_args_and_lower_to_direct_draws() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // A valid indirect buffer (INDIRECT usage) large enough for two 16-byte VkDrawIndirectCommands,
    // backed by memory the app has filled on the CPU (the mapped-buffer indirect-args pattern).
    let indirect =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 64).unwrap();
    let mem = d.allocate_memory(64).unwrap();
    create::bind_buffer_memory(&mut d, indirect, mem, 0).unwrap();
    // cmd0 = {vertexCount:6, instanceCount:2, firstVertex:3, firstInstance:1}
    // cmd1 = {vertexCount:3, instanceCount:1, firstVertex:0, firstInstance:0}
    let mut args = Vec::new();
    for w in [6u32, 2, 3, 1, 3, 1, 0, 0] {
        args.extend_from_slice(&w.to_le_bytes());
    }
    create::write_mapped(&mut d, mem, 0, &args).unwrap();

    // The indirect draw reads both argument structs and lowers each to the SAME direct Enc::Draw.
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indirect(d, cb, indirect, 0, 2, 16).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::Draw {
                vertex_count: 6,
                instance_count: 2,
                first_vertex: 3,
                first_instance: 1
            },
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0
            },
        ]
    );
    // The equivalent DIRECT draws produce the byte-identical encoder stream (indirect == direct).
    let direct = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw(d, cb, 6, 2, 3, 1).unwrap();
        record::cmd_draw(d, cb, 3, 1, 0, 0).unwrap();
    });
    assert_eq!(
        enc, direct,
        "an indirect draw must lower to its direct twin"
    );

    // vkCmdDrawIndexedIndirect reads the 20-byte struct and lowers to the matching DrawIndexed.
    let idx =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 64).unwrap();
    let mem2 = d.allocate_memory(64).unwrap();
    create::bind_buffer_memory(&mut d, idx, mem2, 0).unwrap();
    // {indexCount:9, instanceCount:3, firstIndex:2, vertexOffset:0, firstInstance:5}
    let mut ib = Vec::new();
    for w in [9u32, 3, 2, 0, 5] {
        ib.extend_from_slice(&w.to_le_bytes());
    }
    create::write_mapped(&mut d, mem2, 0, &ib).unwrap();
    let enc_idx = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indexed_indirect(d, cb, idx, 0, 1, 20).unwrap();
    });
    assert_eq!(
        enc_idx,
        vec![Enc::DrawIndexed {
            index_count: 9,
            instance_count: 3,
            first_index: 2,
            base_vertex: 0,
            first_instance: 5
        }]
    );

    // vkCmdDispatchIndirect reads the 12-byte VkDispatchIndirectCommand{x,y,z} out of the same host-visible
    // backing (the first three words of `args`: 6, 2, 3) and lowers to the SAME compute pass the equivalent
    // vkCmdDispatch(6,2,3) would emit — no pipeline / bind group is bound here, so just the pass wrapper.
    let enc_disp = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_dispatch_indirect(d, cb, indirect, 0).unwrap();
    });
    assert_eq!(
        enc_disp,
        vec![
            Enc::BeginComputePass,
            Enc::Dispatch { x: 6, y: 2, z: 3 },
            Enc::EndComputePass
        ],
        "dispatch-indirect lowers the buffer-sourced workgroup counts to a direct Dispatch"
    );
    // The equivalent DIRECT dispatch produces the byte-identical encoder stream (indirect == direct).
    let direct_disp = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_dispatch(d, cb, 6, 2, 3).unwrap();
    });
    assert_eq!(
        enc_disp, direct_disp,
        "an indirect dispatch must lower to its direct twin"
    );

    // Truthful failure: an unknown buffer, a non-INDIRECT buffer, and an out-of-range span all error.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(record::cmd_draw_indirect(&mut d, cb, 0xdead, 0, 1, 16).is_err());
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 64).unwrap();
    assert!(record::cmd_draw_indirect(&mut d, cb, vbuf, 0, 1, 16).is_err());
    // 5 draws * 16 bytes = 80 > 64: out of bounds.
    assert!(record::cmd_draw_indirect(&mut d, cb, indirect, 0, 5, 16).is_err());
}

#[test]
fn indirect_count_draws_read_count_from_buffer_and_clamp_to_max() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // Argument buffer: three 16-byte VkDrawIndirectCommands, CPU-filled (the mapped indirect-args pattern).
    let indirect =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 64).unwrap();
    let amem = d.allocate_memory(64).unwrap();
    create::bind_buffer_memory(&mut d, indirect, amem, 0).unwrap();
    let mut args = Vec::new();
    for w in [6u32, 2, 3, 1, 3, 1, 0, 0, 9, 4, 2, 5] {
        args.extend_from_slice(&w.to_le_bytes());
    }
    create::write_mapped(&mut d, amem, 0, &args).unwrap();
    // A separate host-visible count buffer holding the GPU/CPU-produced draw count `2`.
    let count =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 16).unwrap();
    let cmem = d.allocate_memory(16).unwrap();
    create::bind_buffer_memory(&mut d, count, cmem, 0).unwrap();
    create::write_mapped(&mut d, cmem, 0, &2u32.to_le_bytes()).unwrap();

    // maxDrawCount = 3, count buffer says 2 → draws exactly the first two argument structs.
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indirect_count(d, cb, indirect, 0, count, 0, 3, 16).unwrap();
    });
    assert_eq!(
        enc,
        vec![
            Enc::Draw { vertex_count: 6, instance_count: 2, first_vertex: 3, first_instance: 1 },
            Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
        ],
        "an indirect-count draw reads the count from the buffer and lowers each arg to a direct Draw"
    );

    // maxDrawCount = 1 clamps the buffer's count of 2 down to 1 (spec: actual = min(count, maxDrawCount)).
    let clamped = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indirect_count(d, cb, indirect, 0, count, 0, 1, 16).unwrap();
    });
    assert_eq!(
        clamped,
        vec![Enc::Draw {
            vertex_count: 6,
            instance_count: 2,
            first_vertex: 3,
            first_instance: 1
        }],
        "maxDrawCount must clamp the buffer-sourced count"
    );

    // vkCmdDrawIndexedIndirectCount reads a 20-byte struct per draw; maxDrawCount 1 clamps to one DrawIndexed.
    let idx =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 64).unwrap();
    let imem = d.allocate_memory(64).unwrap();
    create::bind_buffer_memory(&mut d, idx, imem, 0).unwrap();
    let mut ib = Vec::new();
    for w in [9u32, 3, 2, 0, 5] {
        ib.extend_from_slice(&w.to_le_bytes());
    }
    create::write_mapped(&mut d, imem, 0, &ib).unwrap();
    let enc_idx = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indexed_indirect_count(d, cb, idx, 0, count, 0, 1, 20).unwrap();
    });
    assert_eq!(
        enc_idx,
        vec![Enc::DrawIndexed {
            index_count: 9,
            instance_count: 3,
            first_index: 2,
            base_vertex: 0,
            first_instance: 5
        }]
    );

    // Truthful failure: a count buffer without INDIRECT usage, and an unknown count buffer, both error.
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    let vbuf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    assert!(record::cmd_draw_indirect_count(&mut d, cb, indirect, 0, vbuf, 0, 3, 16).is_err());
    assert!(record::cmd_draw_indirect_count(&mut d, cb, indirect, 0, 0xdead, 0, 3, 16).is_err());
}

#[test]
fn copy_buffer_v1_and_v2_share_the_same_lowering() {
    // The `vkCmdCopyBuffer2` shim entry point re-parses `VkCopyBufferInfo2` and delegates to this exact
    // `record::cmd_copy_buffer` lowering — so the v2 path lowers identically to v1 (asserted here).
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 256).unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 256).unwrap();
    let (s, t) = (buf_ir(&d, src), buf_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer(d, cb, src, dst, 8, 16, 32).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToBuffer {
            src: s,
            src_offset: 8,
            dst: t,
            dst_offset: 16,
            size: 32
        }]
    );
}

#[test]
fn pipeline_barrier_records_layout_transition_and_emits_no_ir() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    // VK_IMAGE_LAYOUT_UNDEFINED (0) -> VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL (7).
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_pipeline_barrier(d, cb, &[(img, 0, 7)]).unwrap();
    });
    // The layout-implicit IR carries no encoder op for a barrier.
    assert!(
        enc.is_empty(),
        "a pipeline barrier emits no encoder op, got {enc:?}"
    );
    // The transition is modeled in device bookkeeping.
    assert_eq!(d.image_layouts.get(&img), Some(&7));
}

// ---------------------------------------------------------------------------------------------------
// events + timeline semaphores + query pools
// ---------------------------------------------------------------------------------------------------

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
    assert_eq!(
        sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false)
            .unwrap(),
        false
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
    assert_eq!(
        sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false)
            .unwrap(),
        false
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

// ---------------------------------------------------------------------------------------------------
// descriptor update templates: a template update == the equivalent direct writes (same bind group)
// ---------------------------------------------------------------------------------------------------

/// The 24-byte `VkDescriptorBufferInfo`/`TemplateBufferInfo` blob a buffer-class template entry reads.
fn buffer_info_bytes(buffer: u64, offset: u64, range: u64) -> [u8; 24] {
    let mut b = [0u8; 24];
    b[0..8].copy_from_slice(&buffer.to_le_bytes());
    b[8..16].copy_from_slice(&offset.to_le_bytes());
    b[16..24].copy_from_slice(&range.to_le_bytes());
    b
}

/// Bind `set` at set index 0 and return the resulting `CreateBindGroup`'s entries.
fn bind_and_capture_entries(
    d: &mut Device,
    sink: &mut RecordingSink,
    set: u64,
) -> Vec<hl_gpu::protocol::model::descriptor::BindEntry> {
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_descriptor_sets(d, sink, cb, 0, &[set], &[]).unwrap();
    d.end_command_buffer(cb).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateBindGroup(_, desc)] => desc.entries.clone(),
        other => panic!("expected CreateBindGroup, got {other:?}"),
    }
}

#[test]
fn descriptor_template_update_matches_direct_writes() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let in_buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();
    let out_buf =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();

    let layout = d.create_descriptor_set_layout(vec![
        LayoutBinding {
            binding: 0,
            descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: 0,
        },
        LayoutBinding {
            binding: 1,
            descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: 0,
        },
    ]);
    let pool = d.create_descriptor_pool(8);

    // Set A: two direct buffer writes (the reference path).
    let set_direct = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set_direct, 0, in_buf, 0, 1024).unwrap();
    create::update_descriptor_buffer(&mut d, set_direct, 1, out_buf, 256, 512).unwrap();

    // Set B: the SAME two writes applied via a descriptor update template + a packed data blob.
    let set_tmpl = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    let template = create::create_descriptor_update_template(
        &mut d,
        VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
        vec![
            DescriptorTemplateEntry {
                dst_binding: 0,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
                offset: 0,
                stride: 24,
            },
            DescriptorTemplateEntry {
                dst_binding: 1,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
                offset: 24,
                stride: 24,
            },
        ],
    )
    .unwrap();
    let mut blob = Vec::new();
    blob.extend_from_slice(&buffer_info_bytes(in_buf, 0, 1024));
    blob.extend_from_slice(&buffer_info_bytes(out_buf, 256, 512));
    create::update_descriptor_set_with_template(&mut d, set_tmpl, template, &blob).unwrap();

    // Binding either set produces the identical IR bind-group entries.
    let direct = bind_and_capture_entries(&mut d, &mut sink, set_direct);
    let via_template = bind_and_capture_entries(&mut d, &mut sink, set_tmpl);
    assert_eq!(direct, via_template);
    // …and they resolve the two storage buffers to ir ids 1 & 2 with the written offsets/ranges.
    assert_eq!(via_template.len(), 2);
    assert!(matches!(
        via_template[0].resource,
        BindResource::Buffer {
            id: 1,
            offset: 0,
            size: 1024
        }
    ));
    assert!(matches!(
        via_template[1].resource,
        BindResource::Buffer {
            id: 2,
            offset: 256,
            size: 512
        }
    ));
}

#[test]
fn descriptor_template_rejects_non_descriptor_set_type_and_bad_handles() {
    let mut d = dev();
    // A push-descriptor template type (1) is a truthful feature-not-present, never a fake success.
    assert!(matches!(
        create::create_descriptor_update_template(&mut d, 1, vec![]),
        Err(GpuError::Unsupported(_))
    ));
    // Updating with an unknown template / set is a typed error.
    assert!(create::update_descriptor_set_with_template(&mut d, 0xdead, 0xbeef, &[]).is_err());
    // A blob too short for a declared entry is a truthful out-of-bounds, not a junk read.
    let tmpl = create::create_descriptor_update_template(
        &mut d,
        VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
        vec![DescriptorTemplateEntry {
            dst_binding: 0,
            dst_array_element: 0,
            descriptor_count: 1,
            descriptor_type: vk_descriptor_type::UNIFORM_BUFFER,
            offset: 0,
            stride: 24,
        }],
    )
    .unwrap();
    let pool = d.create_descriptor_pool(1);
    let layout = d.create_descriptor_set_layout(vec![]);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    assert!(matches!(
        create::update_descriptor_set_with_template(&mut d, set, tmpl, &[0u8; 8]),
        Err(GpuError::OutOfBounds)
    ));
}

// ---------------------------------------------------------------------------------------------------
// secondary command buffers: vkCmdExecuteCommands replays a secondary's ops into the primary
// ---------------------------------------------------------------------------------------------------

#[test]
fn execute_commands_replays_secondary_ops_into_primary() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 256).unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 256).unwrap();
    let (s, t) = (buf_ir(&d, src), buf_ir(&d, dst));

    // A secondary records a copy (encoder op) + a fill (buffer write), then becomes Executable.
    let secondary = d.allocate_command_buffer();
    d.begin_command_buffer(secondary, false).unwrap();
    record::cmd_copy_buffer(&mut d, secondary, src, dst, 0, 0, 64).unwrap();
    record::cmd_fill_buffer(&mut d, secondary, dst, 128, 8, 0x0202_0202).unwrap();
    d.end_command_buffer(secondary).unwrap();

    // The primary executes the secondary, then is submitted.
    let primary = d.allocate_command_buffer();
    d.begin_command_buffer(primary, false).unwrap();
    record::cmd_execute_commands(&mut d, primary, &[secondary]).unwrap();
    d.end_command_buffer(primary).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[primary], None).unwrap();

    // The primary's submit carries the secondary's copy (encoder) preceded by the spliced fill (write).
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer {
            id,
            offset: 128,
            data,
        }, Cmd::Submit(cbuf)] => {
            assert_eq!(*id, t);
            assert_eq!(data, &vec![2u8; 8]);
            assert_eq!(
                cbuf.encoder,
                vec![Enc::CopyBufferToBuffer {
                    src: s,
                    src_offset: 0,
                    dst: t,
                    dst_offset: 0,
                    size: 64
                }]
            );
        }
        other => panic!("expected [WriteBuffer, Submit], got {other:?}"),
    }

    // A secondary that is not Executable (still recording) is a typed error, splicing nothing.
    let unfinished = d.allocate_command_buffer();
    d.begin_command_buffer(unfinished, false).unwrap();
    let p2 = d.allocate_command_buffer();
    d.begin_command_buffer(p2, false).unwrap();
    assert!(record::cmd_execute_commands(&mut d, p2, &[unfinished]).is_err());
}

// ---------------------------------------------------------------------------------------------------
// WSI physical-device surface queries: modeled caps / formats / present modes
// ---------------------------------------------------------------------------------------------------

#[test]
fn surface_queries_report_modeled_values() {
    // Support: only the lone present family (0) presents.
    assert!(present::QueueFamily(0).supports_present());
    assert!(!present::QueueFamily(1).supports_present());

    // Capabilities: double/triple-buffered, surface-defined extent, identity/opaque.
    let caps = present::surface_capabilities();
    assert_eq!(caps.min_image_count, 2);
    assert_eq!(caps.max_image_count, 3);
    assert_eq!(caps.current_extent, (u32::MAX, u32::MAX));
    assert_eq!(caps.max_image_extent, (16384, 16384));
    assert_eq!(caps.max_image_array_layers, 1);

    // Formats: BGRA8 + RGBA8, UNORM + SRGB, all SRGB-nonlinear.
    let formats = present::surface_formats();
    use hl_vulkan::model::queue::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR;
    assert!(formats
        .iter()
        .all(|f| f.color_space == VK_COLOR_SPACE_SRGB_NONLINEAR_KHR));
    assert!(formats.iter().any(|f| f.format == vk_format::B8G8R8A8_SRGB));
    assert!(formats.iter().any(|f| f.format == vk_format::R8G8B8A8_SRGB));

    // Present modes: FIFO (the always-available v-synced mode).
    assert_eq!(
        present::surface_present_modes(),
        vec![hl_vulkan::model::queue::VK_PRESENT_MODE_FIFO_KHR]
    );
}

// ---------------------------------------------------------------------------------------------------
// vkGetImageSubresourceLayout + pipeline cache
// ---------------------------------------------------------------------------------------------------

#[test]
fn image_subresource_layout_reports_linear_rgba8_layout() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img = create::create_image(
        &mut d,
        &mut sink,
        64,
        32,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let l = d.image_subresource_layout(img).unwrap();
    assert_eq!(l.offset, 0);
    assert_eq!(l.row_pitch, 64 * 4); // width * 4 bytes/texel
    assert_eq!(l.size, 64 * 4 * 32); // rowPitch * height
                                     // An unknown image is a typed error.
    assert!(d.image_subresource_layout(0xdead).is_err());
}

#[test]
fn pipeline_cache_roundtrips_a_valid_header() {
    let mut d = dev();
    let cache = create::PipelineCache::create(&mut d, &[]);
    let data = create::PipelineCache::data(&d, cache).unwrap();
    // A valid VkPipelineCacheHeaderVersionOne: length 32, version 1 (little-endian).
    assert!(data.len() >= 32);
    assert_eq!(u32::from_le_bytes([data[0], data[1], data[2], data[3]]), 32);
    assert_eq!(u32::from_le_bytes([data[4], data[5], data[6], data[7]]), 1);

    // Merge is a truthful no-op that validates handles.
    let other = create::PipelineCache::create(&mut d, &[]);
    assert!(create::PipelineCache::merge(&d, cache, &[other]).is_ok());
    assert!(create::PipelineCache::merge(&d, cache, &[0xdead]).is_err());
    assert!(create::PipelineCache::merge(&d, 0xdead, &[]).is_err());

    // Destroy then query is a typed error.
    create::PipelineCache::destroy(&mut d, cache);
    assert!(create::PipelineCache::data(&d, cache).is_err());
}

#[test]
fn gpu_error_maps_to_vk_result() {
    assert_eq!(
        result::Status::from_error(&GpuError::Unsupported("x")),
        result::VK_ERROR_FEATURE_NOT_PRESENT
    );
    assert_eq!(
        result::Status::from_error(&GpuError::Invalid("x")),
        result::VK_ERROR_INITIALIZATION_FAILED
    );
    assert_eq!(
        result::Status::from_error(&GpuError::ResourceLimit("x")),
        result::VK_ERROR_OUT_OF_DEVICE_MEMORY
    );
    assert_eq!(
        result::Status::from_error(&GpuError::UnknownId {
            kind: "buffer",
            id: 3
        }),
        result::VK_ERROR_UNKNOWN
    );
}

// ===================================================================================================
// EXHAUSTIVE ENTRY-POINT COVERAGE SWEEP (task #222)
//
// These tests close the remaining service-level gaps so EVERY hl-vulkan service function an FFI entry
// point marshals into is exercised for its real effect (recorded model state / IR) — no untested path,
// no faked success. Each block names the `vk*` entry points it covers.
// ===================================================================================================

/// `record::set_dynamic` is the single seam EVERY extended-dynamic-state `vkCmdSet*` records through
/// (`vkCmdSetCullMode`, `vkCmdSetFrontFace`, `vkCmdSetPrimitiveTopology`, `vkCmdSetDepthTestEnable`,
/// `vkCmdSetDepthWriteEnable`, `vkCmdSetDepthCompareOp`, `vkCmdSetDepthBoundsTestEnable`,
/// `vkCmdSetDepthBounds`, `vkCmdSetStencilTestEnable`, `vkCmdSetRasterizerDiscardEnable`,
/// `vkCmdSetDepthBiasEnable`, `vkCmdSetPrimitiveRestartEnable`, `vkCmdSetLogicOpEXT`,
/// `vkCmdSetPatchControlPointsEXT`, `vkCmdSetLineStipple`, `vkCmdSetLineStippleEnableEXT`,
/// `vkCmdSetVertexInputEXT`, `vkCmdSetRasterizationSamplesEXT`, `vkCmdSetSampleMaskEXT`,
/// `vkCmdSetAlphaToCoverageEnableEXT`, `vkCmdSetAlphaToOneEnableEXT`, `vkCmdSetLogicOpEnableEXT`,
/// `vkCmdSetPolygonModeEXT`, `vkCmdSetTessellationDomainOriginEXT`, `vkCmdSetProvokingVertexModeEXT`,
/// `vkCmdSetLineRasterizationModeEXT`, `vkCmdSetDepthClampEnableEXT`, `vkCmdSetDepthClipEnableEXT`,
/// `vkCmdSetDepthClipNegativeOneToOneEXT`, `vkCmdSetConservativeRasterizationModeEXT`,
/// `vkCmdSetExtraPrimitiveOverestimationSizeEXT`, `vkCmdSetSampleLocationsEnableEXT`,
/// `vkCmdSetRasterizationStreamEXT` + every `*EXT` alias). This asserts the seam records each field
/// exactly as its shim body writes it, and emits NO encoder op (the color oracle models none of it).
#[test]
fn extended_dynamic_state_records_every_field_and_emits_no_ir() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();

    // Mirror each shim `vkCmdSet*` body: it records exactly this mutation through `record::set_dynamic`.
    record::set_dynamic(&mut d, cb, |ds| ds.cull_mode = 0x2).unwrap(); // VK_CULL_MODE_BACK_BIT
    record::set_dynamic(&mut d, cb, |ds| ds.front_face = 1).unwrap(); // CLOCKWISE
    record::set_dynamic(&mut d, cb, |ds| ds.primitive_topology = 3).unwrap(); // TRIANGLE_LIST
    record::set_dynamic(&mut d, cb, |ds| ds.primitive_restart_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.rasterizer_discard_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.depth_test_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.depth_write_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.depth_compare_op = 4).unwrap(); // GREATER
    record::set_dynamic(&mut d, cb, |ds| ds.depth_bounds_test_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.depth_bounds = (0.25, 0.75)).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.depth_bias_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.stencil_test_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.logic_op = 6).unwrap(); // AND
    record::set_dynamic(&mut d, cb, |ds| ds.patch_control_points = 3).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.line_stipple = (2, 0xABCD)).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.line_stipple_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.vertex_binding_count = 5).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.rasterization_samples = 4).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.sample_mask = 0x0F0F).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.alpha_to_coverage_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.alpha_to_one_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.logic_op_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.polygon_mode = 1).unwrap(); // LINE
    record::set_dynamic(&mut d, cb, |ds| ds.tessellation_domain_origin = 1).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.provoking_vertex_mode = 1).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.line_rasterization_mode = 2).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.depth_clamp_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.depth_clip_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.depth_clip_negative_one_to_one = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.conservative_rasterization_mode = 1).unwrap();
    record::set_dynamic(&mut d, cb, |ds| {
        ds.extra_primitive_overestimation_size = 0.5
    })
    .unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.sample_locations_enable = true).unwrap();
    record::set_dynamic(&mut d, cb, |ds| ds.rasterization_stream = 1).unwrap();
    d.end_command_buffer(cb).unwrap();

    let rec = d.command_buffers.get(&cb).unwrap();
    let ds = &rec.dynamic;
    assert_eq!(ds.cull_mode, 0x2);
    assert_eq!(ds.front_face, 1);
    assert_eq!(ds.primitive_topology, 3);
    assert!(ds.primitive_restart_enable);
    assert!(ds.rasterizer_discard_enable);
    assert!(ds.depth_test_enable);
    assert!(ds.depth_write_enable);
    assert_eq!(ds.depth_compare_op, 4);
    assert!(ds.depth_bounds_test_enable);
    assert_eq!(ds.depth_bounds, (0.25, 0.75));
    assert!(ds.depth_bias_enable);
    assert!(ds.stencil_test_enable);
    assert_eq!(ds.logic_op, 6);
    assert_eq!(ds.patch_control_points, 3);
    assert_eq!(ds.line_stipple, (2, 0xABCD));
    assert!(ds.line_stipple_enable);
    assert_eq!(ds.vertex_binding_count, 5);
    assert_eq!(ds.rasterization_samples, 4);
    assert_eq!(ds.sample_mask, 0x0F0F);
    assert!(ds.alpha_to_coverage_enable);
    assert!(ds.alpha_to_one_enable);
    assert!(ds.logic_op_enable);
    assert_eq!(ds.polygon_mode, 1);
    assert_eq!(ds.tessellation_domain_origin, 1);
    assert_eq!(ds.provoking_vertex_mode, 1);
    assert_eq!(ds.line_rasterization_mode, 2);
    assert!(ds.depth_clamp_enable);
    assert!(ds.depth_clip_enable);
    assert!(ds.depth_clip_negative_one_to_one);
    assert_eq!(ds.conservative_rasterization_mode, 1);
    assert_eq!(ds.extra_primitive_overestimation_size, 0.5);
    assert!(ds.sample_locations_enable);
    assert_eq!(ds.rasterization_stream, 1);
    // None of this fixed-function state lowers to an encoder op.
    assert!(
        rec.enc.is_empty(),
        "extended dynamic state emits no encoder op, got {:?}",
        rec.enc
    );
}

/// `record::set_dynamic` enforces the Vulkan "command buffer must be recording" rule — an extended
/// `vkCmdSet*` on a not-yet-begun (or already-ended) buffer is a typed error, never a silent success.
#[test]
fn set_dynamic_on_non_recording_buffer_is_an_error() {
    let mut d = dev();
    let cb = d.allocate_command_buffer(); // Initial, never begun
    assert!(record::set_dynamic(&mut d, cb, |ds| ds.cull_mode = 1).is_err());
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap(); // Executable, no longer recording
    assert!(record::set_dynamic(&mut d, cb, |ds| ds.cull_mode = 1).is_err());
}

/// `vkCmdSetStencilOp` (+ `vkCmdSetStencilOpEXT`) via `record::set_stencil_op` — the face mask
/// (FRONT=0x1, BACK=0x2, FRONT_AND_BACK=0x3) selects which face's `(fail,pass,depthFail,compare)` op
/// tuple is recorded.
#[test]
fn set_stencil_op_selects_faces_by_mask() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // FRONT only.
    record::set_stencil_op(&mut d, cb, 0x1, (1, 2, 3, 4)).unwrap();
    // BACK only.
    record::set_stencil_op(&mut d, cb, 0x2, (5, 6, 7, 8)).unwrap();
    d.end_command_buffer(cb).unwrap();
    let ds = &d.command_buffers.get(&cb).unwrap().dynamic;
    assert_eq!(ds.stencil_op_front, (1, 2, 3, 4));
    assert_eq!(ds.stencil_op_back, (5, 6, 7, 8));

    // FRONT_AND_BACK overwrites both.
    let cb2 = d.allocate_command_buffer();
    d.begin_command_buffer(cb2, false).unwrap();
    record::set_stencil_op(&mut d, cb2, 0x3, (9, 9, 9, 9)).unwrap();
    d.end_command_buffer(cb2).unwrap();
    let ds2 = &d.command_buffers.get(&cb2).unwrap().dynamic;
    assert_eq!(ds2.stencil_op_front, (9, 9, 9, 9));
    assert_eq!(ds2.stencil_op_back, (9, 9, 9, 9));
}

/// `vkCmdSetStencilWriteMask` via `record::cmd_set_stencil_write_mask` — the face mask selects which of
/// the `(front, back)` write-mask pair is written (companion to the already-covered compare-mask path).
#[test]
fn set_stencil_write_mask_selects_faces_by_mask() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_set_stencil_write_mask(&mut d, cb, 0x1, 0xAA).unwrap(); // FRONT
    record::cmd_set_stencil_write_mask(&mut d, cb, 0x2, 0xBB).unwrap(); // BACK
    d.end_command_buffer(cb).unwrap();
    assert_eq!(
        d.command_buffers
            .get(&cb)
            .unwrap()
            .dynamic
            .stencil_write_mask,
        (0xAA, 0xBB)
    );

    let cb2 = d.allocate_command_buffer();
    d.begin_command_buffer(cb2, false).unwrap();
    record::cmd_set_stencil_write_mask(&mut d, cb2, 0x3, 0xCC).unwrap(); // FRONT_AND_BACK
    d.end_command_buffer(cb2).unwrap();
    assert_eq!(
        d.command_buffers
            .get(&cb2)
            .unwrap()
            .dynamic
            .stencil_write_mask,
        (0xCC, 0xCC)
    );
}

/// `vkCmdSetColorWriteMaskEXT` / `vkCmdSetColorWriteEnableEXT` via `record::set_dynamic_attachment_array`
/// — the per-attachment arrays are recorded at their (`first`, `first+len`) span, growing on demand, and
/// an out-of-range attachment span (past `maxColorAttachments`) is a truthful usage error, not a
/// multi-GiB allocation. (`vkCmdSetColorBlendEnableEXT` is already covered elsewhere.)
#[test]
fn dynamic_attachment_arrays_record_at_span_and_bound_check() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // Write masks at attachments [1,3): the vector grows to length 3, slot 0 stays default (0).
    record::set_dynamic_attachment_array(&mut d, cb, 1, &[0x7, 0xF], |ds| {
        &mut ds.color_write_masks
    })
    .unwrap();
    // Write enables at [0,2).
    record::set_dynamic_attachment_array(&mut d, cb, 0, &[1, 0], |ds| &mut ds.color_write_enables)
        .unwrap();
    {
        let ds = &d.command_buffers.get(&cb).unwrap().dynamic;
        assert_eq!(ds.color_write_masks, vec![0, 0x7, 0xF]);
        assert_eq!(ds.color_write_enables, vec![1, 0]);
    }
    // maxColorAttachments is 8: a span ending at 9 is rejected as a usage error (no giant resize).
    let big = vec![0u32; 4];
    assert!(matches!(
        record::set_dynamic_attachment_array(&mut d, cb, 6, &big, |ds| &mut ds.color_blend_enables),
        Err(GpuError::Invalid(_))
    ));
    d.end_command_buffer(cb).unwrap();
}

/// `vkCreatePipelineLayout` via `create::create_pipeline_layout` — the composed descriptor-set-layout
/// handles are recorded verbatim (pipeline compatibility is by set-layout); no IR is emitted.
#[test]
fn pipeline_layout_records_composed_set_layouts() {
    let mut d = dev();
    let sl0 = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: 0,
        descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
        descriptor_count: 1,
        stage_flags: 0x20,
    }]);
    let sl1 = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: 0,
        descriptor_type: vk_descriptor_type::UNIFORM_BUFFER,
        descriptor_count: 1,
        stage_flags: 0x20,
    }]);
    let layout = d.create_pipeline_layout(vec![sl0, sl1]);
    let rec = d
        .pipeline_layouts
        .get(&layout)
        .expect("pipeline layout recorded");
    assert_eq!(rec.set_layouts, vec![sl0, sl1]);
    // A zero-set layout (push-constant-only / empty) is equally valid.
    let empty = d.create_pipeline_layout(vec![]);
    assert!(d
        .pipeline_layouts
        .get(&empty)
        .unwrap()
        .set_layouts
        .is_empty());
    assert_ne!(layout, empty, "each layout gets a distinct handle");
}

/// `vkQueueSubmit(2)` timeline signal via `submit::signal_timeline_values` — the SUBMIT-side timeline
/// signal path (distinct from the host `vkSignalSemaphore`): after the synchronous replay it advances
/// each signalled TIMELINE semaphore's counter monotonically (max), and SKIPS binary/unknown semaphores.
#[test]
fn submit_side_timeline_signal_advances_counter_monotonically() {
    let mut d = dev();
    let tl = sync::create_semaphore(&mut d, true, 2); // timeline, initial 2
    let bin = sync::create_semaphore(&mut d, false, 0); // binary — must be skipped

    // Signal timeline→5 and binary→9 (binary value ignored) in one batch.
    d.signal_timeline_values(&[(tl, 5), (bin, 9)]);
    assert_eq!(d.semaphore_counter(tl).unwrap(), 5);
    assert!(
        d.semaphore_counter(bin).is_err(),
        "binary semaphore has no counter"
    );

    // A lower value never regresses the counter (monotonic max); an unknown handle is a silent skip.
    d.signal_timeline_values(&[(tl, 3), (0xdead_beef, 100)]);
    assert_eq!(d.semaphore_counter(tl).unwrap(), 5);

    // A consumer waiting on the signalled value is already satisfied.
    assert!(sync::wait_semaphores(&d, &[tl], &[5], false));
    assert!(!sync::wait_semaphores(&d, &[tl], &[6], false));
}
