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
    record::use_program(&mut c, 31337);
    // Attaching/linking a never-created program+shader is a graceful failure, not a panic.
    record::attach_shader(&mut c, 31337, 12345);
    assert!(
        !record::link_program(&mut c, 31337),
        "linking a phantom program fails cleanly"
    );
    // Drawing with the phantom program bound records the draw; the frame builder drops a program-less draw.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);

    // A genuinely-created program then links + binds fine.
    let v = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        v,
        "attribute vec4 p; void main(){ gl_Position = p; }\n",
    );
    let f = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, f, "void main(){ gl_FragColor = vec4(1.0); }\n");
    let p = record::create_program(&mut c);
    record::attach_shader(&mut c, p, v);
    record::attach_shader(&mut c, p, f);
    assert!(record::link_program(&mut c, p));
    record::use_program(&mut c, p);
    assert_eq!(c.cur_prog, p);
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
