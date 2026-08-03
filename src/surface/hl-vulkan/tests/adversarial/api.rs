use super::*;

struct CommitThenLoseResponse {
    inner: RecordingSink,
    fired: bool,
}

impl hl_gpu::CommandSink for CommitThenLoseResponse {
    fn negotiate(
        &mut self,
        request: &hl_gpu::FeatureRequest,
    ) -> hl_gpu::Result<hl_gpu::Capabilities> {
        self.inner.negotiate(request)
    }

    fn submit(&mut self, batch: &[Cmd]) -> hl_gpu::Result<()> {
        let texture_create = batch
            .iter()
            .any(|command| matches!(command, Cmd::CreateTexture(..)));
        self.inner.submit(batch)?;
        if texture_create && !self.fired {
            self.fired = true;
            return Err(GpuError::Transport(
                hl_gpu::transport::TransportError::Ambiguous {
                    phase: hl_gpu::transport::TransportPhase::Acknowledgement,
                    detail: "injected response loss after commit".into(),
                },
            ));
        }
        Ok(())
    }

    fn wait(&mut self, fence: hl_gpu::FenceId, value: u64) -> hl_gpu::Result<()> {
        self.inner.wait(fence, value)
    }
}

#[test]
fn buffer_view_preserves_format_range_and_rejects_bad_bounds() {
    let mut d = dev();
    let mut s = sink();
    let buffer = create::create_buffer(
        &mut d,
        &mut s,
        vk_buffer_usage::UNIFORM_TEXEL_BUFFER | vk_buffer_usage::STORAGE_TEXEL_BUFFER,
        256,
    )
    .unwrap();
    let view =
        create::create_buffer_view(&mut d, buffer, TextureFormat::Rgba8Unorm, 16, 64).unwrap();
    let record = d.buffer_views.get(&view).unwrap();
    assert_eq!(record.buffer, buffer);
    assert_eq!(record.format, TextureFormat::Rgba8Unorm);
    assert_eq!((record.offset, record.range), (16, 64));

    assert!(create::create_buffer_view(&mut d, buffer, TextureFormat::Rgba8Unorm, 4, 64).is_err());
    assert!(
        create::create_buffer_view(&mut d, buffer, TextureFormat::Rgba8Unorm, 240, 32).is_err()
    );
    create::destroy_buffer_view(&mut d, view);
    assert!(!d.buffer_views.contains_key(&view));
}

#[test]
fn texel_descriptor_binds_exact_view_metadata_into_ir() {
    let mut d = dev();
    let mut s = sink();
    let buffer =
        create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_TEXEL_BUFFER, 256).unwrap();
    let ir = buf_ir(&d, buffer);
    let view =
        create::create_buffer_view(&mut d, buffer, TextureFormat::Rgba8Unorm, 16, 64).unwrap();
    let layout = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: 3,
        descriptor_type: vk_descriptor_type::STORAGE_TEXEL_BUFFER,
        descriptor_count: 1,
        stage_flags: 0,
    }]);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_texel_buffer_element(&mut d, set, 3, 0, view).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[set], &[]).unwrap();

    let group = last_bind_group(&s);
    assert!(matches!(
        &group.entries[0].resource,
        hl_gpu::protocol::model::descriptor::BindResource::TexelBuffer {
            id,
            offset: 16,
            size: 64,
            format: TextureFormat::Rgba8Unorm,
            writable: true,
        } if *id == ir
    ));
}

#[test]
fn submit_unknown_fence_errors_before_emitting() {
    let mut d = dev();
    let mut s = sink();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    assert!(matches!(
        submit::queue_submit(&mut d, &mut s, &[cb], Some(0xdead)),
        Err(GpuError::Invalid(_))
    ));
    assert!(
        s.batches.is_empty(),
        "a bad fence fails before any Cmd is submitted"
    );
}

// =====================================================================================================
// present / WSI
// =====================================================================================================

