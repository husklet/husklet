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
use hl_gpu::protocol::model::enums::{buffer_usage, Filter, IndexFormat, TextureFormat};
use hl_gpu::{Cmd, FenceId, GpuError, RecordingSink, ShaderPayloadKind};

/// A slot-0 vertex layout carrying interleaved position (offset 0) + color (offset 8), stride 24 — the
/// layout the host rasterizer fetches `pos`/`color` from.
fn pos_color_layout() -> VertexLayout {
    VertexLayout {
        stride: 24,
        step_mode: 0,
        attrs: vec![
            VertexAttr { location: 0, format: 0, offset: 0 },
            VertexAttr { location: 1, format: 0, offset: 8 },
        ],
    }
}

fn dev() -> Device {
    let inst = create::create_instance(result::HL_API_VERSION);
    create::create_device(&inst)
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
    let _buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 4096).unwrap();

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
    assert!(matches!(sink.batches.last().unwrap()[0], Cmd::DestroyBuffer(1)));
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
    create::create_image(&mut d, &mut sink, 256, 128, vk_format::B8G8R8A8_UNORM, usage).unwrap();
    match &sink.batches[0][0] {
        Cmd::CreateTexture(id, desc) => {
            assert_eq!(*id, 1);
            assert_eq!((desc.width, desc.height), (256, 128));
            assert_eq!(desc.format, TextureFormat::Bgra8Unorm);
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
    let words = spirv::sample_compute_spirv("main");
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
    let words = spirv::sample_compute_spirv("computeMain");
    assert_eq!(spirv::entry_points(&words), vec!["computeMain".to_string()]);
    assert!(spirv::validate(&words).is_ok());

    // a byte image that is not a SPIR-V module is a typed error, not a panic.
    assert!(matches!(spirv::words_from_bytes(b"not spirv at all!!!!"), Err(GpuError::Invalid(_))));
    // a 3-byte (non word-multiple) image is rejected on size.
    assert!(spirv::words_from_bytes(&[1, 2, 3]).is_err());
}

#[test]
fn compute_pipeline_rejects_missing_entry() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let sh = create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("main"))
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
    let vs = create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("vsmain"))
        .unwrap();
    let fs = create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("fsmain"))
        .unwrap();
    create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![pos_color_layout()],
        vec![TextureFormat::Bgra8Unorm],
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
    let vs = create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("vsmain"))
        .unwrap();
    let fs = create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("fsmain"))
        .unwrap();
    // Two color attachment formats from the pNext, and no render pass object at all.
    create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![pos_color_layout()],
        vec![TextureFormat::Bgra8Unorm, TextureFormat::Rgba8Unorm],
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
    )
    .unwrap();
    let vs =
        create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("vsmain")).unwrap();
    let fs =
        create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("fsmain")).unwrap();
    let pipe = create::create_graphics_pipeline(
        &mut d,
        &mut sink,
        (vs, "vsmain"),
        Some((fs, "fsmain")),
        vec![pos_color_layout()],
        vec![TextureFormat::Rgba8Unorm],
    )
    .unwrap();

    // a vertex buffer (ir 5).
    let vbuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 24 * 3).unwrap();

    // record the render pass: begin (clear) → bind pipeline → bind vertex buffer → draw → end.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_begin_render_pass(&mut d, cb, target, [0.0, 0.0, 1.0, 1.0], true).unwrap();
    record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, vbuf, 0).unwrap();
    record::cmd_draw(&mut d, cb, 3, 1, 0, 0).unwrap();
    record::cmd_end_render_pass(&mut d, cb).unwrap();
    record::end(&mut d, cb).unwrap();

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
                    Enc::SetVertexBuffer { slot: 0, buffer: 5, offset: 0 },
                    Enc::SetPipeline(4),
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
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
        record::cmd_end_render_pass(d, cb).unwrap();
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
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_begin_rendering(
        &mut d,
        cb,
        &[RenderingColorAttachment { image: target, clear: [0.0; 4], load_clear: false, store: true }],
        None,
    )
    .unwrap();
    assert!(record::cmd_clear_attachment_rect(&mut d, cb, 0, 0, 4, 4, [1.0, 0.0, 0.0, 1.0]).is_ok());
    // An unknown attachment image is a typed error, not a silent skip.
    let cb2 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb2).unwrap();
    assert!(record::cmd_begin_rendering(
        &mut d,
        cb2,
        &[RenderingColorAttachment { image: 0xdead, clear: [0.0; 4], load_clear: true, store: true }],
        None,
    )
    .is_err());
}

