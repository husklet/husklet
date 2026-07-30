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

fn asymmetric_textured_program(c: &mut GlContext) -> u32 {
    const FS: &str = "precision mediump float;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vec2(0.25, 0.75)); }\n";
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    prog
}

fn external_fbo_graph(external: bool) -> Vec<Cmd> {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);

    let source = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, source);
    record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let source_fbo = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, source_fbo);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        source,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let output = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, output);
    record::tex_image_2d_format(&mut context, 64, 64, &[], TextureFormat::Bgra8Unorm);
    if external {
        let generation = context.textures.get(output).expect("output texture").gen;
        context.bind_external_target(
            output,
            generation,
            hl_gpu::protocol::model::descriptor::SurfaceToken::new(19).unwrap(),
        );
    }
    let output_fbo = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, output_fbo);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        output,
        0,
    );
    asymmetric_textured_program(&mut context);
    record::uniform_sampler(&mut context, 0, 0);
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, source);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let mut frame = hl_gl::service::frame::Frame::build(&mut context).expect("frame graph");
    if external {
        frame
            .append_external_presents(|| hl_gpu::protocol::model::descriptor::FrameSerial::new(23))
            .unwrap();
    }
    frame.cmds
}

#[test]
fn imported_external_fbo_uses_window_coordinate_specialization() {
    let commands = external_fbo_graph(true);
    let present_texture = commands
        .iter()
        .find_map(|command| match command {
            Cmd::Present { texture, .. } => Some(*texture),
            _ => None,
        })
        .expect("external FBO presentation");
    let output_texture = commands
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor)
                if descriptor.label == "offscreen-fbo"
                    && descriptor.width == 64
                    && descriptor.height == 64 =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("external output texture");
    assert_eq!(present_texture, output_texture);

    let shaders = commands
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateShader { spirv, .. } => {
                hl_gpu::protocol::model::kernel::GlslDescriptor::from_words(spirv)
                    .and_then(Result::ok)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let vertex = shaders
        .iter()
        .find(|shader| {
            shader.stage == hl_gpu::protocol::model::kernel::glsl_stage::VERTEX
                && shader.source.contains("gl_Position.y = -gl_Position.y")
        })
        .expect("external-target vertex specialization");
    assert_eq!(
        vertex
            .source
            .matches("gl_Position.y = -gl_Position.y")
            .count(),
        1
    );
    let fragment = shaders
        .iter()
        .find(|shader| shader.source.contains("vec2(0.25, 0.75)"))
        .expect("external-target fragment shader");
    assert!(
        fragment.source.contains("1.0 -"),
        "presentation reflection must not cancel the render-target sampler conversion: {}",
        fragment.source
    );
    let pipeline = commands
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor)
                if descriptor.vertex.module
                    == commands
                        .iter()
                        .find_map(|command| match command {
                            Cmd::CreateShader { id, spirv, .. } => {
                                hl_gpu::protocol::model::kernel::GlslDescriptor::from_words(spirv)
                                    .and_then(Result::ok)
                                    .filter(|shader| shader.source == vertex.source)
                                    .map(|_| *id)
                            }
                            _ => None,
                        })
                        .unwrap() =>
            {
                Some(descriptor)
            }
            _ => None,
        })
        .next()
        .expect("external-target pipeline");
    assert_eq!(pipeline.front_face, 1);
}