#[test]
fn present_unknown_swapchain_and_out_of_range_index() {
    let mut d = dev();
    let mut s = sink();
    assert!(matches!(
        present::create_swapchain(&mut d, &mut s, 0xdead, 2),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        d.acquire_next_image(0xdead),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        present::queue_present(&mut d, &mut s, 0xdead, 0, None),
        Err(GpuError::Invalid(_))
    ));

    let surf =
        present::create_surface(&mut d, &mut s, 64, 64, vk_format::B8G8R8A8_UNORM, None).unwrap();
    let sc = present::create_swapchain(&mut d, &mut s, surf, 2).unwrap();
    assert_eq!(d.acquire_next_image(sc).unwrap(), 0);
    // An image index past the swapchain image count is rejected.
    assert!(matches!(
        present::queue_present(&mut d, &mut s, sc, 99, None),
        Err(GpuError::Invalid(_))
    ));
    // A valid present emits Cmd::Present naming the surface's ir + the presented image's REAL texture id.
    present::queue_present(&mut d, &mut s, sc, 0, None).unwrap();
    assert!(!s.commands().any(|c| matches!(c, Cmd::Present { .. })));
}

#[test]
fn surface_queries_report_modeled_values() {
    assert!(present::QueueFamily(0).supports_present());
    assert!(!present::QueueFamily(1).supports_present());
    let caps = present::surface_capabilities();
    assert_eq!(caps.min_image_count, 2);
    assert_eq!(caps.max_image_count, 3);
    assert_eq!(present::surface_present_modes(), vec![2]); // FIFO only
    assert_eq!(present::surface_formats().len(), 4);
}

#[test]
fn replacement_retires_only_the_matching_native_window() {
    let mut d = dev();
    let mut s = sink();
    let app_surface = 0x5000;
    let target = hl_vulkan::model::queue::PresentationTarget::Window(0x777);
    let old_surface =
        present::create_surface(&mut d, &mut s, 64, 64, vk_format::B8G8R8A8_UNORM, None).unwrap();
    let old = present::create_swapchain_for_target(
        &mut d,
        &mut s,
        old_surface,
        app_surface,
        target,
        2,
        0,
    )
    .unwrap();
    let acquired = d.acquire_next_image(old).unwrap();
    assert!(present::has_active_swapchain(&d, target));
    assert!(present::validate_swapchain(&d, valid_swapchain_config()).is_err());

    let matching = present::SwapchainConfig {
        old_swapchain: old,
        ..valid_swapchain_config()
    };
    assert!(present::validate_swapchain(&d, matching).is_ok());
    assert!(present::validate_swapchain(
        &d,
        present::SwapchainConfig {
            application_surface: app_surface + 1,
            presentation_target: hl_vulkan::model::queue::PresentationTarget::Window(0x778),
            old_swapchain: old,
            ..valid_swapchain_config()
        }
    )
    .is_err());

    let foreign_surface =
        present::create_surface(&mut d, &mut s, 64, 64, vk_format::B8G8R8A8_UNORM, None).unwrap();
    let swapchain_count = d.swapchains.len();
    assert!(present::create_swapchain_for_target(
        &mut d,
        &mut s,
        foreign_surface,
        app_surface + 1,
        hl_vulkan::model::queue::PresentationTarget::Window(0x778),
        2,
        old,
    )
    .is_err());
    assert_eq!(d.swapchains.len(), swapchain_count);
    assert!(!d.swapchains.get(&old).unwrap().retired);

    let replacement_surface =
        present::create_surface(&mut d, &mut s, 64, 64, vk_format::B8G8R8A8_UNORM, None).unwrap();
    present::create_swapchain_for_target(
        &mut d,
        &mut s,
        replacement_surface,
        app_surface + 1,
        target,
        2,
        old,
    )
    .unwrap();

    assert!(d.swapchains.get(&old).unwrap().retired);
    assert!(d.acquire_next_image(old).is_err());
    present::queue_present(&mut d, &mut s, old, acquired, None)
        .expect("an image acquired before retirement remains presentable");
    assert!(d.acquire_next_image(old).is_err());
    assert!(present::validate_swapchain(&d, matching).is_err());
}