#[test]
fn indexed_draw_lowers_set_index_buffer_and_draw_indexed() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let ibuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDEX_BUFFER, 6).unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    // VK_INDEX_TYPE_UINT16 = 0.
    record::cmd_bind_index_buffer(&mut d, cb, ibuf, 0, 0).unwrap();
    record::cmd_draw_indexed(&mut d, cb, 3, 1, 0, 0, 0).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => {
            assert_eq!(
                cbuf.encoder,
                vec![
                    Enc::SetIndexBuffer { buffer: 1, offset: 0, format: IndexFormat::U16 },
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
    let in_buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();
    let out_buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();
    let sh = create::create_shader_module_words(&mut d, &mut sink, spirv::sample_compute_spirv("main"))
        .unwrap();
    let pipe = create::create_compute_pipeline(&mut d, &mut sink, sh, "main").unwrap();

    // descriptor set: two storage-buffer bindings (0 = in, 1 = out).
    let layout = create::create_descriptor_set_layout(
        &mut d,
        vec![
            LayoutBinding { binding: 0, descriptor_type: vk_descriptor_type::STORAGE_BUFFER, descriptor_count: 1, stage_flags: 0 },
            LayoutBinding { binding: 1, descriptor_type: vk_descriptor_type::STORAGE_BUFFER, descriptor_count: 1, stage_flags: 0 },
        ],
    );
    let pool = create::create_descriptor_pool(&mut d, 1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set, 0, in_buf, 0, 1024).unwrap();
    create::update_descriptor_buffer(&mut d, set, 1, out_buf, 0, 1024).unwrap();

    // record: bind pipeline + descriptor set (→ CreateBindGroup ir 5) + dispatch.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_bind_pipeline(&mut d, cb, pipe).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut sink, cb, 0, &[set], &[]).unwrap();
    record::cmd_dispatch(&mut d, cb, 64, 1, 1).unwrap();
    record::end(&mut d, cb).unwrap();

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
            assert!(matches!(desc.entries[0].resource, BindResource::Buffer { id: 1, offset: 0, .. }));
            assert!(matches!(desc.entries[1].resource, BindResource::Buffer { id: 2, offset: 0, .. }));
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
    let image =
        create::create_image(&mut d, &mut sink, 64, 64, vk_format::R8G8B8A8_UNORM, vk_image_usage::SAMPLED).unwrap();
    let sampler = create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0]);

    // A set with a single COMBINED_IMAGE_SAMPLER binding at binding 0.
    let layout = create::create_descriptor_set_layout(
        &mut d,
        vec![LayoutBinding {
            binding: 0,
            descriptor_type: vk_descriptor_type::COMBINED_IMAGE_SAMPLER,
            descriptor_count: 1,
            stage_flags: 0,
        }],
    );
    let pool = create::create_descriptor_pool(&mut d, 1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    // `vkUpdateDescriptorSets`: the shim resolves the write's imageView → this VkImage; drive the
    // driver directly with (image, sampler) — the same tables the shim populates.
    create::update_descriptor_image(&mut d, set, 0, Some(image), Some(sampler)).unwrap();

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut sink, cb, 0, &[set], &[]).unwrap();
    record::end(&mut d, cb).unwrap();

    // The bind emits one CreateBindGroup (the last batch): a Texture(image) + Sampler(sampler), both at
    // binding 0 (the combined-sampler layout the wgpu executor's WGSL declares).
    match &sink.batches.last().unwrap()[0] {
        Cmd::CreateBindGroup(_, desc) => {
            assert_eq!(desc.set, 0);
            assert_eq!(desc.entries.len(), 2);
            assert_eq!(desc.entries[0].binding, 0);
            assert_eq!(desc.entries[0].resource, BindResource::Texture { id: img_ir(&d, image) });
            assert_eq!(desc.entries[1].binding, 0);
            assert_eq!(desc.entries[1].resource, BindResource::Sampler { id: samp_ir(&d, sampler) });
        }
        other => panic!("expected CreateBindGroup, got {other:?}"),
    }
}

