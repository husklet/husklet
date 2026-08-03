use super::*;

#[test]
fn cmd_outside_recording_is_rejected() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let cb = d.allocate_command_buffer();
    // Initial (not begun): a vkCmd* must fail.
    assert!(matches!(
        record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0),
        Err(GpuError::Invalid(_))
    ));
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    // Executable (ended): a vkCmd* must fail again.
    assert!(matches!(
        record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn end_without_begin_is_rejected() {
    let mut d = dev();
    let cb = d.allocate_command_buffer();
    assert!(matches!(
        d.end_command_buffer(cb),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn begin_unknown_command_buffer_errors() {
    let mut d = dev();
    assert!(matches!(
        d.begin_command_buffer(0xdead, false),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        d.end_command_buffer(0xdead),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn submit_non_executable_buffer_is_rejected() {
    let mut d = dev();
    let mut s = sink();
    let cb = d.allocate_command_buffer();
    // Initial state → not executable.
    assert!(matches!(
        submit::queue_submit(&mut d, &mut s, &[cb], None),
        Err(GpuError::Invalid(_))
    ));
    // Recording → still not executable.
    d.begin_command_buffer(cb, false).unwrap();
    assert!(matches!(
        submit::queue_submit(&mut d, &mut s, &[cb], None),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn resubmit_semantics_one_time_vs_reusable() {
    let mut d = dev();
    let mut s = sink();

    // A ONE-TIME-SUBMIT buffer is single-use: after its (synchronous) submit completes it is not
    // resubmittable, so a second submit of the same buffer is rejected.
    let once = d.allocate_command_buffer();
    d.begin_command_buffer(once, true).unwrap();
    d.end_command_buffer(once).unwrap();
    submit::queue_submit(&mut d, &mut s, &[once], None).unwrap();
    assert!(matches!(
        submit::queue_submit(&mut d, &mut s, &[once], None),
        Err(GpuError::Invalid(_))
    ));

    // A REUSABLE buffer (no ONE_TIME_SUBMIT) records once and re-submits every frame — the vkcube
    // per-image draw pattern. The synchronous executor completes each submit, returning it to Executable,
    // so repeated submits all succeed.
    let reuse = d.allocate_command_buffer();
    d.begin_command_buffer(reuse, false).unwrap();
    d.end_command_buffer(reuse).unwrap();
    for _ in 0..5 {
        submit::queue_submit(&mut d, &mut s, &[reuse], None).unwrap();
    }
}

#[test]
fn submit_unknown_command_buffer_errors() {
    let mut d = dev();
    let mut s = sink();
    assert!(matches!(
        submit::queue_submit(&mut d, &mut s, &[0xdead], None),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn begin_resets_prior_recording() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0).unwrap();
    // A fresh begin must clear the earlier SetVertexBuffer.
    d.begin_command_buffer(cb, false).unwrap();
    d.end_command_buffer(cb).unwrap();
    submit::queue_submit(&mut d, &mut s, &[cb], None).unwrap();
    match s.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cbuf)] => {
            assert!(cbuf.encoder.is_empty(), "begin cleared the prior recording")
        }
        other => panic!("{other:?}"),
    }
}

// =====================================================================================================
// buffers / images: destroy, use-after-free, double-destroy
// =====================================================================================================

#[test]
fn use_after_destroy_buffer_is_rejected() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    create::destroy_buffer(&mut d, &mut s, buf).unwrap();
    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    assert!(matches!(
        record::cmd_bind_vertex_buffer(&mut d, cb, 0, buf, 0),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn double_destroy_buffer_emits_destroy_once() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::VERTEX_BUFFER, 16).unwrap();
    let ir = buf_ir(&d, buf);
    create::destroy_buffer(&mut d, &mut s, buf).unwrap();
    let n = s.batches.len();
    create::destroy_buffer(&mut d, &mut s, buf).unwrap(); // no-op
    assert_eq!(s.batches.len(), n, "second destroy emits nothing");
    assert!(s
        .commands()
        .any(|c| matches!(c, Cmd::DestroyBuffer(i) if *i == ir)));
}

// =====================================================================================================
// shader / spirv adapter boundaries
// =====================================================================================================

#[test]
fn shader_module_rejects_malformed_spirv() {
    let mut d = dev();
    let mut s = sink();
    // Not a multiple of 4.
    assert!(create::create_shader_module(&mut d, &mut s, &[1, 2, 3]).is_err());
    // Multiple of 4 but shorter than the 5-word header.
    assert!(create::create_shader_module(&mut d, &mut s, &[0u8; 8]).is_err());
    // Full header length but wrong magic.
    let bad_magic: Vec<u8> = [1u32, 0, 0, 0, 0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .collect();
    assert!(matches!(
        create::create_shader_module(&mut d, &mut s, &bad_magic),
        Err(GpuError::Invalid(_))
    ));
}

/// A truncated `OpEntryPoint` (wordCount 1, so no operands at all) must be rejected or ignored, never
/// crash. The point-size rewrite located the end of the inline entry-point name by scanning forward
/// from word 3 without first clamping to the instruction length, so a 1-word `OpEntryPoint` sliced
/// past the instruction. In the guest cdylib (`panic = "abort"`) that kills the application process
/// with no `VkResult` — a malformed SPIR-V module is fully application-controlled input.
#[test]
fn shader_module_survives_a_truncated_entry_point_instruction() {
    let mut d = dev();
    let mut s = sink();
    // Header, then `OpDecorate %99 BuiltIn PointSize` (makes the rewrite proceed past its early
    // return), then `OpEntryPoint` with wordCount 1 and no operands.
    let words: [u32; 10] = [
        0x0723_0203,
        0x0001_0000,
        0,
        0,
        0,
        (4 << 16) | 71,
        99,
        11,
        1,
        (1 << 16) | 15,
    ];
    let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();

    // Either outcome is acceptable; aborting the process is not.
    let _ = create::create_shader_module(&mut d, &mut s, &code);
}

// =====================================================================================================
// pipelines: missing entry points, unknown modules
// =====================================================================================================

#[test]
fn compute_pipeline_unknown_module_and_missing_entry() {
    let mut d = dev();
    let mut s = sink();
    assert!(matches!(
        create::create_compute_pipeline(&mut d, &mut s, 0xdead, "main"),
        Err(GpuError::Invalid(_))
    ));
    let sh = create::create_shader_module_words(
        &mut d,
        &mut s,
        hl_vulkan::adapter::spirv::Module::sample_compute("main"),
    )
    .unwrap();
    assert!(matches!(
        create::create_compute_pipeline(&mut d, &mut s, sh, "nope"),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn graphics_pipeline_rejects_missing_fragment_entry() {
    let mut d = dev();
    let mut s = sink();
    let vs = create::create_shader_module_words(
        &mut d,
        &mut s,
        hl_vulkan::adapter::spirv::Module::sample_compute("vs"),
    )
    .unwrap();
    let fs = create::create_shader_module_words(
        &mut d,
        &mut s,
        hl_vulkan::adapter::spirv::Module::sample_compute("fs"),
    )
    .unwrap();
    // Bad fragment entry → the whole pipeline fails (no id-zero default).
    let r = create::create_graphics_pipeline(
        &mut d,
        &mut s,
        (vs, "vs"),
        Some((fs, "bad")),
        vec![],
        vec![TextureFormat::Rgba8Unorm],
        None,
        None,
        1,
        Topology::TriangleList,
        0,
        0,
        0xf,
    );
    assert!(matches!(r, Err(GpuError::Invalid(_))));
}

#[test]
fn graphics_pipeline_with_no_color_targets_is_valid() {
    let mut d = dev();
    let mut s = sink();
    let vs = create::create_shader_module_words(
        &mut d,
        &mut s,
        hl_vulkan::adapter::spirv::Module::sample_compute("vs"),
    )
    .unwrap();
    // Depth-only / no-color pipeline: an empty color-format slice is valid.
    let pipe = create::create_graphics_pipeline(
        &mut d,
        &mut s,
        (vs, "vs"),
        None,
        Vec::<VertexLayout>::new(),
        vec![],
        None,
        None,
        1,
        Topology::TriangleList,
        0,
        0,
        0xf,
    )
    .unwrap();
    match s
        .commands()
        .find(|c| matches!(c, Cmd::CreateRenderPipeline(..)))
        .unwrap()
    {
        Cmd::CreateRenderPipeline(_, desc) => {
            assert!(desc.color_targets.is_empty());
            assert!(desc.fragment.is_none());
        }
        _ => unreachable!(),
    }
    assert!(d.pipelines.contains_key(&pipe));
}

// =====================================================================================================
// descriptors: pools, dynamic offsets, multiple sets
// =====================================================================================================

#[test]
fn descriptor_pool_exhaustion_and_unknown_pool() {
    let mut d = dev();
    let layout = d.create_descriptor_set_layout(vec![]);
    assert!(matches!(
        create::allocate_descriptor_set(&mut d, 0xdead, layout, 0),
        Err(GpuError::Invalid(_))
    ));
    let pool = d.create_descriptor_pool(1);
    create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    // The pool's single set is consumed → the second allocation is a resource-limit error.
    let err = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap_err();
    assert!(matches!(err, GpuError::ResourceLimit(_)));
    assert_eq!(
        Status::from_error(&err),
        result::VK_ERROR_OUT_OF_DEVICE_MEMORY
    );
}

#[test]
fn update_descriptor_unknown_set_errors() {
    let mut d = dev();
    let mut s = sink();
    let buf = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 16).unwrap();
    assert!(matches!(
        create::update_descriptor_buffer(&mut d, 0xdead, 0, buf, 0, 16),
        Err(GpuError::Invalid(_))
    ));
    assert!(matches!(
        create::update_descriptor_image(&mut d, 0xdead, 0, None, None),
        Err(GpuError::Invalid(_))
    ));
}

#[test]
fn dynamic_offsets_apply_to_dynamic_bindings_only() {
    let mut d = dev();
    let mut s = sink();
    let b0 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 256).unwrap();
    let b2 = create::create_buffer(&mut d, &mut s, vk_buffer_usage::UNIFORM_BUFFER, 256).unwrap();
    let (ir0, ir2) = (buf_ir(&d, b0), buf_ir(&d, b2));
    // binding 0 = static storage; binding 2 = dynamic uniform (consumes one pDynamicOffset).
    let layout = d.create_descriptor_set_layout(vec![
        LayoutBinding {
            binding: 0,
            descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: 0,
        },
        LayoutBinding {
            binding: 2,
            descriptor_type: vk_descriptor_type::UNIFORM_BUFFER_DYNAMIC,
            descriptor_count: 1,
            stage_flags: 0,
        },
    ]);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, set, 0, b0, 0, 256).unwrap();
    create::update_descriptor_buffer(&mut d, set, 2, b2, 8, 64).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[set], &[100]).unwrap();
    let bg = last_bind_group(&s);
    use hl_gpu::protocol::model::descriptor::BindResource;
    for e in &bg.entries {
        let BindResource::Buffer { id, offset, .. } = &e.resource else {
            panic!("expected a buffer resource, got {:?}", e.resource);
        };
        match e.binding {
            0 => {
                assert_eq!(*id, ir0);
                assert_eq!(*offset, 0);
            }
            2 => {
                assert_eq!(*id, ir2);
                assert_eq!(*offset, 8 + 100);
            }
            b => panic!("unexpected binding {b}"),
        }
    }
    assert_eq!(bg.entries.len(), 2);
}

#[test]
fn multiple_sets_get_distinct_set_indices_from_first_set() {
    let mut d = dev();
    let mut s = sink();
    let ba = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
    let bb = create::create_buffer(&mut d, &mut s, vk_buffer_usage::STORAGE_BUFFER, 64).unwrap();
    let layout = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: 0,
        descriptor_type: vk_descriptor_type::STORAGE_BUFFER,
        descriptor_count: 1,
        stage_flags: 0,
    }]);
    let pool = d.create_descriptor_pool(2);
    let sa = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    let sb = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    create::update_descriptor_buffer(&mut d, sa, 0, ba, 0, 64).unwrap();
    create::update_descriptor_buffer(&mut d, sb, 0, bb, 0, 64).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    // first_set = 1 → the two sets land at set indices 1 and 2.
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 1, &[sa, sb], &[]).unwrap();
    let sets: Vec<u32> = s
        .commands()
        .filter_map(|c| match c {
            Cmd::CreateBindGroup(_, desc) => Some(desc.set),
            _ => None,
        })
        .collect();
    assert_eq!(sets, vec![1, 2]);
}

#[test]
fn separate_image_and_sampler_writes_compose_on_one_binding() {
    let mut d = dev();
    let mut s = sink();
    let img = create::create_image(
        &mut d,
        &mut s,
        4,
        4,
        vk_format::R8G8B8A8_UNORM,
        vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let samp = create::create_sampler(&mut d, &mut s, 1, 1, 1, [0, 0, 0], None);
    let img_ir = d.images.get(&img).unwrap().ir_id;
    let samp_ir = d.samplers.get(&samp).unwrap().ir_id;
    let layout = d.create_descriptor_set_layout(vec![LayoutBinding {
        binding: 3,
        descriptor_type: vk_descriptor_type::COMBINED_IMAGE_SAMPLER,
        descriptor_count: 1,
        stage_flags: 0,
    }]);
    let pool = d.create_descriptor_pool(1);
    let set = create::allocate_descriptor_set(&mut d, pool, layout, 0).unwrap();
    // Two separate writes to the SAME binding: image first, sampler later — must compose (both survive).
    create::update_descriptor_image(&mut d, set, 3, Some(img), None).unwrap();
    create::update_descriptor_image(&mut d, set, 3, None, Some(samp)).unwrap();

    let cb = d.allocate_command_buffer();
    d.begin_command_buffer(cb, false).unwrap();
    record::cmd_bind_descriptor_sets(&mut d, &mut s, cb, 0, &[set], &[]).unwrap();
    use hl_gpu::protocol::model::descriptor::BindResource;
    let bg = last_bind_group(&s);
    // Combined descriptor at binding 3: the image stays at binding 3, the sampler splits to binding 3 + 16
    // (the executor's `spirv_split` scheme — a combined image-sampler occupies two distinct bind-group slots).
    assert!(bg
        .entries
        .iter()
        .any(|e| e.binding == 3
            && matches!(e.resource, BindResource::Texture { id } if id == img_ir)));
    assert!(bg
        .entries
        .iter()
        .any(|e| e.binding == 19
            && matches!(e.resource, BindResource::Sampler { id } if id == samp_ir)));
}

// =====================================================================================================
// transfer / copy validation
// =====================================================================================================