#[test]
fn ordinary_non_external_fbo_retains_offscreen_coordinates() {
    let commands = external_fbo_graph(false);
    assert!(!commands
        .iter()
        .any(|command| matches!(command, Cmd::Present { .. })));
    let shaders = commands
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateShader { spirv, .. } => {
                hl_gpu::protocol::model::kernel::GlslDescriptor::from_words(spirv)
                    .and_then(Result::ok)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!shaders
        .iter()
        .any(|shader| shader.source.contains("gl_Position.y = -gl_Position.y")));
    let fragment = shaders
        .iter()
        .find(|shader| shader.source.contains("vec2(0.25, 0.75)"))
        .expect("ordinary offscreen fragment shader");
    assert!(
        fragment.source.contains("1.0 -"),
        "ordinary FBO sampling keeps its offscreen row conversion: {}",
        fragment.source
    );
    let pipelines = commands
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(pipelines.iter().all(|pipeline| pipeline.front_face == 0));
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
    let fbo = c.gen_framebuffer();
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
    assert!(c.delete_texture(atlas));

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // One ordered render pass per framebuffer run. They may share a protocol Submit, but the frame is not
    // collapsed onto the first draw's FBO.
    let passes = batch
        .iter()
        .filter_map(|command| match command {
            Cmd::Submit(buffer) => Some(buffer.encoder.iter()),
            _ => None,
        })
        .flatten()
        .filter(|operation| matches!(operation, Enc::BeginRenderPass { .. }))
        .count();
    assert_eq!(
        passes, 2,
        "a render pass per framebuffer (offscreen atlas, then default window)"
    );

    // The offscreen target is sized to the 16x16 attachment and carries SAMPLED (the composite reads it).
    let (atlas_rt, ad) = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateTexture(id, d) if d.label == "gl-retired-fbo" => Some((*id, d)),
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
        "the composite pass retains and samples the offscreen render target after its GL name is deleted"
    );
}

#[test]
fn framebuffer_render_supersedes_an_earlier_cpu_upload_across_frames() {
    use hl_gpu::protocol::model::descriptor::BindResource;

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let atlas = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(
        &mut context,
        16,
        16,
        &[0x11; 16 * 16 * 4],
        TextureFormat::R8Unorm,
    );
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let rendered = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "offscreen-fbo" => Some(*id),
            _ => None,
        })
        .expect("persistent R8 framebuffer target");

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    textured_program(&mut context);
    record::uniform_sampler(&mut context, 0, 0);
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());

    let sampled = sink.batches[1]
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateBindGroup(_, descriptor) => Some(&descriptor.entries),
            _ => None,
        })
        .flatten()
        .any(|entry| matches!(entry.resource, BindResource::Texture { id } if id == rendered));
    assert!(
        sampled,
        "the accepted framebuffer write is newer than the texture's stale CPU shadow"
    );
    let fragment_source = sink.batches[1]
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateShader {
                kind: hl_gpu::ShaderPayloadKind::Glsl,
                spirv,
                ..
            } => hl_gpu::protocol::model::kernel::GlslDescriptor::from_words(spirv)
                .and_then(Result::ok)
                .map(|descriptor| descriptor.source),
            _ => None,
        })
        .find(|source| source.contains("texture2D") && !source.contains("gl_Position"))
        .expect("ES2 fragment shader");
    assert!(
        fragment_source.contains("1.0 -"),
        "the present-target clip reflection retains the rendered-texture sampler conversion: {fragment_source}"
    );
}

#[test]
fn r8_framebuffer_glyph_coverage_is_swizzled_into_rgba_in_a_later_frame() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let atlas = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(&mut context, 2, 2, &[], TextureFormat::R8Unorm);
    record::tex_parameter_vector(
        &mut context,
        GL_TEXTURE_SWIZZLE_RGBA,
        &[GL_ONE, GL_ONE, GL_ONE, GL_RED],
    );

    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::flush(&mut context, &mut sink).unwrap());

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    textured_program(&mut context);
    record::uniform_sampler(&mut context, 0, 0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    // Deferred lowering must use the state captured by this draw, not a later mutation of the object.
    record::tex_parameter_vector(
        &mut context,
        GL_TEXTURE_SWIZZLE_RGBA,
        &[GL_RED, GL_GREEN, GL_BLUE, GL_ALPHA],
    );
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());

    let fragment_source = sink.batches[1]
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateShader {
                kind: hl_gpu::ShaderPayloadKind::Glsl,
                spirv,
                ..
            } => hl_gpu::protocol::model::kernel::GlslDescriptor::from_words(spirv)
                .and_then(Result::ok)
                .map(|descriptor| descriptor.source),
            _ => None,
        })
        .find(|source| source.contains("texture2D") && !source.contains("gl_Position"))
        .expect("later-frame fragment shader");
    assert!(
        fragment_source.contains("hl_swizzle_"),
        "R8 glyph coverage must apply the texture object's RGBA swizzle: {fragment_source}"
    );
    assert!(
        fragment_source.contains("vec4(1.0, 1.0, 1.0, value.r)"),
        "Chrome's (ONE, ONE, ONE, RED) swizzle maps an exact R8 coverage pixel \
         [64, 0, 0, 255] to [255, 255, 255, 64]: {fragment_source}"
    );
}