#[test]
fn failed_replacement_still_retires_old_and_preserves_acquired_present() {
    let mut d = dev();
    let mut s = sink();
    let target = hl_vulkan::model::queue::PresentationTarget::Window(0x900);
    let old_surface =
        present::create_surface(&mut d, &mut s, 64, 64, vk_format::B8G8R8A8_UNORM, None).unwrap();
    let old =
        present::create_swapchain_for_target(&mut d, &mut s, old_surface, 0x5000, target, 2, 0)
            .unwrap();
    let acquired = d.acquire_next_image(old).unwrap();
    let surfaces_before = d.surfaces.len();
    let images_before = d.images.len();
    let replacement_surface =
        present::create_surface(&mut d, &mut s, 64, 64, vk_format::B8G8R8A8_UNORM, None).unwrap();
    let mut failing = CommitThenLoseResponse {
        inner: sink(),
        fired: false,
    };
    assert!(present::create_swapchain_for_target(
        &mut d,
        &mut failing,
        replacement_surface,
        0x5001,
        target,
        2,
        old,
    )
    .is_err());
    assert_eq!(d.surfaces.len(), surfaces_before);
    assert_eq!(d.images.len(), images_before);
    assert_eq!(
        failing
            .inner
            .commands()
            .filter(|command| matches!(command, Cmd::CreateTexture(..)))
            .count(),
        2,
        "the response is lost only after the complete create transaction commits"
    );
    assert_eq!(
        failing
            .inner
            .commands()
            .filter(|command| matches!(command, Cmd::DestroyTexture(..)))
            .count(),
        2,
        "reconciliation includes every attempted texture id"
    );
    assert!(d.swapchains.get(&old).unwrap().retired);
    assert!(d.acquire_next_image(old).is_err());
    present::queue_present(&mut d, &mut s, old, acquired, None)
        .expect("an image acquired before failed replacement remains presentable");
}

#[test]
fn exhausted_swapchain_pool_never_reissues_an_owned_image() {
    let mut d = dev();
    let mut s = sink();
    let surface =
        present::create_surface(&mut d, &mut s, 64, 64, vk_format::B8G8R8A8_UNORM, None).unwrap();
    let swapchain = present::create_swapchain(&mut d, &mut s, surface, 2).unwrap();
    assert_eq!(d.acquire_next_image_with_timeout(swapchain, 0).unwrap(), 0);
    assert_eq!(d.acquire_next_image_with_timeout(swapchain, 0).unwrap(), 1);
    assert_eq!(
        d.acquire_next_image_with_timeout(swapchain, 0),
        Err(present::AcquireImageError::NotReady)
    );
    assert_eq!(
        d.acquire_next_image_with_timeout(swapchain, 17),
        Err(present::AcquireImageError::Timeout)
    );
    assert!(d.swapchains[&swapchain]
        .images
        .iter()
        .all(|image| image.state == hl_vulkan::model::queue::ImageState::Acquired));
}

#[test]
fn acquire_signals_and_queue_wait_consumes_binary_payload() {
    let mut d = dev();
    let mut s = sink();
    let surface =
        present::create_surface(&mut d, &mut s, 64, 64, vk_format::B8G8R8A8_UNORM, None).unwrap();
    let swapchain = present::create_swapchain(&mut d, &mut s, surface, 2).unwrap();
    let semaphore = sync::create_semaphore(&mut d, false, 0);
    let fence = create::create_fence(&mut d, &mut s, false).unwrap();

    assert_eq!(
        d.try_acquire_next_image(swapchain, semaphore, fence),
        Ok(0)
    );
    assert!(d.semaphores[&semaphore].signaled);
    assert!(d.is_fence_signaled(fence).unwrap());
    d.consume_queue_waits(&[(semaphore, 0)]).unwrap();
    assert!(!d.semaphores[&semaphore].signaled);
    assert!(d.consume_queue_waits(&[(semaphore, 0)]).is_err());
}

fn valid_swapchain_config() -> present::SwapchainConfig {
    present::SwapchainConfig {
        application_surface: 0x5000,
        application_surface_live: true,
        presentation_target: hl_vulkan::model::queue::PresentationTarget::Window(0x777),
        flags: 0,
        min_image_count: 2,
        image_format: vk_format::B8G8R8A8_UNORM,
        image_color_space: 0,
        image_extent: (64, 64),
        image_array_layers: 1,
        image_usage: vk_image_usage::COLOR_ATTACHMENT,
        image_sharing_mode: 0,
        queue_family_index_count: 0,
        queue_family_indices_valid: true,
        pre_transform: 1,
        composite_alpha: 1,
        present_mode: 2,
        old_swapchain: 0,
    }
}

