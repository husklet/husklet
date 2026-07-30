use super::*;

#[test]
fn gl_identity_and_scalar_state_queries_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_get_string = f!(sh.gles, "glGetString", extern "C" fn(u32) -> *const u8);
    let gl_get_stringi = f!(
        sh.gles,
        "glGetStringi",
        extern "C" fn(u32, u32) -> *const u8
    );
    let gl_get_integerv = f!(sh.gles, "glGetIntegerv", extern "C" fn(u32, *mut i32));
    let gl_get_integer64v = f!(sh.gles, "glGetInteger64v", extern "C" fn(u32, *mut i64));
    let gl_get_floatv = f!(sh.gles, "glGetFloatv", extern "C" fn(u32, *mut f32));
    let gl_get_booleanv = f!(sh.gles, "glGetBooleanv", extern "C" fn(u32, *mut u8));
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);
    let gl_clear_color = f!(sh.gles, "glClearColor", extern "C" fn(f32, f32, f32, f32));
    let gl_clear_depthf = f!(sh.gles, "glClearDepthf", extern "C" fn(f32));
    let gl_enable = f!(sh.gles, "glEnable", extern "C" fn(u32));
    let gl_depth_mask = f!(sh.gles, "glDepthMask", extern "C" fn(u8));

    // glGetString: the guest-visible GLES3 identity strings — exact values ANGLE parses.
    assert_eq!(cstr(gl_get_string(GL_VENDOR)), "hl-gl");
    assert_eq!(cstr(gl_get_string(GL_RENDERER)), "hl-gl-metal");
    assert_eq!(cstr(gl_get_string(GL_VERSION)), "OpenGL ES 3.1 hl-gl");
    assert_eq!(
        cstr(gl_get_string(GL_SHADING_LANGUAGE_VERSION)),
        "OpenGL ES GLSL ES 3.10"
    );
    assert_eq!(
        cstr(gl_get_string(GL_EXTENSIONS)),
        "GL_KHR_debug GL_EXT_texture_format_BGRA8888 GL_EXT_read_format_bgra GL_ANGLE_robust_client_memory GL_CHROMIUM_bind_generates_resource GL_CHROMIUM_copy_texture GL_ANGLE_client_arrays GL_ANGLE_webgl_compatibility GL_ANGLE_request_extension GL_OES_EGL_image GL_OES_EGL_sync GL_OES_rgb8_rgba8 GL_OES_depth24 GL_OES_mapbuffer"
    );

    // glGetIntegerv scalar limits — the truthful executor ceiling, never uninitialized garbage.
    let getint = |p: u32| {
        let mut v: i32 = -455_764_240;
        gl_get_integerv(p, &mut v);
        v
    };
    assert_eq!(getint(GL_MAX_TEXTURE_SIZE), 8192);
    assert_eq!(getint(GL_MAX_VERTEX_ATTRIBS), 16);
    assert_eq!(getint(GL_MAJOR_VERSION), 3);
    assert_eq!(getint(GL_MINOR_VERSION), 1);
    assert_eq!(
        getint(GL_NUM_EXTENSIONS),
        14,
        "matches the GL_EXTENSIONS inventory"
    );
    assert_eq!(getint(GL_DEPTH_BITS), 24);
    assert_eq!(getint(GL_STENCIL_BITS), 8);
    assert_eq!(
        getint(0xBEEF),
        0,
        "an unknown pname writes a single 0, never garbage"
    );

    // A multi-slot query (GL_VIEWPORT writes 4 ints). Drive glViewport (4 ints in) then read it back so
    // the round-trip is DETERMINISTIC and self-contained: the default viewport falls back to the surface
    // extent, which is 0x0 until some surface is made current — so asserting a non-zero default made this
    // depend on another (serialized-but-unordered) test having seeded a surface first. Setting the viewport
    // explicitly exercises the same multi-slot out-param marshalling without that ordering dependency.
    let gl_viewport = f!(sh.gles, "glViewport", extern "C" fn(i32, i32, i32, i32));
    gl_viewport(0, 0, 800, 600);
    let mut vp = [-1i32; 4];
    gl_get_integerv(GL_VIEWPORT, vp.as_mut_ptr());
    assert_eq!(
        vp,
        [0, 0, 800, 600],
        "glViewport -> glGetIntegerv(GL_VIEWPORT) round-trips 4 ints"
    );

    // glGetInteger64v: the SAME ceiling widened to i64 (width-conversion marshalling).
    let mut v64: i64 = -1;
    gl_get_integer64v(GL_MAX_TEXTURE_SIZE, &mut v64);
    assert_eq!(v64, 8192);

    // glGetStringi enumerates the SAME inventory the space-separated GL_EXTENSIONS string reports, in the
    // same order, and rejects any index at or past GL_NUM_EXTENSIONS. Deriving the expectation from the
    // string (rather than hardcoding positions) pins the invariant ANGLE relies on — the two sources of
    // truth agree — and does not rot when the inventory grows.
    let advertised = cstr(gl_get_string(GL_EXTENSIONS));
    let names: Vec<&str> = advertised.split(' ').collect();
    let count = getint(GL_NUM_EXTENSIONS);
    assert_eq!(
        names.len() as i32,
        count,
        "GL_NUM_EXTENSIONS counts exactly the names in GL_EXTENSIONS"
    );
    for (index, name) in names.iter().enumerate() {
        assert_eq!(
            &cstr(gl_get_stringi(GL_EXTENSIONS, index as u32)),
            name,
            "glGetStringi({index}) matches the GL_EXTENSIONS string at that position"
        );
    }
    assert!(gl_get_stringi(GL_EXTENSIONS, count as u32).is_null());
    assert_eq!(
        gl_get_error(),
        GL_INVALID_VALUE,
        "OOB glGetStringi index -> GL_INVALID_VALUE"
    );
    assert!(
        gl_get_stringi(0xBEEF, 0).is_null(),
        "a non-GL_EXTENSIONS name is null"
    );
    assert_eq!(
        gl_get_error(),
        GL_INVALID_ENUM,
        "bad glGetStringi name -> GL_INVALID_ENUM"
    );

    // glGetFloatv: GL_COLOR_CLEAR_VALUE writes the 4 floats glClearColor recorded.
    gl_clear_color(0.25, 0.5, 0.75, 1.0);
    let mut cc = [-1f32; 4];
    gl_get_floatv(GL_COLOR_CLEAR_VALUE, cc.as_mut_ptr());
    assert_eq!(cc, [0.25, 0.5, 0.75, 1.0]);
    gl_clear_depthf(0.5);
    let mut cd = [-1f32; 1];
    gl_get_floatv(GL_DEPTH_CLEAR_VALUE, cd.as_mut_ptr());
    assert_eq!(cd[0], 0.5);

    // glGetBooleanv: the GLboolean (u8) enable state, exact 1/0.
    gl_enable(GL_DEPTH_TEST);
    let mut b: u8 = 0xAB;
    gl_get_booleanv(GL_DEPTH_TEST, &mut b);
    assert_eq!(b, 1, "glEnable(GL_DEPTH_TEST) reads back as GLboolean 1");
    gl_depth_mask(0);
    let mut dw: u8 = 0xAB;
    gl_get_booleanv(GL_DEPTH_WRITEMASK, &mut dw);
    assert_eq!(dw, 0, "glDepthMask(false) reads back as GLboolean 0");

    // Null out-params are ignored without a deref (the guards on every getter).
    gl_get_integerv(GL_MAX_TEXTURE_SIZE, core::ptr::null_mut());
    gl_get_floatv(GL_COLOR_CLEAR_VALUE, core::ptr::null_mut());
    gl_get_booleanv(GL_DEPTH_TEST, core::ptr::null_mut());
    gl_get_integer64v(GL_MAX_TEXTURE_SIZE, core::ptr::null_mut());
    assert_eq!(
        gl_get_error(),
        GL_NO_ERROR,
        "null-safe getters raise no error"
    );
}

// ==================================================================================================
// 2) Indexed queries: glGetIntegeri_v / glGetInteger64i_v / glGetBooleani_v after a base binding
// ==================================================================================================