#[test]
fn cpu_upload_supersedes_an_earlier_framebuffer_render() {
    use hl_gpu::protocol::model::descriptor::BindResource;

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let atlas = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::R8Unorm);
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let old_render_target = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "offscreen-fbo" => Some(*id),
            _ => None,
        })
        .expect("old framebuffer target");

    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::tex_sub_image_2d(
        &mut context,
        GL_TEXTURE_2D,
        0,
        0,
        0,
        16,
        16,
        &[0x77; 16 * 16 * 4],
    );
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    textured_program(&mut context);
    record::uniform_sampler(&mut context, 0, 0);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());

    let sampled: Vec<_> = sink.batches[1]
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateBindGroup(_, descriptor) => Some(&descriptor.entries),
            _ => None,
        })
        .flatten()
        .filter_map(|entry| match entry.resource {
            BindResource::Texture { id } => Some(id),
            _ => None,
        })
        .collect();
    assert!(!sampled.contains(&old_render_target));
    assert!(
        sink.batches[1]
            .iter()
            .any(|command| matches!(command, Cmd::CreateTexture(_, descriptor) if descriptor.usage & texture_usage::COPY_DST != 0)),
        "the later CPU upload creates fresh sampled residency"
    );
}

#[test]
fn offscreen_blit_after_window_pass_does_not_replace_present_target() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let source_texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, source_texture);
    record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let source = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, source);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        source_texture,
        0,
    );
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let destination_texture = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, destination_texture);
    record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let destination = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_DRAW_FRAMEBUFFER, destination);
    record::framebuffer_texture_2d(
        &mut context,
        GL_DRAW_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        destination_texture,
        0,
    );
    record::bind_framebuffer(&mut context, GL_READ_FRAMEBUFFER, source);
    record::blit_framebuffer(
        &mut context,
        0,
        0,
        16,
        16,
        0,
        0,
        16,
        16,
        GL_COLOR_BUFFER_BIT,
        GL_NEAREST,
    );

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let (window_texture, _) = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "default-fbo" => {
                Some((*id, descriptor))
            }
            _ => None,
        })
        .expect("default window target");
    match batch.last().expect("present command") {
        Cmd::Present { texture, .. } => assert_eq!(
            *texture, window_texture,
            "an offscreen blit after window rendering must not replace the presented window target"
        ),
        command => panic!("expected window Present, got {command:?}"),
    }
}