#[test]
fn swapchain_validation_accepts_exactly_advertised_parameters() {
    let d = dev();
    assert!(present::validate_swapchain(&d, valid_swapchain_config()).is_ok());
    for format in present::surface_formats() {
        for present_mode in present::surface_present_modes() {
            let config = present::SwapchainConfig {
                min_image_count: present::surface_capabilities().max_image_count,
                image_format: format.format,
                image_color_space: format.color_space,
                image_extent: present::surface_capabilities().max_image_extent,
                image_usage: present::surface_capabilities().supported_usage_flags,
                present_mode,
                ..valid_swapchain_config()
            };
            assert!(
                present::validate_swapchain(&d, config).is_ok(),
                "rejected advertised combination {config:?}"
            );
        }
    }

    let invalid = [
        present::SwapchainConfig {
            application_surface_live: false,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            flags: 1,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            min_image_count: 1,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            min_image_count: 4,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            image_format: vk_format::R8_UNORM,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            image_color_space: 1,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            image_extent: (0, 64),
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            image_array_layers: 2,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            image_usage: vk_image_usage::SAMPLED,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            image_sharing_mode: 1,
            queue_family_index_count: 1,
            queue_family_indices_valid: true,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            image_sharing_mode: 1,
            queue_family_index_count: 2,
            queue_family_indices_valid: false,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            pre_transform: 2,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            composite_alpha: 2,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            present_mode: 3,
            ..valid_swapchain_config()
        },
        present::SwapchainConfig {
            old_swapchain: 0xdead,
            ..valid_swapchain_config()
        },
    ];
    for config in invalid {
        assert!(
            present::validate_swapchain(&d, config).is_err(),
            "accepted {config:?}"
        );
    }
}

// =====================================================================================================
// pipeline cache
// =====================================================================================================

#[test]
fn pipeline_cache_roundtrip_merge_and_unknown() {
    let mut d = dev();
    let header = create::PipelineCache::header(&d);
    assert_eq!(header.len(), 32);
    assert_eq!(
        u32::from_le_bytes([header[0], header[1], header[2], header[3]]),
        32
    ); // length field
    assert_eq!(
        u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
        1
    ); // version ONE

    // A short initial blob falls back to a fresh valid header.
    let c = create::PipelineCache::create(&mut d, &[1, 2, 3]);
    assert_eq!(create::PipelineCache::data(&d, c).unwrap().len(), 32);
    // A >=32-byte initial blob is retained verbatim.
    let mut blob = header.clone();
    blob.extend_from_slice(&[7u8; 8]);
    let c2 = create::PipelineCache::create(&mut d, &blob);
    assert_eq!(create::PipelineCache::data(&d, c2).unwrap(), blob);
    // Merge validates handles.
    assert!(create::PipelineCache::merge(&d, c, &[c2]).is_ok());
    assert!(matches!(
        create::PipelineCache::merge(&d, 0xdead, &[c2]),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        create::PipelineCache::merge(&d, c, &[0xdead]),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        create::PipelineCache::data(&d, 0xdead),
        Err(GpuError::Invalid(_))
    ));
    create::PipelineCache::destroy(&mut d, c);
    assert!(matches!(
        create::PipelineCache::data(&d, c),
        Err(GpuError::Invalid(_))
    ));
}

// =====================================================================================================
// descriptor update templates
// =====================================================================================================