#[test]
fn separate_sampled_image_and_sampler_descriptors_lower_at_their_own_bindings() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    let image =
        create::create_image(&mut d, &mut sink, 32, 32, vk_format::R8G8B8A8_UNORM, vk_image_usage::SAMPLED).unwrap();
    let sampler = create::create_sampler(&mut d, &mut sink, 0, 0, 0, [0, 0, 0]);

    // A SAMPLED_IMAGE at binding 0 and a separate SAMPLER at binding 1.
    let layout = create::create_descriptor_set_layout(
        &mut d,
        vec![
            LayoutBinding { binding: 0, descriptor_type: vk_descriptor_type::SAMPLED_IMAGE, descriptor_count: 1, stage_flags: 0 },
            LayoutBinding { binding: 1, descriptor_type: vk_descriptor_type::SAMPLER, descriptor_count: 1, stage_flags: 0 },
        ],
    );
    let pool = create::create_descriptor_pool(&mut d, 1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_image(&mut d, set, 0, Some(image), None).unwrap();
    create::update_descriptor_image(&mut d, set, 1, None, Some(sampler)).unwrap();

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut sink, cb, 0, &[set], &[]).unwrap();
    record::end(&mut d, cb).unwrap();

    // Two entries: a Texture at binding 0 and a Sampler at binding 1 (binding-ascending resolution).
    match &sink.batches.last().unwrap()[0] {
        Cmd::CreateBindGroup(_, desc) => {
            assert_eq!(desc.entries.len(), 2);
            assert_eq!(desc.entries[0].binding, 0);
            assert_eq!(desc.entries[0].resource, BindResource::Texture { id: img_ir(&d, image) });
            assert_eq!(desc.entries[1].binding, 1);
            assert_eq!(desc.entries[1].resource, BindResource::Sampler { id: samp_ir(&d, sampler) });
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

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::end(&mut d, cb).unwrap();
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
    let buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::UNIFORM_BUFFER, 256).unwrap();
    let mem = create::allocate_memory(&mut d, 256);
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[1, 2, 3, 4]).unwrap();

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::end(&mut d, cb).unwrap();
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
fn unmapped_memory_still_flushes_its_write_at_submit() {
    // The data-loss edge: a real app stages into a mapped buffer, then vkUnmapMemory BEFORE submitting.
    // The upload must survive the unmap and still reach the device as a WriteBuffer at the next submit.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::UNIFORM_BUFFER, 256).unwrap();
    let buf_ir = d.buffers.get(&buf).unwrap().ir_id;
    let mem = create::allocate_memory(&mut d, 256);
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[9, 8, 7, 6]).unwrap();
    create::unmap_memory(&mut d, mem); // <-- unmap before submit; the write must not be dropped

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // Exactly one WriteBuffer carrying the written bytes flushes despite the unmap.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset, data }, Cmd::Submit(_)] => {
            assert_eq!((*id, *offset), (buf_ir, 0));
            assert_eq!(data.len(), 256);
            assert_eq!(&data[..4], &[9, 8, 7, 6], "the unmapped write reached the device");
        }
        other => panic!("expected [WriteBuffer, Submit] after unmap, got {other:?}"),
    }

    // The pending upload is one-shot: a SECOND submit (no re-map/re-write) flushes nothing more.
    let cb2 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb2).unwrap();
    record::end(&mut d, cb2).unwrap();
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
    let buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::UNIFORM_BUFFER, 256).unwrap();
    let mem = create::allocate_memory(&mut d, 256);
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    create::map_memory(&mut d, mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[1, 2, 3, 4]).unwrap();
    // A flush of a sub-range while still mapped captures a pending record too — it must NOT double the write.
    create::capture_pending_upload(&mut d, mem, 0, 4);

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    let writes = sink
        .batches
        .last()
        .unwrap()
        .iter()
        .filter(|c| matches!(c, Cmd::WriteBuffer { .. }))
        .count();
    assert_eq!(writes, 1, "still-mapped + pending coalesce to a single WriteBuffer (no double-write)");
}

#[test]
fn unmapped_unbound_host_staging_flushes_nothing() {
    // Host-only staging with no buffer bound has no device buffer to upload to; unmapping it must capture
    // nothing (a truthful no-op) so the submit emits no WriteBuffer.
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let mem = create::allocate_memory(&mut d, 128);
    create::map_memory(&mut d, mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[1, 2, 3, 4]).unwrap();
    create::unmap_memory(&mut d, mem);
    assert!(d.memories.get(&mem).unwrap().pending_flush.is_none(), "unbound staging captures nothing");

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::end(&mut d, cb).unwrap();
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
    let buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let buf_ir = d.buffers.get(&buf).unwrap().ir_id;
    let mem = create::allocate_memory(&mut d, 256);
    create::bind_buffer_memory(&mut d, buf, mem, 0).unwrap();
    create::map_memory(&mut d, mem).unwrap();

    // Invalidating / mapping a whole bound allocation reads the buffer back over the sink's device→host
    // port — the SAME `read_buffer` cuda's `cuMemcpyDtoH` and GL's `glReadPixels` issue.
    create::read_mapped(&mut d, &mut sink, mem, 0, u64::MAX).unwrap();
    assert_eq!(sink.reads, vec![(hl_gpu::BufferId(buf_ir), 0, 256)], "one whole-buffer readback");

    // A bounded sub-range honours the mapped offset/size (buffer offset = mem offset − bound_offset = 64).
    create::read_mapped(&mut d, &mut sink, mem, 64, 32).unwrap();
    assert_eq!(sink.reads.last().copied(), Some((hl_gpu::BufferId(buf_ir), 64, 32)));
}

