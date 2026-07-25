use super::*;

#[test]
fn textured_quad_encoder_sequence_and_present() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let ops = submit_ops(batch);

    // Expected ordered encoder shape: stage copy, then the render pass.
    assert!(matches!(
        ops[0],
        Enc::CopyBufferToTexture {
            width: 2,
            height: 2,
            ..
        }
    ));
    assert!(matches!(ops[1], Enc::BeginRenderPass { .. }));
    assert!(matches!(ops[2], Enc::SetPipeline(_)));
    assert!(matches!(ops[3], Enc::SetViewport { .. }));
    assert!(matches!(ops[4], Enc::SetScissor { .. }));
    assert!(matches!(ops[5], Enc::SetBindGroup { index: 0, .. }));
    assert!(matches!(ops[6], Enc::SetVertexBuffer { slot: 0, .. }));
    match &ops[7] {
        Enc::Draw {
            vertex_count,
            instance_count,
            first_vertex,
            ..
        } => {
            assert_eq!(*vertex_count, 6);
            assert_eq!(*instance_count, 1);
            assert_eq!(*first_vertex, 0);
        }
        other => panic!("expected Draw, got {other:?}"),
    }
    assert!(matches!(ops[8], Enc::EndRenderPass));

    // the frame ends with a Present of the rendered default target.
    assert!(matches!(batch.last().unwrap(), Cmd::Present { .. }));
}

#[test]
fn textured_quad_bind_group_binds_texture_and_sampler() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let batch = &sink.batches[0];

    let bg = batch.iter().find_map(|c| match c {
        Cmd::CreateBindGroup(_, d) => Some(d),
        _ => None,
    });
    let bg = bg.expect("CreateBindGroup");
    assert!(bg
        .entries
        .iter()
        .any(|e| matches!(e.resource, BindResource::Texture { .. })));
    assert!(bg
        .entries
        .iter()
        .any(|e| matches!(e.resource, BindResource::Sampler { .. })));
}

