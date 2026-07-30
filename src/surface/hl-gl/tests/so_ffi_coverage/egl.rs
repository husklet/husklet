use super::*;

const ES31_CONTEXT: [i32; 5] = [EGL_CONTEXT_CLIENT_VERSION, 3, 0x30FB, 1, 0x3038];

#[test]
fn egl_owner_binding_selects_the_gles_client_context() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let egl_get_display = f!(
        sh.egl,
        "eglGetDisplay",
        extern "C" fn(*mut c_void) -> *mut c_void
    );
    let egl_initialize = f!(
        sh.egl,
        "eglInitialize",
        extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32
    );
    let egl_create_context = f!(
        sh.egl,
        "eglCreateContext",
        extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void
    );
    let egl_make_current = f!(
        sh.egl,
        "eglMakeCurrent",
        extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32
    );
    let gl_viewport = f!(sh.gles, "glViewport", extern "C" fn(i32, i32, i32, i32));
    let gl_get_integerv = f!(sh.gles, "glGetIntegerv", extern "C" fn(u32, *mut i32));

    let display = egl_get_display(core::ptr::null_mut());
    assert_eq!(egl_initialize(display, &mut 0, &mut 0), EGL_TRUE);
    let first = egl_create_context(
        display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        ES31_CONTEXT.as_ptr(),
    );
    let second = egl_create_context(
        display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        ES31_CONTEXT.as_ptr(),
    );

    assert_eq!(
        egl_make_current(display, core::ptr::null_mut(), core::ptr::null_mut(), first),
        EGL_TRUE
    );
    gl_viewport(1, 2, 300, 400);

    assert_eq!(
        egl_make_current(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            second
        ),
        EGL_TRUE
    );
    gl_viewport(5, 6, 700, 800);

    assert_eq!(
        egl_make_current(display, core::ptr::null_mut(), core::ptr::null_mut(), first),
        EGL_TRUE
    );
    let mut viewport = [-1; 4];
    gl_get_integerv(GL_VIEWPORT, viewport.as_mut_ptr());
    assert_eq!(
        viewport,
        [1, 2, 300, 400],
        "libGLESv2 must observe libEGL's calling-thread current context"
    );

    assert_eq!(
        egl_make_current(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE
    );
}