#[test]
fn flush_executes_pending_work_and_swap_presents_the_completed_target() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    // An offscreen FBO with a color texture; render one triangle INTO it (fbo != 0).
    let atlas = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(&mut c, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let fbo = c.gen_framebuffer();
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

    // A window draw (default framebuffer, fbo == 0) belongs to the later swap.
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 0);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.draws().len(), 2, "two draws recorded before flush");

    // glFlush executes the offscreen work without consuming the window draw.
    assert!(
        swap::flush(&mut c, &mut sink).unwrap(),
        "pending work was flushed"
    );
    assert_eq!(
        c.draws().len(),
        1,
        "glFlush retains the window draw for eglSwapBuffers"
    );
    let flush_batch = &sink.batches[0];
    assert!(
        flush_batch.iter().any(|c| matches!(c, Cmd::Submit(_))),
        "the offscreen pass is submitted"
    );
    assert!(
        !flush_batch.iter().any(|c| matches!(c, Cmd::Present { .. })),
        "the offscreen flush presents nothing (no window swap)"
    );
    let offscreen_vertex = flush_batch
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateShader { spirv, .. } => {
                hl_gpu::protocol::model::kernel::GlslDescriptor::from_words(spirv)
                    .and_then(Result::ok)
                    .filter(|shader| {
                        shader.stage == hl_gpu::protocol::model::kernel::glsl_stage::VERTEX
                    })
            }
            _ => None,
        })
        .next()
        .expect("offscreen vertex shader");
    assert!(
        !offscreen_vertex
            .source
            .contains("gl_Position.y = -gl_Position.y"),
        "FBO rendering retains GL texture orientation"
    );

    // The retained window draw is executed and presented at swap.
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(
        sink.batches[1]
            .iter()
            .any(|c| matches!(c, Cmd::Present { .. })),
        "the swap presents the window"
    );
    // The default window target is an INTERNAL render target: it stores rows top-down like every other
    // internal target, so no clip reflection is emitted for it (see `RenderPasses::stores_bottom_up_rows`).
    // Reflecting it mirrored every presented frame; only an imported external image keeps the reflection
    // (pinned in `render::order`). The window pass therefore needs no separate shader VARIANT either — the
    // program's offscreen modules are reused verbatim.
    assert!(
        !sink.batches[1]
            .iter()
            .filter_map(|command| match command {
                Cmd::CreateShader { spirv, .. } =>
                    hl_gpu::protocol::model::kernel::GlslDescriptor::from_words(spirv)
                        .and_then(Result::ok),
                _ => None,
            })
            .any(|shader| shader.source.contains("gl_Position.y = -gl_Position.y")),
        "the default window target keeps GL clip space unreflected"
    );
    let present_pipeline = sink.batches[1]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .expect("present pipeline");
    assert_eq!(
        present_pipeline.front_face, 0,
        "with no clip reflection the declared GL winding is preserved"
    );

    // With nothing offscreen pending, a second flush is a no-op (returns false, submits nothing).
    let batches_before = sink.batches.len();
    assert!(!swap::flush(&mut c, &mut sink).unwrap());
    assert_eq!(
        sink.batches.len(),
        batches_before,
        "an empty flush submits no batch"
    );
}

#[test]
fn partial_flush_defers_destroy_pinned_by_a_retained_window_draw() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    let sampled = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, sampled);
    record::tex_image_2d(&mut context, 2, 2, &[0xA5; 16]);
    textured_program(&mut context);
    record::uniform_sampler(&mut context, 0, 0);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let sampled_ir = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor)
                if descriptor.usage & texture_usage::SAMPLED != 0
                    && descriptor.width == 2
                    && descriptor.height == 2 =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("sampled texture becomes resident");

    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    let offscreen = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, offscreen);
    record::tex_image_2d_format(&mut context, 8, 8, &[], TextureFormat::Rgba8Unorm);
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        offscreen,
        0,
    );
    flat_program(&mut context);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(sampled));

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    assert!(!sink.batches[1]
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == sampled_ir)));

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert!(sink.batches[2]
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == sampled_ir)));
    assert!(sink.batches[2].iter().any(|command| match command {
        Cmd::CreateBindGroup(_, descriptor) => descriptor.entries.iter().any(|entry| matches!(
            entry.resource,
            hl_gpu::protocol::model::descriptor::BindResource::Texture { id }
                if id == sampled_ir
        ),),
        _ => false,
    }));
}

