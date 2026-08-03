use super::harness::*;

// 2. SHADOW MAPPING — a depth-only pass rendering occluders feeding a pass that samples the depth map.
// ===================================================================================================

/// Pass 1 is a DEPTH-ONLY render pass (no color attachment, one depth attachment) rendering occluders;
/// pass 2's draw samples that depth texture as a shadow map. Asserts pass 1 lowers to `BeginRenderPass {
/// color: [], depth: Some(..) }` (the depth-only shape — NO color attachment) + the occluder draw, and
/// pass 2's bind group resolves the depth texture as a sampled shadow map.
#[test]
fn shadow_mapping_depth_only_pass_then_samples_depth_map() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // The shadow map: a D32 depth image, DEPTH_STENCIL_ATTACHMENT to render into + SAMPLED to read back.
    let shadow = create::create_image(
        &mut d,
        &mut sink,
        1024,
        1024,
        vk_format::D32_SFLOAT,
        vk_image_usage::DEPTH_STENCIL_ATTACHMENT | vk_image_usage::SAMPLED,
        1,
    )
    .unwrap();
    let shadow_ir = img_ir(&d, shadow);

    // Occluder pipeline: NO color targets, depth-write on (a depth-only pipeline). Main pipeline: color.
    let occluder = pipeline(
        &mut d,
        &mut sink,
        vec![],
        Some(DepthState::depth_only(
            TextureFormat::Depth32Float,
            true,
            compare::LESS,
        )),
    );
    let main = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm], None);
    let scene = sampled_color(&mut d, &mut sink, 800, 600);
    let scene_ir = img_ir(&d, scene);
    let sampler = create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0], None);

    // ---- Pass 1: depth-only shadow pass. No color attachment, one CLEAR depth attachment.
    let cb1 = d.allocate_command_buffer();
    d.begin_command_buffer(cb1, false).unwrap();
    record::cmd_begin_rendering(
        &mut d,
        cb1,
        &[], // <-- ZERO color attachments: the depth-only pass must NOT emit a color attachment.
        Some(RenderingDepthAttachment {
            image: shadow,
            clear_depth: 1.0,
            load_clear: true,
        }),
    )
    .unwrap();
    record::cmd_bind_pipeline(&mut d, cb1, occluder).unwrap();
    record::cmd_draw(&mut d, cb1, 24, 1, 0, 0).unwrap();
    d.end_render_pass(cb1).unwrap();
    d.end_command_buffer(cb1).unwrap();
    let enc1 = submit_encoder(&mut d, &mut sink, cb1);

    assert_eq!(
        enc1,
        vec![
            Enc::BeginRenderPass {
                color: vec![],
                depth: Some(DepthAttachment { texture: shadow_ir, depth_load: LoadOp::Clear,
            stencil_load: LoadOp::Clear, clear_depth: 1.0, clear_stencil: 0 }),
            },
            Enc::SetPipeline(d.pipelines.get(&occluder).unwrap().ir_id),
            Enc::Draw { vertex_count: 24, instance_count: 1, first_vertex: 0, first_instance: 0 },
            Enc::EndRenderPass,
        ],
        "shadow pass must lower to a depth-only BeginRenderPass (color: [], depth: Some) + the occluder draw"
    );

    // ---- Pass 2: sample the depth texture as a shadow map, then the main scene draw reads it.
    let cb2 = d.allocate_command_buffer();
    d.begin_command_buffer(cb2, false).unwrap();
    let entries = combined_sampler_set(&mut d, &mut sink, cb2, &[shadow], sampler);
    let s = samp_ir(&d, sampler);
    assert_eq!(
        entries,
        vec![
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 0,
                resource: BindResource::Texture { id: shadow_ir }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 16,
                resource: BindResource::Sampler { id: s }
            },
        ],
        "the main pass must bind the DEPTH texture as a sampled shadow map"
    );

    record::cmd_begin_rendering(
        &mut d,
        cb2,
        &[color_clear(scene, [0.1, 0.1, 0.1, 1.0])],
        None,
    )
    .unwrap();
    record::cmd_bind_pipeline(&mut d, cb2, main).unwrap();
    record::cmd_draw(&mut d, cb2, 3, 1, 0, 0).unwrap();
    d.end_render_pass(cb2).unwrap();
    d.end_command_buffer(cb2).unwrap();
    let enc2 = submit_encoder(&mut d, &mut sink, cb2);
    assert!(
        enc2.contains(&Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: scene_ir,
                load: LoadOp::Clear,
                clear: [0.1, 0.1, 0.1, 1.0],
                store: true
            }],
            depth: None,
        }),
        "the main pass renders into the color scene target: {enc2:?}"
    );
}

// ===================================================================================================