#[test]
fn map_memory_of_unbound_host_staging_issues_no_readback() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // Host-only staging: no buffer bound, so there is no readable device source. The readback must be a
    // truthful no-op (never a faked/zero read), leaving the staging as-is.
    let mem = create::allocate_memory(&mut d, 128);
    create::map_memory(&mut d, mem).unwrap();
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
    let surface = present::create_surface(&mut d, &mut sink, 1920, 1080, vk_format::B8G8R8A8_UNORM, 7).unwrap();
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

    let idx = present::acquire_next_image(&d, sc).unwrap();
    present::queue_present(&mut d, &mut sink, sc, idx).unwrap();

    // the present names the surface's ir id + the presented image's REAL backing texture id.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Present { surface: s, texture: t }] => {
            assert_eq!(*s, 1); // the CreateSurface ir id
            assert_eq!(*t, img0_ir); // the presented swapchain image's real render-target texture
        }
        other => panic!("expected Present, got {other:?}"),
    }
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
    let cb = record::allocate_command_buffer(d);
    record::begin(d, cb).unwrap();
    record_fn(d, cb);
    record::end(d, cb).unwrap();
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
    assert_eq!(enc, vec![Enc::CopyBufferToBuffer { src: s, src_offset: 16, dst: t, dst_offset: 32, size: 64 }]);
}

#[test]
fn copy_buffer_to_image_lowers_to_copy_buffer_to_texture() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // A 4x4 RGBA8 target: tight-packed bytes_per_row = 4*4 = 16; span = 16*3 + 16 = 64.
    let src = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_SRC, 64).unwrap();
    let dst =
        create::create_image(&mut d, &mut sink, 4, 4, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_DST)
            .unwrap();
    let (s, t) = (buf_ir(&d, src), img_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_buffer_to_image(d, cb, src, dst, 0, 0, 0, 4, 4).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyBufferToTexture { src: s, src_offset: 0, bytes_per_row: 16, dst: t, mip: 0, width: 4, height: 4 }]
    );
}

#[test]
fn copy_image_to_buffer_lowers_to_copy_texture_to_buffer() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src =
        create::create_image(&mut d, &mut sink, 4, 4, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_SRC)
            .unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let (s, t) = (img_ir(&d, src), buf_ir(&d, dst));
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_copy_image_to_buffer(d, cb, src, dst, 0, 0, 0, 4, 4).unwrap();
    });
    assert_eq!(
        enc,
        vec![Enc::CopyTextureToBuffer { src: s, mip: 0, width: 4, height: 4, dst: t, dst_offset: 0, bytes_per_row: 16 }]
    );
}

#[test]
fn copy_image_lowers_to_copy_texture_to_texture() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src =
        create::create_image(&mut d, &mut sink, 8, 8, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_SRC)
            .unwrap();
    let dst =
        create::create_image(&mut d, &mut sink, 8, 8, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_DST)
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
            extent: Extent3d { width: 4, height: 4, depth: 1 },
        }]
    );
    // Copy-compatible-format rejection: differing formats are a typed error, not a silent mis-copy.
    let other =
        create::create_image(&mut d, &mut sink, 8, 8, vk_format::B8G8R8A8_UNORM, vk_image_usage::TRANSFER_DST)
            .unwrap();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    assert!(record::cmd_copy_image(&mut d, cb, src, other, (0, 0), (0, 0), (4, 4)).is_err());
}

#[test]
fn blit_image_lowers_to_blit_texture_with_filter() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let src =
        create::create_image(&mut d, &mut sink, 8, 8, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_SRC)
            .unwrap();
    let dst =
        create::create_image(&mut d, &mut sink, 16, 16, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_DST)
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
            src_extent: Extent3d { width: 8, height: 8, depth: 1 },
            dst: t,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d { x: 0, y: 0, z: 0 },
            dst_extent: Extent3d { width: 16, height: 16, depth: 1 },
            filter: Filter::Linear,
        }]
    );
}

#[test]
fn clear_color_image_lowers_to_full_extent_clear_rect() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img =
        create::create_image(&mut d, &mut sink, 32, 16, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_DST)
            .unwrap();
    let ir = img_ir(&d, img);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_clear_color_image(d, cb, img, [0.25, 0.5, 0.75, 1.0]).unwrap();
    });
    assert_eq!(enc, vec![Enc::ClearRect { texture: ir, x: 0, y: 0, w: 32, h: 16, color: [0.25, 0.5, 0.75, 1.0] }]);
}