// GskGpu forwards its GLSL-ES VERBATIM to the host executor's ES route, which numbers samplers by a
// running `layout(binding=)` counter over EVERY host-recognized `uniform <samplerType> NAME;` in fragment
// text order — INCLUDING the inactive `samplerExternalOES` declarations of GskGpu's
// `#ifdef …_IS_EXTERNAL / #else` texture blocks. So the ACTIVE `sampler2D GSK_TEXTURE0` lands on host
// binding k=1 (texture 3 / sampler 4), not k=0 — the external `GSK_TEXTURE0` before it consumed k=0. The
// driver's own reflection skips the external decls and dedups, so its declaration index (0) differs from
// the host's k (1). The lowering must bind the texture at the HOST binding the compiled shader declares,
// or the bind group NACKs against the auto layout ("bindings … (1) does not match … (3)"). This asserts
// the GSK_TEXTURE0 texture/sampler land at binding 3/4, and NOT at the naive declaration-index 1/2.
#[test]
fn gsk_style_verbatim_sampler_binds_at_host_binding_past_the_external_branch() {
    // A vertex shader using `gl_VertexID` forces the forward-VERBATIM path (GskGpu vertex-pulling).
    const GSK_VS: &str = "#version 320 es\nout vec2 vUV;\nvoid main(){ vUV = vec2(0.0); gl_Position = vec4(float(gl_VertexID), 0.0, 0.0, 1.0); }\n";
    // The GskGpu texture-block shape: each texture is EITHER a `samplerExternalOES` OR a `sampler2D` triple,
    // in two preprocessor branches. Both are textually present (neither side preprocesses here), so the host
    // counts the external decl (k=0) before the active `sampler2D GSK_TEXTURE0` (k=1).
    const GSK_FS: &str = "#version 320 es\nprecision highp float;\n\
#ifdef GSK_TEXTURE0_IS_EXTERNAL\n\
uniform samplerExternalOES GSK_TEXTURE0;\n\
#else\n\
uniform sampler2D GSK_TEXTURE0;\n\
uniform sampler2D GSK_TEXTURE0_1;\n\
uniform sampler2D GSK_TEXTURE0_2;\n\
#endif\n\
in vec2 vUV;\nout vec4 fragColor;\nvoid main(){ fragColor = texture(GSK_TEXTURE0, vUV); }\n";

    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();

    // A minimal vertex buffer + attribute so the draw lowers.
    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 48], 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);

    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, GSK_VS);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, GSK_FS);
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);

    // The driver reflects three sampler2D samplers (external skipped + deduped), but their HOST bindings
    // start at k=1 because the external `GSK_TEXTURE0` consumed k=0.
    let p = c.programs.program(prog).expect("linked program");
    assert_eq!(
        p.samp_names,
        vec!["GSK_TEXTURE0", "GSK_TEXTURE0_1", "GSK_TEXTURE0_2"]
    );
    assert_eq!(
        p.samp_bindings,
        vec![1, 2, 3],
        "the active sampler2D GSK_TEXTURE0 is host binding k=1 (past the external branch's k=0), not the \
         naive declaration index 0"
    );

    // GSK_TEXTURE0 -> texture unit 0, with a real texture bound there (the only sampled texture).
    record::uniform_sampler(&mut c, 0, 0);
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 2, 2, &[0xABu8; 16]);
    record::viewport(&mut c, [0, 0, 640, 480]);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let bg = batch
        .iter()
        .find_map(|cmd| match cmd {
            Cmd::CreateBindGroup(_, d) => Some(d),
            _ => None,
        })
        .expect("CreateBindGroup");

    // The GSK_TEXTURE0 texture lands at host binding 3 and its sampler at 4 — the layout the compiled shader
    // declares (UBO@0 / tex@1+2k / sampler@2+2k with k=1). NOT the naive declaration-index binding 1/2.
    let tex_binding = bg
        .entries
        .iter()
        .find(|e| matches!(e.resource, BindResource::Texture { .. }))
        .map(|e| e.binding)
        .expect("a texture bind entry");
    let smp_binding = bg
        .entries
        .iter()
        .find(|e| matches!(e.resource, BindResource::Sampler { .. }))
        .map(|e| e.binding)
        .expect("a sampler bind entry");
    assert_eq!(
        tex_binding, 3,
        "GSK_TEXTURE0's texture must bind at the host k=1 texture binding (3)"
    );
    assert_eq!(
        smp_binding, 4,
        "GSK_TEXTURE0's sampler must bind at the host k=1 sampler binding (4)"
    );
    assert!(
        !bg.entries.iter().any(|e| e.binding == 1),
        "nothing lands on binding 1 — that is the external branch's k=0 texture slot the shader never uses"
    );
}

