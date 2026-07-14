//! Lowering tests: drive each Vulkan service against a `hl_gpu::RecordingSink` and assert the exact
//! protocol `Cmd`/`Enc` sequence the operation lowers to (plus the SPIR-V passthrough adapter).
//!
//! This is the acceptance gate for the Vulkan→IR lowering layer: no loader, no socket, no GPU — just
//! the recorded command stream, which is wire-identical to what the shipping ICD emits.

use hl_vulkan::adapter::spirv;
use hl_vulkan::model::descriptor::{vk_descriptor_type, LayoutBinding};
use hl_vulkan::model::memory::{vk_buffer_usage, vk_format, vk_image_usage};
use hl_vulkan::result;
use hl_vulkan::service::{create, present, record, submit};
use hl_vulkan::{Device, Instance};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{BindResource, VertexAttr, VertexLayout};
use hl_gpu::protocol::model::enums::{buffer_usage, IndexFormat, TextureFormat};
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
        TextureFormat::Bgra8Unorm,
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
        TextureFormat::Rgba8Unorm,
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

    let sc = present::create_swapchain(&mut d, surface, 2).unwrap();
    let idx = present::acquire_next_image(&d, sc).unwrap();
    present::queue_present(&mut d, &mut sink, sc, idx).unwrap();

    // the present names the surface's ir id + the presented image's (reserved) present texture id.
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Present { surface: s, texture: t }] => {
            assert_eq!(*s, 1); // the CreateSurface ir id
            assert_eq!(*t, 1); // PRESENT_TEXTURE_ID
        }
        other => panic!("expected Present, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------------
// result mapping
// ---------------------------------------------------------------------------------------------------

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
