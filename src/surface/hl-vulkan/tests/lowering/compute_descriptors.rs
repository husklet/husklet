use super::*;

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