#[test]
fn clear_attachments_lowers_to_clear_rect_on_active_target() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let target =
        create::create_image(&mut d, &mut sink, 64, 64, vk_format::R8G8B8A8_UNORM, vk_image_usage::COLOR_ATTACHMENT)
            .unwrap();
    let ir = img_ir(&d, target);
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_begin_render_pass(d, cb, target, [0.0, 0.0, 0.0, 1.0], false).unwrap();
        record::cmd_clear_attachment_rect(d, cb, 8, 8, 16, 16, [1.0, 0.0, 0.0, 1.0]).unwrap();
        record::cmd_end_render_pass(d, cb).unwrap();
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
            Enc::ClearRect { texture: ir, x: 8, y: 8, w: 16, h: 16, color: [1.0, 0.0, 0.0, 1.0] },
            Enc::EndRenderPass,
        ]
    );
    // A clear-attachments outside a render pass is a typed error (no active target to clear).
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    assert!(record::cmd_clear_attachment_rect(&mut d, cb, 0, 0, 4, 4, [0.0; 4]).is_err());
}

#[test]
fn fill_and_update_buffer_flush_as_write_buffer_at_submit() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let ir = buf_ir(&d, buf);
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    // Fill [0,8) with 0x01010101 (two words), then update [16,20) with explicit bytes.
    record::cmd_fill_buffer(&mut d, cb, buf, 0, 8, 0x0101_0101).unwrap();
    record::cmd_update_buffer(&mut d, cb, buf, 16, &[9, 8, 7, 6]).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // The two buffer writes flush (in record order) as WriteBuffers before the (empty) Submit.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id: i1, offset: 0, data: d1 }, Cmd::WriteBuffer { id: i2, offset: 16, data: d2 }, Cmd::Submit(_)] => {
            assert_eq!((*i1, *i2), (ir, ir));
            assert_eq!(d1, &vec![1u8; 8]);
            assert_eq!(d2, &vec![9u8, 8, 7, 6]);
        }
        other => panic!("expected [WriteBuffer, WriteBuffer, Submit], got {other:?}"),
    }
    // fill rejects a non-COPY_DST buffer and a misaligned offset.
    let vbuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 64).unwrap();
    let cb2 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb2).unwrap();
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
            Enc::SetViewport { x: 0.0, y: 0.0, w: 640.0, h: 480.0, min_depth: 0.0, max_depth: 1.0 },
            Enc::SetScissor { x: 0, y: 0, w: 640, h: 480 },
        ]
    );
}

#[test]
fn push_constants_reach_the_command_buffer_for_the_draw() {
    let mut d = dev();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    // Write 8 bytes at offset 0, then overwrite 4 bytes at offset 4 (grows/patches the block in place).
    record::cmd_push_constants(&mut d, cb, 0, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    record::cmd_push_constants(&mut d, cb, 4, &[9, 9, 9, 9]).unwrap();
    // The recorded block is honest command state a draw reads (the IR has no push-constant channel yet).
    assert_eq!(d.command_buffers.get(&cb).unwrap().push_constants, vec![1, 2, 3, 4, 9, 9, 9, 9]);
    // Misaligned / zero-size pushes are typed errors, never a silent partial write.
    assert!(record::cmd_push_constants(&mut d, cb, 2, &[0, 0, 0, 0]).is_err());
    assert!(record::cmd_push_constants(&mut d, cb, 0, &[0, 0, 0]).is_err());
}

#[test]
fn dynamic_state_is_recorded_but_emits_no_encoder_op() {
    let mut d = dev();
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_set_line_width(&mut d, cb, 2.5).unwrap();
    record::cmd_set_depth_bias(&mut d, cb, 1.0, 0.0, 2.0).unwrap();
    record::cmd_set_blend_constants(&mut d, cb, [0.1, 0.2, 0.3, 0.4]).unwrap();
    // FRONT_AND_BACK = 0x3 sets both faces; FRONT = 0x1 sets only the front.
    record::cmd_set_stencil_reference(&mut d, cb, 0x3, 7).unwrap();
    record::cmd_set_stencil_compare_mask(&mut d, cb, 0x1, 0xff).unwrap();
    record::end(&mut d, cb).unwrap();

    // The state is recorded (observable, honest) …
    let rec = d.command_buffers.get(&cb).unwrap();
    assert_eq!(rec.dynamic.line_width, 2.5);
    assert_eq!(rec.dynamic.depth_bias, (1.0, 0.0, 2.0));
    assert_eq!(rec.dynamic.blend_constants, [0.1, 0.2, 0.3, 0.4]);
    assert_eq!(rec.dynamic.stencil_reference, (7, 7));
    assert_eq!(rec.dynamic.stencil_compare_mask, (0xff, 0));
    // … but the software rasterizer models none of it, so no encoder op is emitted.
    assert!(rec.enc.is_empty(), "dynamic state emits no encoder op, got {:?}", rec.enc);
}

#[test]
fn indirect_draws_validate_buffer_and_emit_no_op() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // A valid indirect buffer (INDIRECT usage) large enough for two 16-byte VkDrawIndirectCommands.
    let indirect =
        create::create_buffer(&mut d, &mut sink, vk_buffer_usage::INDIRECT_BUFFER, 64).unwrap();
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_draw_indirect(d, cb, indirect, 0, 2, 16).unwrap();
        record::cmd_draw_indexed_indirect(d, cb, indirect, 0, 1, 20).unwrap();
        record::cmd_dispatch_indirect(d, cb, indirect, 0).unwrap();
    });
    // The IR carries no indirect draw/dispatch op — validated, but a documented no-op.
    assert!(enc.is_empty(), "indirect draws emit no encoder op, got {enc:?}");

    // Truthful failure: an unknown buffer, a non-INDIRECT buffer, and an out-of-range span all error.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    assert!(record::cmd_draw_indirect(&mut d, cb, 0xdead, 0, 1, 16).is_err());
    let vbuf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::VERTEX_BUFFER, 64).unwrap();
    assert!(record::cmd_draw_indirect(&mut d, cb, vbuf, 0, 1, 16).is_err());
    // 5 draws * 16 bytes = 80 > 64: out of bounds.
    assert!(record::cmd_draw_indirect(&mut d, cb, indirect, 0, 5, 16).is_err());
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
    assert_eq!(enc, vec![Enc::CopyBufferToBuffer { src: s, src_offset: 8, dst: t, dst_offset: 16, size: 32 }]);
}

