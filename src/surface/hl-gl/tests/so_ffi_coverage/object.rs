use super::*;

#[test]
fn gl_object_state_queries_marshal_real_state() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_gen_buffers = f!(sh.gles, "glGenBuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_buffer = f!(sh.gles, "glBindBuffer", extern "C" fn(u32, u32));
    let gl_buffer_data = f!(
        sh.gles,
        "glBufferData",
        extern "C" fn(u32, isize, *const c_void, u32)
    );
    let gl_get_buffer_parameteriv = f!(
        sh.gles,
        "glGetBufferParameteriv",
        extern "C" fn(u32, u32, *mut i32)
    );
    let gl_get_buffer_parameteri64v = f!(
        sh.gles,
        "glGetBufferParameteri64v",
        extern "C" fn(u32, u32, *mut i64)
    );
    let gl_gen_textures = f!(sh.gles, "glGenTextures", extern "C" fn(i32, *mut u32));
    let gl_bind_texture = f!(sh.gles, "glBindTexture", extern "C" fn(u32, u32));
    let gl_tex_parameteri = f!(sh.gles, "glTexParameteri", extern "C" fn(u32, u32, i32));
    let gl_get_tex_parameteriv = f!(
        sh.gles,
        "glGetTexParameteriv",
        extern "C" fn(u32, u32, *mut i32)
    );
    let gl_get_tex_parameterfv = f!(
        sh.gles,
        "glGetTexParameterfv",
        extern "C" fn(u32, u32, *mut f32)
    );
    let gl_tex_image2d = f!(
        sh.gles,
        "glTexImage2D",
        extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void)
    );
    let gl_get_tex_level_parameteriv = f!(
        sh.gles,
        "glGetTexLevelParameteriv",
        extern "C" fn(u32, i32, u32, *mut i32)
    );
    let gl_gen_renderbuffers = f!(sh.gles, "glGenRenderbuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_renderbuffer = f!(sh.gles, "glBindRenderbuffer", extern "C" fn(u32, u32));
    let gl_renderbuffer_storage = f!(
        sh.gles,
        "glRenderbufferStorage",
        extern "C" fn(u32, u32, i32, i32)
    );
    let gl_get_renderbuffer_parameteriv = f!(
        sh.gles,
        "glGetRenderbufferParameteriv",
        extern "C" fn(u32, u32, *mut i32)
    );
    let gl_gen_framebuffers = f!(sh.gles, "glGenFramebuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_framebuffer = f!(sh.gles, "glBindFramebuffer", extern "C" fn(u32, u32));
    let gl_framebuffer_texture2d = f!(
        sh.gles,
        "glFramebufferTexture2D",
        extern "C" fn(u32, u32, u32, u32, i32)
    );
    let gl_get_fb_attachment_parameteriv = f!(
        sh.gles,
        "glGetFramebufferAttachmentParameteriv",
        extern "C" fn(u32, u32, u32, *mut i32)
    );

    // glGetBufferParameteriv: real byte size + usage of the bound buffer.
    let mut buf: u32 = 0;
    gl_gen_buffers(1, &mut buf);
    gl_bind_buffer(GL_ARRAY_BUFFER, buf);
    let bytes = [7u8; 48];
    gl_buffer_data(
        GL_ARRAY_BUFFER,
        bytes.len() as isize,
        bytes.as_ptr() as *const c_void,
        GL_STATIC_DRAW,
    );
    let mut sz: i32 = -1;
    gl_get_buffer_parameteriv(GL_ARRAY_BUFFER, GL_BUFFER_SIZE, &mut sz);
    assert_eq!(sz, 48, "GL_BUFFER_SIZE is the uploaded byte length");
    let mut usage: i32 = -1;
    gl_get_buffer_parameteriv(GL_ARRAY_BUFFER, GL_BUFFER_USAGE, &mut usage);
    assert_eq!(
        usage as u32, GL_STATIC_DRAW,
        "GL_BUFFER_USAGE echoes the stored hint"
    );
    // The 64-bit view (width conversion).
    let mut sz64: i64 = -1;
    gl_get_buffer_parameteri64v(GL_ARRAY_BUFFER, GL_BUFFER_SIZE, &mut sz64);
    assert_eq!(sz64, 48);

    // glGetTexParameteriv / fv: filter state of the bound texture (fv shares the same int, widened).
    let mut tex: u32 = 0;
    gl_gen_textures(1, &mut tex);
    gl_bind_texture(GL_TEXTURE_2D, tex);
    gl_tex_parameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    gl_tex_parameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    let mut minf: i32 = -1;
    gl_get_tex_parameteriv(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, &mut minf);
    assert_eq!(
        minf, GL_NEAREST,
        "GL_TEXTURE_MIN_FILTER reads back what was set"
    );
    let mut magf: f32 = -1.0;
    gl_get_tex_parameterfv(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, &mut magf);
    assert_eq!(
        magf as i32, GL_LINEAR,
        "glGetTexParameterfv widens the enum to f32 exactly"
    );

    // glGetTexLevelParameteriv: level-0 extent + internal format of a uploaded texture.
    gl_tex_image2d(
        GL_TEXTURE_2D,
        0,
        GL_RGBA as i32,
        64,
        32,
        0,
        GL_RGBA,
        GL_UNSIGNED_BYTE,
        core::ptr::null(),
    );
    let mut w: i32 = -1;
    let mut h: i32 = -1;
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, &mut w);
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_HEIGHT, &mut h);
    assert_eq!((w, h), (64, 32), "glTexImage2D extent is reflected");
    let mut ifmt: i32 = -1;
    gl_get_tex_level_parameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_INTERNAL_FORMAT, &mut ifmt);
    assert_eq!(
        ifmt as u32, GL_RGBA8,
        "the model materializes every texture as RGBA8"
    );

    // glGetRenderbufferParameteriv: extent + RGBA8 format of the bound renderbuffer.
    let mut rb: u32 = 0;
    gl_gen_renderbuffers(1, &mut rb);
    gl_bind_renderbuffer(GL_RENDERBUFFER, rb);
    gl_renderbuffer_storage(GL_RENDERBUFFER, GL_RGBA8, 100, 50);
    let mut rw: i32 = -1;
    let mut rh: i32 = -1;
    gl_get_renderbuffer_parameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_WIDTH, &mut rw);
    gl_get_renderbuffer_parameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_HEIGHT, &mut rh);
    assert_eq!((rw, rh), (100, 50));
    let mut rifmt: i32 = -1;
    gl_get_renderbuffer_parameteriv(GL_RENDERBUFFER, GL_RENDERBUFFER_INTERNAL_FORMAT, &mut rifmt);
    assert_eq!(rifmt as u32, GL_RGBA8);

    // glGetFramebufferAttachmentParameteriv: a color-attached texture reflects TYPE=GL_TEXTURE + its name.
    let mut fb: u32 = 0;
    gl_gen_framebuffers(1, &mut fb);
    gl_bind_framebuffer(GL_FRAMEBUFFER, fb);
    gl_framebuffer_texture2d(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    let mut atype: i32 = -1;
    gl_get_fb_attachment_parameteriv(
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE,
        &mut atype,
    );
    assert_eq!(
        atype, GL_TEXTURE,
        "the color attachment's object type is GL_TEXTURE"
    );
    let mut aname: i32 = -1;
    gl_get_fb_attachment_parameteriv(
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME,
        &mut aname,
    );
    assert_eq!(aname as u32, tex, "and its name is the attached texture");
}

// ==================================================================================================
// 6) Compressed + copy texture uploads: assert the destination extent is allocated (reflected via
//    glGetTexLevelParameteriv) even though no pixels are decoded/copied (honest model behavior).
// ==================================================================================================