// Bind-group COMPLETENESS: a program that DECLARES three samplers but has a real texture bound at only ONE
// of them must still emit a texture+sampler bind entry for EVERY declared sampler — the two empty slots get
// a shared 1x1 placeholder texture + default sampler. The compiled shader's auto bind-group layout carries
// an entry per declared+sampled binding (UBO@0 + three tex/sampler pairs at 1+2k / 2+2k = 7 bindings), so a
// bind group that emitted only the one populated pair (3 entries) NACKs against the 7-binding layout
// ("Number of bindings … (3) does not match … (7)"). This asserts the driver now covers all seven bindings
// and reuses ONE placeholder texture (created once) for both empty slots.
#[test]
fn declared_but_unbound_samplers_get_a_placeholder_bind_entry() {
    // Three sampler2D + a data uniform, all sampled — the plain ES2 (non-verbatim) path, so the sampler
    // host bindings are the identity k = declaration index (0,1,2 → tex 1/3/5, sampler 2/4/6, UBO 0).
    const VS3: &str =
        "attribute vec2 aPos;\nvarying vec2 vUV;\nvoid main(){ vUV = aPos; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    const FS3: &str = "precision mediump float;\nvarying vec2 vUV;\nuniform vec4 uColor;\n\
uniform sampler2D uTex0;\nuniform sampler2D uTex1;\nuniform sampler2D uTex2;\n\
void main(){ gl_FragColor = texture2D(uTex0, vUV) + texture2D(uTex1, vUV) + texture2D(uTex2, vUV) + uColor; }\n";

    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();

    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 48], 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);

    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS3);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, FS3);
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);

    // The program reflects three samplers + one data uniform.
    let p = c.programs.program(prog).expect("linked program");
    assert_eq!(p.samp_names, vec!["uTex0", "uTex1", "uTex2"]);
    assert_eq!(
        p.samp_bindings,
        vec![0, 1, 2],
        "ES2 path: sampler host binding == declaration index"
    );
    assert!(
        p.has_uniforms(),
        "uColor is a data uniform → a UBO at binding 0"
    );

    // Each sampler points at a distinct unit (as GskGpu's glUniform1i does), but a real texture is bound
    // ONLY at unit 0 — units 1 and 2 stay empty, so uTex1/uTex2 are declared-but-unbound.
    record::uniform_sampler(&mut c, 0, 0); // uTex0 -> unit 0 (populated)
    record::uniform_sampler(&mut c, 1, 1); // uTex1 -> unit 1 (empty)
    record::uniform_sampler(&mut c, 2, 2); // uTex2 -> unit 2 (empty)
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 2, 2, &[0xABu8; 16]);
    record::viewport(&mut c, [0, 0, 640, 480]);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let bg = batch
        .iter()
        .find_map(|cmd| match cmd {
            Cmd::CreateBindGroup(_, d) => Some(d),
            _ => None,
        })
        .expect("CreateBindGroup");

    // Every declared sampler is covered: 3 textures + 3 samplers + the UBO buffer = 7 entries, one per
    // binding of the compiled shader's auto layout — no declared sampler is skipped.
    let n_tex = bg
        .entries
        .iter()
        .filter(|e| matches!(e.resource, BindResource::Texture { .. }))
        .count();
    let n_smp = bg
        .entries
        .iter()
        .filter(|e| matches!(e.resource, BindResource::Sampler { .. }))
        .count();
    let n_buf = bg
        .entries
        .iter()
        .filter(|e| matches!(e.resource, BindResource::Buffer { .. }))
        .count();
    assert_eq!(
        n_tex, 3,
        "a texture entry for EVERY declared sampler (1 real + 2 placeholder)"
    );
    assert_eq!(n_smp, 3, "a sampler entry for EVERY declared sampler");
    assert_eq!(n_buf, 1, "the data-uniform UBO at binding 0");
    assert_eq!(
        bg.entries.len(),
        7,
        "UBO@0 + three tex/sampler pairs = 7 bindings, matching the auto layout"
    );

    // The bindings land exactly on the layout's slots: UBO 0, textures 1/3/5, samplers 2/4/6.
    let mut tex_bindings: Vec<u32> = bg
        .entries
        .iter()
        .filter(|e| matches!(e.resource, BindResource::Texture { .. }))
        .map(|e| e.binding)
        .collect();
    tex_bindings.sort_unstable();
    assert_eq!(
        tex_bindings,
        vec![1, 3, 5],
        "texture bindings at 1+2k for k=0,1,2"
    );

    // The two empty slots (uTex1, uTex2) share ONE placeholder texture id — the default is created once.
    let placeholder_ids: Vec<u32> = bg
        .entries
        .iter()
        .filter_map(|e| match e.resource {
            BindResource::Texture { id } if e.binding == 3 || e.binding == 5 => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(placeholder_ids.len(), 2);
    assert_eq!(
        placeholder_ids[0], placeholder_ids[1],
        "both empty sampler slots reuse ONE placeholder texture"
    );

    // Exactly one 1x1 placeholder texture was created for the whole frame (created once, reused).
    let placeholder_creates = batch
        .iter()
        .filter(|c| matches!(c, Cmd::CreateTexture(_, d) if d.width == 1 && d.height == 1))
        .count();
    assert_eq!(
        placeholder_creates, 1,
        "the 1x1 placeholder texture is created exactly once"
    );
}
