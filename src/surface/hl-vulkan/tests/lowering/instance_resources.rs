use super::*;

#[test]
fn physical_device_reports_metal_class_props() {
    let inst = Instance::new(result::HL_API_VERSION);
    let pd = &inst.physical_device;
    assert_eq!(pd.name, "hl Metal (Vulkan)");
    assert_eq!(pd.api_version, result::HL_API_VERSION); // Vulkan 1.3.0
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
    create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0], Some(3));
    match &sink.batches[0][0] {
        Cmd::CreateSampler(1, desc) => assert_eq!(desc.compare, Some(3)),
        other => panic!("expected comparison sampler, got {other:?}"),
    }

    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0], None);
    match &sink.batches[0][0] {
        Cmd::CreateSampler(1, desc) => assert_eq!(desc.compare, None),
        other => panic!("expected ordinary sampler, got {other:?}"),
    }
}

#[test]
fn create_sampler_preserves_vk_filter_cubic_ext() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    create::create_sampler(
        &mut d,
        &mut sink,
        1_000_015_000,
        1_000_015_000,
        0,
        [2, 2, 2],
        None,
    );
    let Cmd::CreateSampler(_, desc) = &sink.batches[0][0] else {
        panic!("expected CreateSampler");
    };
    assert_eq!(desc.min_filter, hl_gpu::protocol::model::enums::Filter::Cubic);
    assert_eq!(desc.mag_filter, hl_gpu::protocol::model::enums::Filter::Cubic);
}

#[test]
fn create_sampler_preserves_border_color_and_lod_clamps() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    create::create_sampler_full(&mut d, &mut sink, 1, 1, 1, [3, 3, 3], [-2.5, 7.25], 5, None);
    let Cmd::CreateSampler(_, desc) = &sink.batches[0][0] else { panic!("expected sampler") };
    assert_eq!(desc.address_u, hl_gpu::protocol::model::enums::AddressMode::ClampToBorder);
    assert_eq!(desc.border_color, hl_gpu::protocol::model::enums::BorderColor::IntOpaqueWhite);
    assert_eq!([desc.lod_min_clamp, desc.lod_max_clamp], [-2.5, 7.25]);
}

#[test]
fn image_sampler_and_fence_destruction_release_ir_objects_once() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let image = create::create_image(
        &mut d,
        &mut sink,
        8,
        8,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let sampler = create::create_sampler(&mut d, &mut sink, 0, 0, 0, [0, 0, 0], None);
    let fence = create::create_fence(&mut d, &mut sink, false).unwrap();

    create::destroy_image(&mut d, &mut sink, image).unwrap();
    create::destroy_sampler(&mut d, &mut sink, sampler).unwrap();
    create::destroy_fence(&mut d, &mut sink, fence).unwrap();
    assert!(matches!(sink.batches[3][0], Cmd::DestroyTexture(1)));
    assert!(matches!(sink.batches[4][0], Cmd::DestroySampler(2)));
    assert!(matches!(sink.batches[5][0], Cmd::DestroyFence(3)));

    let before = sink.batches.len();
    create::destroy_image(&mut d, &mut sink, image).unwrap();
    create::destroy_sampler(&mut d, &mut sink, sampler).unwrap();
    create::destroy_fence(&mut d, &mut sink, fence).unwrap();
    assert_eq!(sink.batches.len(), before);
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
fn shader_and_pipeline_destruction_release_ir_objects_once() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();
    let shader = create::create_shader_module_words(
        &mut d,
        &mut sink,
        spirv::Module::sample_compute("main"),
    )
    .unwrap();
    let pipeline = create::create_compute_pipeline(&mut d, &mut sink, shader, "main").unwrap();

    create::destroy_pipeline(&mut d, &mut sink, pipeline).unwrap();
    create::destroy_shader_module(&mut d, &mut sink, shader).unwrap();
    assert!(matches!(sink.batches[2][0], Cmd::DestroyPipeline(2)));
    assert!(matches!(sink.batches[3][0], Cmd::DestroyShader(1)));

    let before = sink.batches.len();
    create::destroy_pipeline(&mut d, &mut sink, pipeline).unwrap();
    create::destroy_shader_module(&mut d, &mut sink, shader).unwrap();
    assert_eq!(sink.batches.len(), before);
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
