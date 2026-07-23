use super::*;

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