#[test]
fn descriptor_template_array_stride_and_short_blob() {
    let mut d = dev();
    let mut s = sink();
    let b0 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let b1 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let (ir0, ir1) = (buf_ir(&d, b0), buf_ir(&d, b1));
    let layout = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: 0,
        descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
        descriptor_count: 2,
        stage_flags: 0,
    }]);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();

    // One entry: 2 array elements of a buffer descriptor at offset 0, stride 24 (sizeof VkDescriptorBufferInfo).
    let entry = DescriptorTemplateEntry {
        dst_binding: 0,
        dst_array_element: 0,
        descriptor_count: 2,
        descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
        offset: 0,
        stride: 24,
    };
    let tmpl = create::create_descriptor_update_template(
        &mut d,
        VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
        vec![entry],
    )
    .unwrap();
    // The exact byte count the shim must present: offset + (count-1)*stride + 24 = 48.
    assert_eq!(d.descriptor_template_data_len(tmpl), Some(48));

    let mut data = Vec::new();
    push_buffer_info(&mut data, b0, 0, 128);
    push_buffer_info(&mut data, b1, 8, 64);
    create::update_descriptor_set_with_template(&mut d, set, tmpl, &data).unwrap();
    // Both array elements retain their descriptor-array identity.
    let rec = d.descriptor_sets.get(&set).unwrap();
    assert_eq!(rec.buffers.get(&(0, 0)), Some(&(b0, 0, 128)));
    assert_eq!(rec.buffers.get(&(0, 1)), Some(&(b1, 8, 64)));
    let _ = (ir0, ir1);

    // A short blob (one struct missing its tail) is a truthful OutOfBounds, never a junk read.
    let short = &data[..data.len() - 1];
    assert!(matches!(
        create::update_descriptor_set_with_template(&mut d, set, tmpl, short),
        Err(GpuError::OutOfBounds)
    ));
    // Unknown template / set.
    assert!(matches!(
        create::update_descriptor_set_with_template(&mut d, set, 0xdead, &data),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        create::update_descriptor_set_with_template(&mut d, 0xdead, tmpl, &data),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn descriptor_template_wrong_type_is_unsupported() {
    let mut d = dev();
    // Only DESCRIPTOR_SET (0) templates are modeled; PUSH_DESCRIPTORS (1) is a truthful FEATURE_NOT_PRESENT.
    let err = create::create_descriptor_update_template(&mut d, 1, vec![]).unwrap_err();
    assert!(matches!(err, GpuError::Unsupported(_)));
    assert_eq!(
        Status::from_error(&err),
        result::VK_ERROR_FEATURE_NOT_PRESENT
    );
}

// =====================================================================================================
// secondary command buffers
// =====================================================================================================

#[test]
fn execute_commands_requires_recording_primary_and_executable_secondaries() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let ir = buf_ir(&d, buf);
    // A recorded, ended (Executable) secondary.
    let sec = d.allocate_command_buffer();
    d.begin_command_buffer(sec, false).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, sec, 0, buf, 0).unwrap();
    d.end_command_buffer(sec).unwrap();

    // A non-recording primary is rejected.
    let prim0 = d.allocate_command_buffer();
    assert!(matches!(
        record::cmd_execute_commands(&mut d, prim0, &[sec]),
        Err(GpuError::Invalid(_))
    ));

    // A recording primary + a NON-executable secondary is rejected, and splices nothing.
    let prim = d.allocate_command_buffer();
    d.begin_command_buffer(prim, false).unwrap();
    let not_ready = d.allocate_command_buffer();
    assert!(matches!(
        record::cmd_execute_commands(&mut d, prim, &[not_ready]),
        Err(GpuError::Invalid(_))
    ));
    assert!(d.command_buffers.get(&prim).unwrap().enc.is_empty());

    // The valid splice replays the secondary's ops into the primary.
    record::cmd_execute_commands(&mut d, prim, &[sec]).unwrap();
    d.end_command_buffer(prim).unwrap();
    submit::queue_submit(&mut d, &mut s, &[prim], None).unwrap();
    match s.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => assert_eq!(
            cbuf.encoder,
            vec![Enc::SetVertexBuffer {
                slot: 0,
                buffer: ir,
                offset: 0
            }]
        ),
        other => panic!("{other:?}"),
    }
}

// =====================================================================================================
// image subresource layout
// =====================================================================================================

#[test]
fn image_subresource_layout_and_unknown() {
    let mut d = dev();
    let mut s = sink();
    let img = create::create_image(
        &mut d,
        &mut s,
        10,
        6,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::TRANSFER_DST,
        1,
    )
    .unwrap();
    let layout = d.image_subresource_layout(img).unwrap();
    assert_eq!(layout.offset, 0);
    assert_eq!(layout.row_pitch, 40); // width*4
    assert_eq!(layout.size, 240); // row_pitch*height
    assert!(matches!(
        d.image_subresource_layout(0xdead),
        Err(GpuError::Invalid(_))
    ));
}

// =====================================================================================================
// result mapping
// =====================================================================================================

#[test]
fn gpu_error_maps_to_expected_vk_results() {
    assert_eq!(
        Status::from_error(&GpuError::Invalid("x")),
        result::VK_ERROR_INITIALIZATION_FAILED
    );
    assert_eq!(
        Status::from_error(&GpuError::OutOfBounds),
        result::VK_ERROR_MEMORY_MAP_FAILED
    );
    assert_eq!(
        Status::from_error(&GpuError::Unsupported("x")),
        result::VK_ERROR_FEATURE_NOT_PRESENT
    );
    assert_eq!(
        Status::from_error(&GpuError::ResourceLimit("x")),
        result::VK_ERROR_OUT_OF_DEVICE_MEMORY
    );
}

// =====================================================================================================
// instance version handling
// =====================================================================================================

#[test]
fn instance_records_requested_api_version() {
    let older = result::make_api_version(0, 1, 0, 0);
    let inst = Instance::new(older);
    assert_eq!(inst.app_api_version, older);
    // The physical device always advertises the ICD's own (1.4) version regardless of the app request.
    assert_eq!(inst.physical_device.api_version, result::HL_API_VERSION);
}