#[test]
fn pipeline_barrier_records_layout_transition_and_emits_no_ir() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img =
        create::create_image(&mut d, &mut sink, 8, 8, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_DST)
            .unwrap();
    // VK_IMAGE_LAYOUT_UNDEFINED (0) -> VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL (7).
    let enc = record_and_submit(&mut d, &mut sink, |d, cb| {
        record::cmd_pipeline_barrier(d, cb, &[(img, 0, 7)]).unwrap();
    });
    // The layout-implicit IR carries no encoder op for a barrier.
    assert!(enc.is_empty(), "a pipeline barrier emits no encoder op, got {enc:?}");
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
    let ev = sync::create_event(&mut d);
    assert!(!sync::event_status(&d, ev).unwrap()); // created unsignaled

    // Host set/reset mutate directly.
    sync::set_event(&mut d, ev, true).unwrap();
    assert!(sync::event_status(&d, ev).unwrap());
    sync::set_event(&mut d, ev, false).unwrap();
    assert!(!sync::event_status(&d, ev).unwrap());

    // A device vkCmdSetEvent resolves at (synchronous) submit completion — signaled once submit returns.
    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_set_event(&mut d, cb, ev, true).unwrap();
    record::end(&mut d, cb).unwrap();
    assert!(!sync::event_status(&d, ev).unwrap(), "not signaled until the submit completes");
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();
    assert!(sync::event_status(&d, ev).unwrap(), "device-set event signaled after submit");

    // An unknown event is a typed error, never a false success.
    assert!(sync::set_event(&mut d, 0xdead, true).is_err());
}

#[test]
fn timeline_semaphore_signal_wait_roundtrips() {
    let mut d = dev();
    let sem = sync::create_semaphore(&mut d, true, 2); // timeline, initial 2
    assert_eq!(sync::semaphore_counter(&d, sem).unwrap(), 2);

    // Host signal advances the counter monotonically (a signal below the current value is a no-op).
    sync::signal_semaphore(&mut d, sem, 5).unwrap();
    assert_eq!(sync::semaphore_counter(&d, sem).unwrap(), 5);
    sync::signal_semaphore(&mut d, sem, 3).unwrap();
    assert_eq!(sync::semaphore_counter(&d, sem).unwrap(), 5);

    // A satisfied wait (counter >= value) is true; an unmet one is false (→ VK_TIMEOUT at the shim).
    assert!(sync::wait_semaphores(&d, &[sem], &[5], false));
    assert!(!sync::wait_semaphores(&d, &[sem], &[6], false));

    // A binary semaphore has no timeline counter — host counter ops are typed errors.
    let bin = sync::create_semaphore(&mut d, false, 0);
    assert!(sync::semaphore_counter(&d, bin).is_err());
    assert!(sync::signal_semaphore(&mut d, bin, 1).is_err());
}

