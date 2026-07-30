use super::*;

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
        bias_constant: 0,
        bias_slope_scale: 0.0,
        bias_clamp: 0.0,
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
