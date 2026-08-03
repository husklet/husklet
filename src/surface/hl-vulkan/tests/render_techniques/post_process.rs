use super::harness::*;

// 3. POST-PROCESS CHAIN — ping-pong: render A, sample A→B, sample B→C.
// ===================================================================================================

/// Render a scene into texture A, then a pass samples A and renders into B, then a pass samples B and
/// renders into C. Asserts the ping-pong: each pass's sampler binding names the PREVIOUS stage's texture,
/// and each pass's `BeginRenderPass` targets the NEXT texture (no aliasing, no stale binding).
#[test]
fn post_process_chain_ping_pongs_sampler_and_target() {
    let mut d = dev();
    let mut sink = RecordingSink::with_full_caps();

    let a = sampled_color(&mut d, &mut sink, 256, 256);
    let b = sampled_color(&mut d, &mut sink, 256, 256);
    let c = sampled_color(&mut d, &mut sink, 256, 256);
    let (a_ir, b_ir, c_ir) = (img_ir(&d, a), img_ir(&d, b), img_ir(&d, c));
    let scene = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm], None);
    let blur = pipeline(&mut d, &mut sink, vec![TextureFormat::Rgba8Unorm], None);
    let sampler = create::create_sampler(&mut d, &mut sink, 1, 1, 1, [0, 0, 0], None);

    // ---- Stage 0: render the scene into A.
    let cb0 = d.allocate_command_buffer();
    d.begin_command_buffer(cb0, false).unwrap();
    record::cmd_begin_rendering(&mut d, cb0, &[color_clear(a, [0.0; 4])], None).unwrap();
    record::cmd_bind_pipeline(&mut d, cb0, scene).unwrap();
    record::cmd_draw(&mut d, cb0, 3, 1, 0, 0).unwrap();
    d.end_render_pass(cb0).unwrap();
    d.end_command_buffer(cb0).unwrap();
    let enc0 = submit_encoder(&mut d, &mut sink, cb0);
    assert_eq!(
        enc0[0],
        Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture: a_ir,
                load: LoadOp::Clear,
                clear: [0.0; 4],
                store: true
            }],
            depth: None,
        }
    );

    // A closure for one ping-pong stage: SAMPLE `src`, render into `dst`. Returns (sampled ir, target ir).
    let stage = |d: &mut Device, sink: &mut RecordingSink, src: u64, dst: u64| -> (u32, u32) {
        let cb = d.allocate_command_buffer();
        d.begin_command_buffer(cb, false).unwrap();
        let entries = combined_sampler_set(d, sink, cb, &[src], sampler);
        // The stage samples EXACTLY the previous stage's texture.
        let sampled = match entries[0].resource {
            BindResource::Texture { id } => id,
            ref other => panic!("expected a sampled Texture, got {other:?}"),
        };
        record::cmd_begin_rendering(d, cb, &[color_clear(dst, [0.0; 4])], None).unwrap();
        record::cmd_bind_pipeline(d, cb, blur).unwrap();
        record::cmd_draw(d, cb, 3, 1, 0, 0).unwrap();
        d.end_render_pass(cb).unwrap();
        d.end_command_buffer(cb).unwrap();
        let enc = submit_encoder(d, sink, cb);
        let target = match &enc[0] {
            Enc::BeginRenderPass { color, .. } => color[0].texture,
            other => panic!("expected BeginRenderPass, got {other:?}"),
        };
        (sampled, target)
    };

    // ---- Stage 1: sample A → render B. ---- Stage 2: sample B → render C.
    let (s1, t1) = stage(&mut d, &mut sink, a, b);
    let (s2, t2) = stage(&mut d, &mut sink, b, c);

    assert_eq!((s1, t1), (a_ir, b_ir), "stage 1 samples A and targets B");
    assert_eq!(
        (s2, t2),
        (b_ir, c_ir),
        "stage 2 samples B and targets C (ping-pong advanced, no aliasing)"
    );
    assert_ne!(
        s2, t2,
        "a ping-pong stage never samples its own render target"
    );
}

// ===================================================================================================