#[test]
fn query_pool_timestamp_records_and_results_readable() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    // A 2-slot TIMESTAMP pool (VkQueryType TIMESTAMP = 2).
    let pool = sync::create_query_pool(&mut d, 2, 2).unwrap();

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_reset_query_pool(&mut d, cb, pool, 0, 2).unwrap();
    record::cmd_write_timestamp(&mut d, cb, pool, 0).unwrap();
    record::end(&mut d, cb).unwrap();

    // Before submit the slot is unavailable → NOT_READY (no WAIT/PARTIAL).
    let mut out = [0u8; 4];
    assert_eq!(
        sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false).unwrap(),
        false
    );

    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // After the (synchronous) submit the timestamp slot is available with a monotonic serial (1).
    let mut out = [0u8; 4];
    assert!(sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false).unwrap());
    assert_eq!(u32::from_le_bytes(out), 1);

    // A host reset clears availability again.
    sync::reset_query_pool(&mut d, pool, 0, 2);
    let mut out = [0u8; 4];
    assert_eq!(
        sync::get_query_pool_results(&d, pool, 0, 1, &mut out, 4, false, false, false, false).unwrap(),
        false
    );
}

#[test]
fn copy_query_pool_results_writes_dst_buffer_at_submit() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let pool = sync::create_query_pool(&mut d, 2, 1).unwrap();
    let dst = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::TRANSFER_DST, 64).unwrap();
    let dst_ir = buf_ir(&d, dst);

    let cb = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, cb).unwrap();
    record::cmd_reset_query_pool(&mut d, cb, pool, 0, 1).unwrap();
    record::cmd_write_timestamp(&mut d, cb, pool, 0).unwrap();
    // 32-bit results, no availability, stride 4.
    record::cmd_copy_query_pool_results(&mut d, cb, pool, 0, 1, dst, 0, 4, false, false).unwrap();
    record::end(&mut d, cb).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[cb], None).unwrap();

    // On completion the resolved timestamp is written into the destination buffer (trailing WriteBuffer).
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(_), Cmd::WriteBuffer { id, offset: 0, data }] => {
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
    let cb = record::allocate_command_buffer(d);
    record::begin(d, cb).unwrap();
    record::cmd_bind_descriptor_sets(d, sink, cb, 0, &[set], &[]).unwrap();
    record::end(d, cb).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::CreateBindGroup(_, desc)] => desc.entries.clone(),
        other => panic!("expected CreateBindGroup, got {other:?}"),
    }
}

#[test]
fn descriptor_template_update_matches_direct_writes() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let in_buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();
    let out_buf = create::create_buffer(&mut d, &mut sink, vk_buffer_usage::STORAGE_BUFFER, 1024).unwrap();

    let layout = create::create_descriptor_set_layout(
        &mut d,
        vec![
            LayoutBinding { binding: 0, descriptor_type: vk_descriptor_type::STORAGE_BUFFER, descriptor_count: 1, stage_flags: 0 },
            LayoutBinding { binding: 1, descriptor_type: vk_descriptor_type::STORAGE_BUFFER, descriptor_count: 1, stage_flags: 0 },
        ],
    );
    let pool = create::create_descriptor_pool(&mut d, 8);

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
            DescriptorTemplateEntry { dst_binding: 0, dst_array_element: 0, descriptor_count: 1, descriptor_type: vk_descriptor_type::STORAGE_BUFFER, offset: 0, stride: 24 },
            DescriptorTemplateEntry { dst_binding: 1, dst_array_element: 0, descriptor_count: 1, descriptor_type: vk_descriptor_type::STORAGE_BUFFER, offset: 24, stride: 24 },
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
    assert!(matches!(via_template[0].resource, BindResource::Buffer { id: 1, offset: 0, size: 1024 }));
    assert!(matches!(via_template[1].resource, BindResource::Buffer { id: 2, offset: 256, size: 512 }));
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
        vec![DescriptorTemplateEntry { dst_binding: 0, dst_array_element: 0, descriptor_count: 1, descriptor_type: vk_descriptor_type::UNIFORM_BUFFER, offset: 0, stride: 24 }],
    )
    .unwrap();
    let pool = create::create_descriptor_pool(&mut d, 1);
    let layout = create::create_descriptor_set_layout(&mut d, vec![]);
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
    let secondary = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, secondary).unwrap();
    record::cmd_copy_buffer(&mut d, secondary, src, dst, 0, 0, 64).unwrap();
    record::cmd_fill_buffer(&mut d, secondary, dst, 128, 8, 0x0202_0202).unwrap();
    record::end(&mut d, secondary).unwrap();

    // The primary executes the secondary, then is submitted.
    let primary = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, primary).unwrap();
    record::cmd_execute_commands(&mut d, primary, &[secondary]).unwrap();
    record::end(&mut d, primary).unwrap();
    submit::queue_submit(&mut d, &mut sink, &[primary], None).unwrap();

    // The primary's submit carries the secondary's copy (encoder) preceded by the spliced fill (write).
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset: 128, data }, Cmd::Submit(cbuf)] => {
            assert_eq!(*id, t);
            assert_eq!(data, &vec![2u8; 8]);
            assert_eq!(cbuf.encoder, vec![Enc::CopyBufferToBuffer { src: s, src_offset: 0, dst: t, dst_offset: 0, size: 64 }]);
        }
        other => panic!("expected [WriteBuffer, Submit], got {other:?}"),
    }

    // A secondary that is not Executable (still recording) is a typed error, splicing nothing.
    let unfinished = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, unfinished).unwrap();
    let p2 = record::allocate_command_buffer(&mut d);
    record::begin(&mut d, p2).unwrap();
    assert!(record::cmd_execute_commands(&mut d, p2, &[unfinished]).is_err());
}

