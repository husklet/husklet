use super::*;

#[test]
fn gl_compressed_and_copy_textures_allocate_extent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_gen_textures = f!(sh.gles, "glGenTextures", extern "C" fn(i32, *mut u32));
    let gl_bind_texture = f!(sh.gles, "glBindTexture", extern "C" fn(u32, u32));
    let gl_compressed_tex_image2d = f!(
        sh.gles,
        "glCompressedTexImage2D",
        extern "C" fn(u32, i32, u32, i32, i32, i32, i32, *const c_void)
    );
    let gl_copy_tex_image2d = f!(
        sh.gles,
        "glCopyTexImage2D",
        extern "C" fn(u32, i32, u32, i32, i32, i32, i32, i32)
    );
    let gl_get_tex_level_parameteriv = f!(
        sh.gles,
        "glGetTexLevelParameteriv",
        extern "C" fn(u32, i32, u32, *mut i32)
    );
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);

    // glCompressedTexImage2D allocates the bound texture's extent (payload ignored — RGBA8 can't decode).
    let mut ct: u32 = 0;
    gl_gen_textures(1, &mut ct);
    gl_bind_texture(GL_TEXTURE_2D, ct);
    let payload = [0u8; 128];
    gl_compressed_tex_image2d(
        GL_TEXTURE_2D,
        0,
        GL_COMPRESSED_RGBA8_ETC2_EAC,
        16,
        16,
        0,
        payload.len() as i32,
        payload.as_ptr() as *const c_void,
    );
    let mut cw: i32 = -1;
    let mut ch: i32 = -1;
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut cw);
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &mut ch);
    assert_eq!(
        (cw, ch),
        (16, 16),
        "compressed upload allocated the 16x16 extent"
    );

    // glCopyTexImage2D allocates the destination extent from the read framebuffer region.
    let mut pt: u32 = 0;
    gl_gen_textures(1, &mut pt);
    gl_bind_texture(GL_TEXTURE_2D, pt);
    let _ = gl_get_error();
    gl_copy_tex_image2d(GL_TEXTURE_2D, 0, GL_RGBA, 0, 0, 24, 24, 0);
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "a valid glCopyTexImage2D raises no error"
    );
    let mut pw: i32 = -1;
    let mut ph: i32 = -1;
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut pw);
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &mut ph);
    assert_eq!(
        (pw, ph),
        (24, 24),
        "glCopyTexImage2D allocated the 24x24 destination extent"
    );

    // A bad target/border is GL_INVALID_VALUE (the validated marshalling path).
    gl_copy_tex_image2d(
        GL_TEXTURE_2D,
        0,
        GL_RGBA,
        0,
        0,
        8,
        8,
        1, /* bad border */
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "a non-zero border is rejected"
    );
}

// ==================================================================================================
// 7) EGL round-trips: eglCreateContext / eglMakeCurrent / eglQueryContext / eglQuerySurface /
//    eglCreateSync + eglClientWaitSync / eglCreateImage / eglWaitClient / eglSwapInterval + the
//    per-thread current-binding getters. Driven over the surfaceless display the eglinfo path uses.
// ==================================================================================================