#[test]
fn partial_flush_promotes_a_deleted_producer_target_for_the_retained_consumer() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let atlas = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(
        &mut context,
        8,
        8,
        &[0x11; 8 * 8 * 4],
        TextureFormat::Rgba8Unorm,
    );
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    textured_program(&mut context);
    record::uniform_sampler(&mut context, 0, 0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(atlas));

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let target = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "gl-retired-fbo" => Some(*id),
            _ => None,
        })
        .expect("offscreen producer target");
    assert!(!sink.batches[0]
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == target)));

    let batches = sink.batches.len();
    assert_eq!(
        context.flush_retirements(&mut sink).unwrap(),
        0,
        "capture cleanup must not retire a producer still pinned by the retained window draw"
    );
    assert_eq!(
        sink.batches.len(),
        batches,
        "a fully pinned retirement queue submits no standalone cleanup"
    );
    assert!(
        context
            .pending_destroys()
            .iter()
            .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == target)),
        "the target remains queued for the consumer frame's tail"
    );

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert!(sink.batches[1].iter().any(|command| match command {
        Cmd::CreateBindGroup(_, descriptor) => descriptor.entries.iter().any(|entry| matches!(
            entry.resource,
            hl_gpu::protocol::model::descriptor::BindResource::Texture { id }
                if id == target
        ),),
        _ => false,
    }));
    assert!(sink.batches[1]
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == target)));
}

#[test]
fn partial_flush_keeps_a_deleted_producer_alive_for_a_cross_boundary_blit() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let source = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, source);
    record::tex_image_2d_format(
        &mut context,
        8,
        8,
        &[0x33; 8 * 8 * 4],
        TextureFormat::Rgba8Unorm,
    );
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        source,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    record::bind_framebuffer(&mut context, GL_READ_FRAMEBUFFER, framebuffer);
    record::bind_framebuffer(&mut context, GL_DRAW_FRAMEBUFFER, 0);
    record::blit_framebuffer(
        &mut context,
        0,
        0,
        8,
        8,
        0,
        0,
        8,
        8,
        GL_COLOR_BUFFER_BIT,
        GL_NEAREST,
    );
    assert!(context.delete_texture(source));

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let target = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "gl-retired-fbo" => Some(*id),
            _ => None,
        })
        .expect("accepted producer target");
    context.queue_buffer_destroy(41);

    assert_eq!(
        context.flush_retirements(&mut sink).unwrap(),
        1,
        "unrelated cleanup remains reclaimable while the blit source is pinned"
    );
    assert_eq!(sink.batches[1], vec![Cmd::DestroyBuffer(41)]);
    assert!(context
        .pending_destroys()
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == target)));

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert!(sink.batches[2].iter().any(|command| matches!(
        command,
        Cmd::Submit(command_buffer)
            if command_buffer.encoder.iter().any(|operation| matches!(
                operation,
                Enc::CopyTextureToTexture { src, .. } | Enc::BlitTexture { src, .. }
                    if *src == target
            ))
    )));
    assert!(sink.batches[2]
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == target)));
}

#[test]
fn partial_flush_replaces_an_old_resident_target_with_the_new_deleted_producer() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let atlas = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(
        &mut context,
        8,
        8,
        &[0x22; 8 * 8 * 4],
        TextureFormat::Rgba8Unorm,
    );
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let old = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "offscreen-fbo" => Some(*id),
            _ => None,
        })
        .expect("old resident target");

    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    textured_program(&mut context);
    record::uniform_sampler(&mut context, 0, 0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(atlas));

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let new = sink.batches[1]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "gl-retired-fbo" => Some(*id),
            _ => None,
        })
        .expect("new frame-owned producer target");
    assert_ne!(old, new);
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let bound = sink.batches[2]
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateBindGroup(_, descriptor) => Some(&descriptor.entries),
            _ => None,
        })
        .flatten()
        .any(|entry| {
            matches!(
                entry.resource,
                hl_gpu::protocol::model::descriptor::BindResource::Texture { id } if id == new
            )
        });
    assert!(bound, "retained consumer must use the new producer target");
}

#[test]
fn partial_flush_does_not_retire_a_live_cached_producer_target() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let atlas = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(&mut context, 8, 8, &[], TextureFormat::Rgba8Unorm);
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    textured_program(&mut context);
    record::uniform_sampler(&mut context, 0, 0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, atlas);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let target = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "offscreen-fbo" => Some(*id),
            _ => None,
        })
        .expect("cached producer target");
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    assert!(!sink.batches[1]
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == target)));
}

