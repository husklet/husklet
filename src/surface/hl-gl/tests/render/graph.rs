use super::*;

// ---------------------------------------------------------------------------------------------------
// multi-FBO frame graph: render into an offscreen FBO, then sample it from the default framebuffer
// ---------------------------------------------------------------------------------------------------

const FS_TEX: &str = "precision mediump float;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vec2(0.5, 0.5)); }\n";

/// Link + bind a program that samples `uTex` (one sampler, no data uniforms) and return its GL name.
fn textured_program(c: &mut GlContext) -> u32 {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS_TEX);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    prog
}

#[test]
fn multi_fbo_frame_lowers_a_pass_per_framebuffer_and_presents_the_window() {
    use hl_gpu::protocol::model::descriptor::BindResource;
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    // A 16x16 offscreen atlas texture attached to FBO A (the GskGL glyph-atlas shape, tiny vs the window).
    let atlas = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(&mut c, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let fbo = c.framebuffers.gen();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );

    // Pass 1 — render geometry INTO the offscreen atlas.
    flat_program(&mut c);
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // Pass 2 — bind the DEFAULT framebuffer and draw a quad that SAMPLES the atlas (the composite).
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 0);
    let comp = textured_program(&mut c);
    record::use_program(&mut c, comp);
    record::uniform_sampler(&mut c, 0, 0); // uTex -> texture unit 0
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, atlas); // sample the FBO's color attachment
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // One render-pass Submit per framebuffer run — the frame is NOT collapsed onto the first draw's FBO.
    let submits = batch.iter().filter(|c| matches!(c, Cmd::Submit(_))).count();
    assert_eq!(
        submits, 2,
        "a render pass per framebuffer (offscreen atlas, then default window)"
    );

    // The offscreen target is sized to the 16x16 attachment and carries SAMPLED (the composite reads it).
    let (atlas_rt, ad) = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateTexture(id, d) if d.label == "offscreen-fbo" => Some((*id, d)),
            _ => None,
        })
        .expect("an offscreen render target");
    assert_eq!(
        (ad.width, ad.height),
        (16, 16),
        "offscreen target sized to its attachment"
    );
    assert!(
        ad.usage & texture_usage::SAMPLED != 0,
        "offscreen target is SAMPLED by the composite pass"
    );

    // The DEFAULT target is the WINDOW size and is what the frame presents (not the 16x16 atlas).
    let (def_rt, dd) = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateTexture(id, d) if d.label == "default-fbo" => Some((*id, d)),
            _ => None,
        })
        .expect("a default window render target");
    assert_eq!(
        (dd.width, dd.height),
        (64, 64),
        "default target is the window size"
    );
    match batch.last().unwrap() {
        Cmd::Present { texture, .. } => {
            assert_eq!(
                *texture, def_rt,
                "presents the WINDOW target, not the offscreen atlas"
            );
            assert_ne!(*texture, atlas_rt);
        }
        other => panic!("expected Present of the window target, got {other:?}"),
    }

    // Cross-pass sampling: the composite's bind group references the offscreen RENDER-TARGET texture id
    // (the rendered atlas), NOT a fresh upload of the attachment's CPU storage.
    let samples_atlas = batch
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateBindGroup(_, descriptor) => Some(&descriptor.entries),
            _ => None,
        })
        .flatten()
        .any(|entry| matches!(&entry.resource, BindResource::Texture { id } if *id == atlas_rt));
    assert!(
        samples_atlas,
        "the composite pass samples the offscreen render-target texture (cross-pass)"
    );
}

#[test]
fn flush_offscreen_submits_offscreen_passes_and_retains_window_draws() {
    // glFlush / glFinish drain the accumulated OFFSCREEN passes now (no Present) while retaining the
    // window (default-framebuffer) draws for the eventual eglSwapBuffers — keeping the swap frame bounded.
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    // An offscreen FBO with a color texture; render one triangle INTO it (fbo != 0).
    let atlas = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(&mut c, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let fbo = c.framebuffers.gen();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );
    flat_program(&mut c);
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // A window draw (default framebuffer, fbo == 0) that must be RETAINED for the swap.
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 0);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.draws.len(), 2, "two draws recorded before flush");

    // glFlush: drains the ONE offscreen pass (a Submit with NO Present), retains the window draw.
    assert!(
        swap::flush_offscreen(&mut c, &mut sink).unwrap(),
        "offscreen work was flushed"
    );
    assert_eq!(c.draws.len(), 1, "the window draw is retained for the swap");
    let flush_batch = &sink.batches[0];
    assert!(
        flush_batch.iter().any(|c| matches!(c, Cmd::Submit(_))),
        "the offscreen pass is submitted"
    );
    assert!(
        !flush_batch.iter().any(|c| matches!(c, Cmd::Present { .. })),
        "the offscreen flush presents nothing (no window swap)"
    );

    // The retained window draw still presents at swap.
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(
        sink.batches[1]
            .iter()
            .any(|c| matches!(c, Cmd::Present { .. })),
        "the swap presents the window"
    );

    // With nothing offscreen pending, a second flush is a no-op (returns false, submits nothing).
    let batches_before = sink.batches.len();
    assert!(!swap::flush_offscreen(&mut c, &mut sink).unwrap());
    assert_eq!(
        sink.batches.len(),
        batches_before,
        "an empty flush submits no batch"
    );
}
