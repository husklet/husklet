use super::harness::*;

// 1. DEFERRED SHADING — an MRT G-buffer pass feeding a fullscreen lighting pass that samples it.
// ===================================================================================================

/// Pass 1 writes a 3-attachment G-buffer (albedo/normal/position) with a geometry draw; pass 2's fullscreen
/// lighting draw SAMPLES all three G-buffer textures. Asserts pass 1 lowers to a `BeginRenderPass` with 3
/// color attachments + the geometry draw, and pass 2 binds the 3 textures (+ their samplers) and draws.
#[test]
fn deferred_shading_mrt_gbuffer_then_lighting_samples_three_targets() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    // The three G-buffer render targets, each also SAMPLED so the lighting pass can read them back.
    let albedo = sampled_color(&mut d, &mut sink, 128, 128);
    let normal = sampled_color(&mut d, &mut sink, 128, 128);
    let position = sampled_color(&mut d, &mut sink, 128, 128);
    let (a_ir, n_ir, p_ir) = (img_ir(&d, albedo), img_ir(&d, normal), img_ir(&d, position));

    // Geometry pipeline: 3 color targets (MRT). Lighting pipeline: 1 color target + no depth.
    let geo = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm; 3], None);
    let light = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm], None);
    let out = sampled_color(&mut d, &mut sink, 128, 128);
    let out_ir = img_ir(&d, out);
    let sampler = create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0], None);

    // ---- Pass 1: fill the G-buffer with a geometry draw (MRT: 3 color attachments, all CLEAR+STORE).
    let cb1 = d.allocate_command_buffer();
    d.begin_command_buffer(cb1, false).unwrap();
    record::cmd_begin_rendering(
        &mut d,
        cb1,
        &[
            color_clear(albedo, [0.0; 4]),
            color_clear(normal, [0.0; 4]),
            color_clear(position, [0.0; 4]),
        ],
        None,
    )
    .unwrap();
    record::cmd_bind_pipeline(&mut d, cb1, geo).unwrap();
    record::cmd_draw(&mut d, cb1, 36, 1, 0, 0).unwrap();
    d.end_render_pass(cb1).unwrap();
    d.end_command_buffer(cb1).unwrap();
    let enc1 = submit_encoder(&mut d, &mut sink, cb1);

    assert_eq!(
        enc1,
        vec![
            Enc::BeginRenderPass {
                color: vec![
                    ColorAttachment {
                        texture: a_ir,
                        load: LoadOp::Clear,
                        clear: [0.0; 4],
                        store: true
                    },
                    ColorAttachment {
                        texture: n_ir,
                        load: LoadOp::Clear,
                        clear: [0.0; 4],
                        store: true
                    },
                    ColorAttachment {
                        texture: p_ir,
                        load: LoadOp::Clear,
                        clear: [0.0; 4],
                        store: true
                    },
                ],
                depth: None,
            },
            Enc::SetPipeline(d.pipelines.get(&geo).unwrap().ir_id),
            Enc::Draw {
                vertex_count: 36,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0
            },
            Enc::EndRenderPass,
        ],
        "G-buffer pass 1 must lower to a 3-color-attachment BeginRenderPass + the geometry draw"
    );

    // ---- Pass 2: bind the 3 G-buffer textures as samplers, then a fullscreen lighting draw reads them.
    let cb2 = d.allocate_command_buffer();
    d.begin_command_buffer(cb2, false).unwrap();
    let entries =
        combined_sampler_set(&mut d, &mut sink, cb2, &[albedo, normal, position], sampler);

    // The lighting pass samples EXACTLY the three G-buffer textures: a Texture at each binding B + its
    // Sampler at B + 16 (the combined-image-sampler split the wgpu executor performs).
    let s = samp_ir(&d, sampler);
    assert_eq!(
        entries,
        vec![
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 0,
                resource: BindResource::Texture { id: a_ir }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 1,
                resource: BindResource::Texture { id: n_ir }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 2,
                resource: BindResource::Texture { id: p_ir }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 16,
                resource: BindResource::Sampler { id: s }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 17,
                resource: BindResource::Sampler { id: s }
            },
            hl_gpu::protocol::model::descriptor::BindEntry {
                binding: 18,
                resource: BindResource::Sampler { id: s }
            },
        ],
        "lighting pass must sample all three G-buffer textures (image at B, sampler at B+16)"
    );

    record::cmd_begin_rendering(&mut d, cb2, &[color_clear(out, [0.0; 4])], None).unwrap();
    record::cmd_bind_pipeline(&mut d, cb2, light).unwrap();
    record::cmd_draw(&mut d, cb2, 3, 1, 0, 0).unwrap();
    d.end_render_pass(cb2).unwrap();
    d.end_command_buffer(cb2).unwrap();
    let enc2 = submit_encoder(&mut d, &mut sink, cb2);

    // The lighting pass targets the output and replays the sampled bind group into the fullscreen draw.
    let light_ir = d.pipelines.get(&light).unwrap().ir_id;
    let bind_group = match enc2.iter().find_map(|e| match e {
        Enc::SetBindGroup { index: 0, group } => Some(*group),
        _ => None,
    }) {
        Some(g) => g,
        None => panic!("lighting draw did not replay the G-buffer bind group: {enc2:?}"),
    };
    assert_eq!(
        enc2,
        vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: out_ir,
                    load: LoadOp::Clear,
                    clear: [0.0; 4],
                    store: true
                }],
                depth: None,
            },
            Enc::SetPipeline(light_ir),
            Enc::SetBindGroup {
                index: 0,
                group: bind_group
            },
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0
            },
            Enc::EndRenderPass,
        ],
        "lighting pass must render into the output with the G-buffer bound + a fullscreen draw"
    );
}

// ===================================================================================================