#[test]
fn deleting_an_unlowered_fbo_target_retires_the_deferred_target_after_submit() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, texture);
    record::tex_image_2d_format(&mut context, 8, 8, &[], TextureFormat::Rgba8Unorm);
    let generation = context.textures.get(texture).unwrap().gen;
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        texture,
        0,
    );
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_texture(texture));

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let retired = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "gl-retired-fbo" => Some(*id),
            _ => None,
        })
        .expect("deleted deferred FBO gets a frame-local target");
    assert!(sink.batches[0]
        .iter()
        .any(|command| matches!(command, Cmd::DestroyTexture(id) if *id == retired)));
    assert_eq!(context.resident_fbo_target_tex(texture, generation), None);
}

#[test]
fn flush_executes_default_framebuffer_for_non_window_surfaces() {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert!(
        swap::flush(&mut context, &mut sink).unwrap(),
        "a pbuffer or surfaceless default framebuffer must execute at glFlush"
    );
    assert!(context.draws().is_empty());
    assert!(sink.batches[0]
        .iter()
        .any(|command| matches!(command, Cmd::Submit(_))));
    assert!(!sink.batches[0]
        .iter()
        .any(|command| matches!(command, Cmd::Present { .. })));
}

#[test]
fn window_readback_and_present_share_one_submission() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let pixels = hl_gl::service::readpixels::swap_xrgb(&mut context, &mut sink, 64, 64).unwrap();

    let pixels = pixels.unwrap();
    assert_eq!(pixels.len(), 64 * 64 * 4);
    assert!(pixels.chunks_exact(4).all(|pixel| pixel[3] == 0xff));
    assert_eq!(
        sink.batches.len(),
        1,
        "render, readback copy, and present must use one command batch"
    );
    assert_eq!(sink.reads.len(), 1);
    assert!(sink.batches[0]
        .iter()
        .any(|command| matches!(command, Cmd::Present { .. })));
    assert!(context.draws().is_empty());
}

#[test]
fn surfaceless_context_flushes_user_framebuffer_work() {
    let mut context = ctx_64();
    context.set_surface_available(false);
    let mut sink = RecordingSink::with_full_caps();
    let color = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, color);
    record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        color,
        0,
    );
    record::clear(&mut context);
    record::bind_framebuffer(&mut context, GL_DRAW_FRAMEBUFFER, 0);
    assert_eq!(context.bound_framebuffer(), 0);
    assert_eq!(context.read_framebuffer(), framebuffer);

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    assert!(sink.batches[0]
        .iter()
        .any(|command| matches!(command, Cmd::Submit(_))));
}

#[test]
fn read_pixels_reads_an_offscreen_target_after_flush() {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    let mut sink = RecordingSink::with_full_caps();
    let color = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, color);
    record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        color,
        0,
    );
    record::clear_color(
        &mut context,
        [17.0 / 255.0, 34.0 / 255.0, 51.0 / 255.0, 1.0],
    );
    record::clear(&mut context);

    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let resident = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateTexture(id, descriptor) if descriptor.label == "offscreen-fbo" => Some(*id),
            _ => None,
        })
        .expect("flush materializes a persistent offscreen target");

    let pixels =
        hl_gl::service::readpixels::read_pixels(&mut context, &mut sink, 0, 0, 1, 1, GL_RGBA)
            .unwrap();

    assert_eq!(pixels.len(), 4);
    assert_eq!(sink.batches.len(), 2, "readback submits after the flush");
    assert!(sink.batches[1].iter().any(|command| {
        matches!(
            command,
            Cmd::Submit(command_buffer)
                if command_buffer.encoder.iter().any(
                    |command| matches!(command, Enc::CopyTextureToBuffer { src, .. } if *src == resident)
                )
        )
    }));
}
