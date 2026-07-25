use super::*;

#[test]
fn gl_readpixels_and_vertex_attrib_getters_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_read_pixels = f!(
        sh.gles,
        "glReadPixels",
        extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void)
    );
    let gl_get_vertex_attribiv = f!(
        sh.gles,
        "glGetVertexAttribiv",
        extern "C" fn(u32, u32, *mut i32)
    );
    let gl_get_vertex_attribfv = f!(
        sh.gles,
        "glGetVertexAttribfv",
        extern "C" fn(u32, u32, *mut f32)
    );
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);

    // glReadPixels error-path marshalling (these branches validate BEFORE touching the sink, so they are
    // executor-independent): a non-UNSIGNED_BYTE type and a bad format are GL_INVALID_ENUM; negative extent
    // is GL_INVALID_VALUE; a null client pointer (no PBO bound) is GL_INVALID_VALUE.
    let mut px = [0u8; 16];
    let _ = gl_get_error();
    gl_read_pixels(
        0,
        0,
        2,
        2,
        GL_RGBA,
        GL_FLOAT,
        px.as_mut_ptr() as *mut c_void,
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_ENUM,
        "a non-UNSIGNED_BYTE type → GL_INVALID_ENUM"
    );
    gl_read_pixels(
        0,
        0,
        2,
        2,
        0xBEEF,
        GL_UNSIGNED_BYTE,
        px.as_mut_ptr() as *mut c_void,
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_ENUM,
        "an unknown format → GL_INVALID_ENUM"
    );
    gl_read_pixels(
        0,
        0,
        -1,
        2,
        GL_RGBA,
        GL_UNSIGNED_BYTE,
        px.as_mut_ptr() as *mut c_void,
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "a negative width → GL_INVALID_VALUE"
    );
    gl_read_pixels(0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null_mut());
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "a null pixels ptr (no PBO) → GL_INVALID_VALUE"
    );
    // A zero-area read is a spec-legal no-op that writes nothing and raises no error.
    gl_read_pixels(
        0,
        0,
        0,
        0,
        GL_RGBA,
        GL_UNSIGNED_BYTE,
        px.as_mut_ptr() as *mut c_void,
    );
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "a zero-area glReadPixels is a no-op"
    );

    // glGetVertexAttrib{i,f}v: null-safe out-param stores (the reference-model default is 0), and the
    // sentinel is overwritten with 0 for a non-null pointer.
    let mut iv: i32 = 0x7EAD;
    gl_get_vertex_attribiv(0, GL_VERTEX_ATTRIB_ARRAY_ENABLED, &mut iv);
    assert_eq!(iv, 0, "glGetVertexAttribiv writes the modeled 0 default");
    let mut fv: f32 = -12.5;
    gl_get_vertex_attribfv(0, GL_VERTEX_ATTRIB_ARRAY_ENABLED, &mut fv);
    assert_eq!(
        fv, 0.0,
        "glGetVertexAttribfv writes the modeled 0.0 default"
    );
    // Null out-params are ignored without a deref.
    gl_get_vertex_attribiv(0, GL_VERTEX_ATTRIB_ARRAY_ENABLED, core::ptr::null_mut());
    gl_get_vertex_attribfv(0, GL_VERTEX_ATTRIB_ARRAY_ENABLED, core::ptr::null_mut());
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "null-safe attribute getters raise no error"
    );
}
