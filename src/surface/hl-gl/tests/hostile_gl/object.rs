use super::*;

// ===================================================================================================
// bad / dangling / never-created object names → safe no-op or GL error, never a panic
// ===================================================================================================

/// Binding, using, attaching, linking, and drawing with never-created object names must not panic; a
/// valid object created afterwards still works.
#[test]
fn dangling_object_names_to_bind_use_attach_never_panic() {
    let mut c = ctx();
    // Binding never-created names is a safe no-op (state stores the name; nothing dereferenced).
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, 777);
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, 888);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 999);
    record::bind_vertex_array(&mut c, 4242);
    // glUseProgram with a name GL never generated is GL_INVALID_VALUE (ES 3.0 §2.11.3) and leaves the
    // current program untouched — so the phantom is NOT bound and the draw below is program-less.
    record::use_program(&mut c, 31337);
    assert_eq!(
        c.current_program(),
        0,
        "a rejected glUseProgram leaves the current program unchanged"
    );
    // Attaching/linking a never-created program+shader is a graceful failure, not a panic.
    record::attach_shader(&mut c, 31337, 12345);
    assert!(
        !record::link_program(&mut c, 31337),
        "linking a phantom program fails cleanly"
    );
    // A program-less draw is recorded; the frame builder drops it. No further error, and no panic.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(
        c.take_gl_error(),
        GL_INVALID_VALUE,
        "the phantom glUseProgram is the one reported error (first-error-wins)"
    );
    assert_eq!(
        c.take_gl_error(),
        GL_NO_ERROR,
        "and nothing else raised one"
    );

    // A genuinely-created program then links + binds fine.
    let v = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        v,
        "attribute vec4 p; void main(){ gl_Position = p; }\n",
    );
    record::compile_shader(&mut c, v);
    let f = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, f, "void main(){ gl_FragColor = vec4(1.0); }\n");
    record::compile_shader(&mut c, f);
    let p = record::create_program(&mut c);
    record::attach_shader(&mut c, p, v);
    record::attach_shader(&mut c, p, f);
    assert!(record::link_program(&mut c, p));
    record::use_program(&mut c, p);
    assert_eq!(c.current_program(), p);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
}

/// Deleting never-created object names returns `false` and raises no error (mirrors `glDelete*` on unknown
/// names) — no panic. `glDetachShader` on unknown names is `GL_INVALID_VALUE`.
#[test]
fn deleting_and_detaching_unknown_names_is_safe() {
    let mut c = ctx();
    assert!(!c.delete_buffer(5000));
    assert!(!c.delete_texture(5000));
    assert!(!record::delete_framebuffer(&mut c, 5000));
    assert!(!record::delete_renderbuffer(&mut c, 5000));
    assert!(!record::delete_vertex_array(&mut c, 5000));
    record::delete_program(&mut c, 5000);
    record::delete_shader(&mut c, 5000);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // glDetachShader with phantom names is GL_INVALID_VALUE, no panic.
    record::detach_shader(&mut c, 5000, 6000);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

/// A program that FAILED TO LINK, then used and drawn with anyway, must lower and present without
/// panicking — and must not silently claim it drew.
///
/// This is the glmark2 `terrain` shape: the scene's link fails, glmark2 does not check `GL_LINK_STATUS`
/// and issues its draws regardless, and the frame then reaches lowering, present and `glReadPixels` with a
/// current program that has no pipeline. Unlike a never-generated name, this program EXISTS, so
/// `glUseProgram` accepts it and the draw carries a real program id that owns no translated shader.
#[test]
fn drawing_with_a_program_that_failed_to_link_lowers_presents_and_reads_back() {
    let mut c = ctx();
    c.set_present_frame(
        Some(hl_gpu::protocol::model::descriptor::SurfaceToken::new(7).unwrap()),
        Some(hl_gpu::protocol::model::descriptor::FrameSerial::new(1).unwrap()),
    );
    let mut sink = RecordingSink::with_full_caps();

    // A vertex shader that cannot compile, so the link fails for a real reason.
    let v = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        v,
        "attribute vec2 aPos;\nuniform atomic_uint hits;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n",
    );
    record::compile_shader(&mut c, v);
    let f = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, f, "void main(){ gl_FragColor = vec4(1.0); }\n");
    record::compile_shader(&mut c, f);
    let p = record::create_program(&mut c);
    record::attach_shader(&mut c, p, v);
    record::attach_shader(&mut c, p, f);
    assert!(
        !record::link_program(&mut c, p),
        "the program must report a failed link"
    );

    // glmark2 ignores the failure and draws anyway.
    record::use_program(&mut c, p);
    assert_eq!(
        c.current_program(),
        p,
        "a linked-failed program still binds"
    );
    // A fully formed draw: real vertex data bound to attribute 0, as glmark2 has.
    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 24], 0x88E4);
    c.enable_vertex_attrib(0);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]);
    record::clear(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    record::draw_arrays(&mut c, GL_TRIANGLES, 3, 3);

    // Lowering + present must complete rather than panic or abort.
    let presented = hl_gl::service::swap::swap_buffers(&mut c, &mut sink)
        .expect("a frame whose only program failed to link must still lower");
    assert!(
        presented,
        "the program-independent glClear must still reach the framebuffer"
    );
    assert!(c.draws().is_empty(), "the frame was consumed");

    // No pipeline can exist for a program that never translated.
    assert!(
        !sink
            .batches
            .iter()
            .flatten()
            .any(|command| matches!(command, hl_gpu::Cmd::CreateRenderPipeline(..))),
        "a program that failed to link must not produce a render pipeline"
    );

    // And the readback path over the same state is equally safe.
    let pixels = readpixels::read_pixels(&mut c, &mut sink, 0, 0, 320, 240, GL_RGBA)
        .expect("glReadPixels after a failed-link frame");
    assert_eq!(pixels.len(), 320 * 240 * 4);
}