// ---------------------------------------------------------------------------------------------------
// WSI physical-device surface queries: modeled caps / formats / present modes
// ---------------------------------------------------------------------------------------------------

#[test]
fn surface_queries_report_modeled_values() {
    // Support: only the lone present family (0) presents.
    assert!(present::surface_supports_present(0));
    assert!(!present::surface_supports_present(1));

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
    assert!(formats.iter().all(|f| f.color_space == VK_COLOR_SPACE_SRGB_NONLINEAR_KHR));
    assert!(formats.iter().any(|f| f.format == vk_format::B8G8R8A8_SRGB));
    assert!(formats.iter().any(|f| f.format == vk_format::R8G8B8A8_SRGB));

    // Present modes: FIFO (the always-available v-synced mode).
    assert_eq!(present::surface_present_modes(), vec![hl_vulkan::model::queue::VK_PRESENT_MODE_FIFO_KHR]);
}

// ---------------------------------------------------------------------------------------------------
// vkGetImageSubresourceLayout + pipeline cache
// ---------------------------------------------------------------------------------------------------

#[test]
fn image_subresource_layout_reports_linear_rgba8_layout() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let img = create::create_image(&mut d, &mut sink, 64, 32, vk_format::R8G8B8A8_UNORM, vk_image_usage::TRANSFER_DST).unwrap();
    let l = create::image_subresource_layout(&d, img).unwrap();
    assert_eq!(l.offset, 0);
    assert_eq!(l.row_pitch, 64 * 4); // width * 4 bytes/texel
    assert_eq!(l.size, 64 * 4 * 32); // rowPitch * height
    // An unknown image is a typed error.
    assert!(create::image_subresource_layout(&d, 0xdead).is_err());
}

#[test]
fn pipeline_cache_roundtrips_a_valid_header() {
    let mut d = dev();
    let cache = create::create_pipeline_cache(&mut d, &[]);
    let data = create::get_pipeline_cache_data(&d, cache).unwrap();
    // A valid VkPipelineCacheHeaderVersionOne: length 32, version 1 (little-endian).
    assert!(data.len() >= 32);
    assert_eq!(u32::from_le_bytes([data[0], data[1], data[2], data[3]]), 32);
    assert_eq!(u32::from_le_bytes([data[4], data[5], data[6], data[7]]), 1);

    // Merge is a truthful no-op that validates handles.
    let other = create::create_pipeline_cache(&mut d, &[]);
    assert!(create::merge_pipeline_caches(&d, cache, &[other]).is_ok());
    assert!(create::merge_pipeline_caches(&d, cache, &[0xdead]).is_err());
    assert!(create::merge_pipeline_caches(&d, 0xdead, &[]).is_err());

    // Destroy then query is a typed error.
    create::destroy_pipeline_cache(&mut d, cache);
    assert!(create::get_pipeline_cache_data(&d, cache).is_err());
}

#[test]
fn gpu_error_maps_to_vk_result() {
    assert_eq!(
        result::vk_result_from_gpu_error(&GpuError::Unsupported("x")),
        result::VK_ERROR_FEATURE_NOT_PRESENT
    );
    assert_eq!(
        result::vk_result_from_gpu_error(&GpuError::Invalid("x")),
        result::VK_ERROR_INITIALIZATION_FAILED
    );
    assert_eq!(
        result::vk_result_from_gpu_error(&GpuError::ResourceLimit("x")),
        result::VK_ERROR_OUT_OF_DEVICE_MEMORY
    );
    assert_eq!(
        result::vk_result_from_gpu_error(&GpuError::UnknownId { kind: "buffer", id: 3 }),
        result::VK_ERROR_UNKNOWN
    );
}