#[test]
fn egl_context_surface_sync_roundtrips_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };
    // Deterministic surface geometry for eglQuerySurface.
    std::env::set_var("HL_GL_SURFACE_W", "640");
    std::env::set_var("HL_GL_SURFACE_H", "480");

    let egl_get_proc = f!(
        sh.egl,
        "eglGetProcAddress",
        extern "C" fn(*const c_char) -> *mut c_void
    );
    let egl_initialize = f!(
        sh.egl,
        "eglInitialize",
        extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32
    );
    let egl_create_context = f!(
        sh.egl,
        "eglCreateContext",
        extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i32) -> *mut c_void
    );
    let egl_make_current = f!(
        sh.egl,
        "eglMakeCurrent",
        extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32
    );
    let egl_get_current_context = f!(
        sh.egl,
        "eglGetCurrentContext",
        extern "C" fn() -> *mut c_void
    );
    let egl_get_current_display = f!(
        sh.egl,
        "eglGetCurrentDisplay",
        extern "C" fn() -> *mut c_void
    );
    let egl_get_current_surface = f!(
        sh.egl,
        "eglGetCurrentSurface",
        extern "C" fn(i32) -> *mut c_void
    );
    let egl_query_context = f!(
        sh.egl,
        "eglQueryContext",
        extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32
    );
    let egl_query_surface = f!(
        sh.egl,
        "eglQuerySurface",
        extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32
    );
    let egl_create_pbuffer = f!(
        sh.egl,
        "eglCreatePbufferSurface",
        extern "C" fn(*mut c_void, *mut c_void, *const i32) -> *mut c_void
    );
    let egl_create_sync = f!(
        sh.egl,
        "eglCreateSync",
        extern "C" fn(*mut c_void, u32, *const isize) -> *mut c_void
    );
    let egl_client_wait_sync = f!(
        sh.egl,
        "eglClientWaitSync",
        extern "C" fn(*mut c_void, *mut c_void, i32, u64) -> i32
    );
    let egl_destroy_sync = f!(
        sh.egl,
        "eglDestroySync",
        extern "C" fn(*mut c_void, *mut c_void) -> u32
    );
    let egl_create_image = f!(
        sh.egl,
        "eglCreateImage",
        extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const isize) -> *mut c_void
    );
    let egl_wait_client = f!(sh.egl, "eglWaitClient", extern "C" fn() -> u32);
    let egl_swap_interval = f!(
        sh.egl,
        "eglSwapInterval",
        extern "C" fn(*mut c_void, i32) -> u32
    );

    // Bring up the surfaceless display (the same path egl_surfaceless_config.rs drives).
    let get_platform_display: extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void = unsafe {
        let c = std::ffi::CString::new("eglGetPlatformDisplayEXT").unwrap();
        core::mem::transmute(egl_get_proc(c.as_ptr()))
    };
    let dpy = get_platform_display(
        EGL_PLATFORM_SURFACELESS_MESA,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!dpy.is_null());
    assert_eq!(egl_initialize(dpy, &mut 0, &mut 0), EGL_TRUE);

    // eglCreateContext hands back a non-null opaque EGLContext token.
    let ctx = egl_create_context(
        dpy,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        ES31_CONTEXT.as_ptr(),
    );
    assert!(
        !ctx.is_null(),
        "eglCreateContext returns a non-null context token"
    );
    let surf = egl_create_pbuffer(dpy, 1usize as *mut c_void, core::ptr::null());
    assert!(
        !surf.is_null(),
        "eglCreatePbufferSurface returns a surface token"
    );

    // eglMakeCurrent binds ctx+surface on THIS thread; the getters report exactly that binding back.
    assert_eq!(egl_make_current(dpy, surf, surf, ctx), EGL_TRUE);
    assert_eq!(
        egl_get_current_context(),
        ctx,
        "eglGetCurrentContext reports the bound context"
    );
    assert_eq!(
        egl_get_current_display(),
        dpy,
        "eglGetCurrentDisplay reports the bound display"
    );
    const EGL_DRAW: i32 = 0x3059;
    const EGL_READ: i32 = 0x305A;
    assert_eq!(
        egl_get_current_surface(EGL_DRAW),
        surf,
        "the draw surface round-trips"
    );
    assert_eq!(
        egl_get_current_surface(EGL_READ),
        surf,
        "the read surface round-trips"
    );

    // eglQueryContext: the fixed GLES3 identity libepoxy classifies on (ANGLE-facing).
    let qctx = |a: i32| {
        let mut v: i32 = -1;
        assert_eq!(egl_query_context(dpy, ctx, a, &mut v), EGL_TRUE);
        v
    };
    assert_eq!(qctx(EGL_CONTEXT_CLIENT_TYPE) as u32, EGL_OPENGL_ES_API);
    assert_eq!(qctx(EGL_CONTEXT_CLIENT_VERSION), 3);
    assert_eq!(qctx(EGL_RENDER_BUFFER), EGL_BACK_BUFFER);
    let mut unknown = -1;
    assert_eq!(
        egl_query_context(dpy, ctx, 0xBEEF, &mut unknown),
        EGL_FALSE,
        "an unknown context attribute is rejected"
    );
    assert_eq!(unknown, -1);
    assert_eq!(
        egl_query_context(dpy, ctx, EGL_CONTEXT_CLIENT_TYPE, core::ptr::null_mut()),
        EGL_FALSE
    );

    // eglQuerySurface: the live surface geometry (640x480 from the env).
    let mut w: i32 = -1;
    let mut h: i32 = -1;
    assert_eq!(egl_query_surface(dpy, surf, EGL_WIDTH, &mut w), EGL_TRUE);
    assert_eq!(egl_query_surface(dpy, surf, EGL_HEIGHT, &mut h), EGL_TRUE);
    assert_eq!(
        (w, h),
        (640, 480),
        "eglQuerySurface reports the live pbuffer extent"
    );
    assert_eq!(
        egl_query_surface(dpy, surf, EGL_WIDTH, core::ptr::null_mut()),
        EGL_FALSE
    );

    // A fence starts pending and becomes satisfied only after the host executor reports completion.
    if std::env::var_os("HL_GPU_EXEC").is_some() {
        let sync = egl_create_sync(dpy, EGL_SYNC_FENCE, core::ptr::null());
        assert!(!sync.is_null());
        assert_eq!(egl_client_wait_sync(dpy, sync, 0, 0), 0x30F5);
        assert_eq!(egl_client_wait_sync(dpy, sync, 1, u64::MAX), 0x30F6);
        assert_eq!(egl_destroy_sync(dpy, sync), EGL_TRUE);
        assert_eq!(egl_destroy_sync(dpy, sync), EGL_FALSE);
    }

    // Texture-backed EGLImage import is unsupported until the texture object can be shared truthfully.
    let img = egl_create_image(
        dpy,
        ctx,
        0x30B9, /* EGL_GL_TEXTURE_2D */
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(
        img.is_null(),
        "unsupported image targets return EGL_NO_IMAGE"
    );

    // eglWaitClient / eglSwapInterval succeed (deferred model completes synchronously at swap).
    assert_eq!(egl_wait_client(), EGL_TRUE);
    assert_eq!(egl_swap_interval(dpy, 1), EGL_TRUE);

    // Release the thread binding so we leave no current context dangling for the next serialized test.
    assert_eq!(
        egl_make_current(
            dpy,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE
    );
    assert!(
        egl_get_current_context().is_null(),
        "the binding is released"
    );
}
