use super::*;

#[test]
fn matrix_marshalling_leaves_std140_padding_to_the_uniform_model() {
    let values = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
    let packed = unsafe { mat_bytes_cr(3, 3, 1, false, values.as_ptr()) };
    assert_eq!(packed.len(), 9 * std::mem::size_of::<f32>());

    let (uniforms, size) = hl_gl::adapter::glsl::StageSources::new(
        "uniform mat3 transform;\nvoid main(){gl_Position=vec4(0);}",
        "void main(){gl_FragColor=vec4(1);}",
    )
    .uniform_layout()
    .expect("mat3 layout");
    let mut block = vec![0; size as usize];
    uniforms[0].write(&mut block, &packed);

    for (offset, expected) in [
        (0, 1.0f32),
        (4, 2.0),
        (8, 3.0),
        (16, 4.0),
        (20, 5.0),
        (24, 6.0),
        (32, 7.0),
        (36, 8.0),
        (40, 9.0),
    ] {
        assert_eq!(
            f32::from_le_bytes(block[offset..offset + 4].try_into().unwrap()),
            expected
        );
    }
}

// `EGL_KHR_create_context` (advertised in the display extension string) carries the debug / robust-access
// request as bits in `EGL_CONTEXT_FLAGS_KHR`, and its default value is 0. Refusing the attribute refuses
// EVERY version, so a toolkit that passes it gets no GL context at all.
#[test]
fn khr_create_context_flags_are_honoured_and_only_reject_the_opengl_only_bit() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let config = CONFIG_TOKEN as *mut c_void;
    let with_flags = |flags: i32| {
        [
            EGL_CONTEXT_CLIENT_VERSION,
            3,
            EGL_CONTEXT_MINOR_VERSION_KHR,
            0,
            EGL_CONTEXT_FLAGS_KHR,
            flags,
            EGL_NONE,
        ]
    };

    for flags in [
        0,
        EGL_CONTEXT_OPENGL_DEBUG_BIT_KHR,
        EGL_CONTEXT_OPENGL_ROBUST_ACCESS_BIT_KHR,
        EGL_CONTEXT_OPENGL_DEBUG_BIT_KHR | EGL_CONTEXT_OPENGL_ROBUST_ACCESS_BIT_KHR,
    ] {
        let attributes = with_flags(flags);
        let context = eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr());
        assert!(
            !context.is_null(),
            "EGL_CONTEXT_FLAGS_KHR = {flags} accepted"
        );
        let mut value = -1;
        assert_eq!(
            eglQueryContext(
                display,
                context,
                EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT,
                &mut value
            ),
            EGL_TRUE
        );
        let robust = flags & EGL_CONTEXT_OPENGL_ROBUST_ACCESS_BIT_KHR != 0;
        assert_eq!(
            value, robust as i32,
            "the robust-access bit means what the EXT attribute means"
        );
        assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
    }

    // The forward-compatible bit is defined for OpenGL only: EGL_BAD_ATTRIBUTE on an ES context.
    let attributes = with_flags(EGL_CONTEXT_OPENGL_FORWARD_COMPATIBLE_BIT_KHR);
    assert!(
        eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr()).is_null()
    );
    assert_eq!(eglGetError(), EGL_BAD_ATTRIBUTE);

    // `EGL_KHR_create_context`'s own spelling of the reset-notification attribute is accepted too.
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_KHR,
        EGL_LOSE_CONTEXT_ON_RESET_EXT,
        EGL_NONE,
    ];
    let context = eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr());
    assert!(!context.is_null());
    let mut value = 0;
    assert_eq!(
        eglQueryContext(
            display,
            context,
            EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
            &mut value
        ),
        EGL_TRUE
    );
    assert_eq!(value, EGL_LOSE_CONTEXT_ON_RESET_EXT);
    assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
}

#[test]
fn robust_es31_context_is_validated_and_queryable() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let config = CONFIG_TOKEN as *mut c_void;
    let attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        1,
        EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT,
        EGL_TRUE as i32,
        EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
        EGL_LOSE_CONTEXT_ON_RESET_EXT,
        EGL_NONE,
    ];
    let context = eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr());
    assert!(!context.is_null());

    let mut value = 0;
    assert_eq!(
        eglQueryContext(
            display,
            context,
            EGL_CONTEXT_OPENGL_ROBUST_ACCESS_EXT,
            &mut value
        ),
        EGL_TRUE
    );
    assert_eq!(value, EGL_TRUE as i32);
    assert_eq!(
        eglQueryContext(display, context, EGL_CONTEXT_MINOR_VERSION_KHR, &mut value),
        EGL_TRUE
    );
    assert_eq!(value, 1);
    assert_eq!(
        eglQueryContext(
            display,
            context,
            EGL_CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY_EXT,
            &mut value
        ),
        EGL_TRUE
    );
    assert_eq!(value, EGL_LOSE_CONTEXT_ON_RESET_EXT);

    let bad_attributes = [0xDEAD, 1, EGL_NONE];
    assert!(eglCreateContext(
        display,
        config,
        core::ptr::null_mut(),
        bad_attributes.as_ptr()
    )
    .is_null());
    assert_eq!(eglGetError(), EGL_BAD_ATTRIBUTE);

    let unsupported_version = [EGL_CONTEXT_CLIENT_VERSION, 1, EGL_NONE];
    assert!(eglCreateContext(
        display,
        config,
        core::ptr::null_mut(),
        unsupported_version.as_ptr()
    )
    .is_null());
    assert_eq!(eglGetError(), EGL_BAD_MATCH);

    assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
}

#[test]
fn chrome_es30_es20_and_dawn_es31_requests_report_the_selected_profile() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let config = CONFIG_TOKEN as *mut c_void;
    for (major, minor, version, glsl) in [
        (3, 0, "OpenGL ES 3.0 hl-gl", "OpenGL ES GLSL ES 3.00"),
        (2, 0, "OpenGL ES 2.0 hl-gl", "OpenGL ES GLSL ES 1.00"),
        (3, 1, "OpenGL ES 3.1 hl-gl", "OpenGL ES GLSL ES 3.10"),
    ] {
        let attributes = [
            EGL_CONTEXT_CLIENT_VERSION,
            major,
            EGL_CONTEXT_MINOR_VERSION_KHR,
            minor,
            EGL_NONE,
        ];
        let context = eglCreateContext(display, config, core::ptr::null_mut(), attributes.as_ptr());
        assert!(!context.is_null(), "ES {major}.{minor} context");
        assert_eq!(
            eglMakeCurrent(
                display,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                context
            ),
            EGL_TRUE
        );
        let mut reported_major = 0;
        let mut reported_minor = 0;
        glGetIntegerv(GL_MAJOR_VERSION, &mut reported_major);
        glGetIntegerv(GL_MINOR_VERSION, &mut reported_minor);
        assert_eq!((reported_major, reported_minor), (major, minor));
        let version_ptr = glGetString(GL_VERSION) as *const c_char;
        let glsl_ptr = glGetString(GL_SHADING_LANGUAGE_VERSION) as *const c_char;
        assert_eq!(
            unsafe { core::ffi::CStr::from_ptr(version_ptr) }.to_str(),
            Ok(version)
        );
        assert_eq!(
            unsafe { core::ffi::CStr::from_ptr(glsl_ptr) }.to_str(),
            Ok(glsl)
        );
        assert_eq!(
            eglMakeCurrent(
                display,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut()
            ),
            EGL_TRUE
        );
        assert_eq!(eglDestroyContext(display, context), EGL_TRUE);
    }
}

#[test]
fn chrome_shared_no_error_context_has_context_local_error_semantics() {
    let display = DISPLAY_TOKEN as *mut c_void;
    let config = CONFIG_TOKEN as *mut c_void;
    let regular_attributes = [
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        0,
        EGL_NONE,
    ];
    let regular = eglCreateContext(
        display,
        config,
        core::ptr::null_mut(),
        regular_attributes.as_ptr(),
    );
    assert!(!regular.is_null());

    let no_error_attributes = [
        EGL_CONTEXT_OPENGL_NO_ERROR_KHR,
        EGL_TRUE as i32,
        EGL_CONTEXT_CLIENT_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION_KHR,
        0,
        EGL_NONE,
    ];
    let shared = eglCreateContext(display, config, regular, no_error_attributes.as_ptr());
    assert!(!shared.is_null());

    let mut no_error = 0;
    assert_eq!(
        eglQueryContext(
            display,
            shared,
            EGL_CONTEXT_OPENGL_NO_ERROR_KHR,
            &mut no_error
        ),
        EGL_TRUE
    );
    assert_eq!(no_error, EGL_TRUE as i32);

    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            shared
        ),
        EGL_TRUE
    );
    glEGLImageTargetTexture2DOES(0xDEAD, core::ptr::null_mut());
    assert_eq!(glGetError(), GL_NO_ERROR);

    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            regular
        ),
        EGL_TRUE
    );
    glEGLImageTargetTexture2DOES(0xDEAD, core::ptr::null_mut());
    assert_eq!(glGetError(), GL_INVALID_ENUM);

    assert_eq!(
        eglMakeCurrent(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE
    );
    assert_eq!(eglDestroyContext(display, shared), EGL_TRUE);
    assert_eq!(eglDestroyContext(display, regular), EGL_TRUE);
}

#[test]
fn dawn_required_egl_procedures_all_resolve() {
    for name in [
        "eglBindAPI",
        "eglChooseConfig",
        "eglCreateContext",
        "eglCreatePbufferSurface",
        "eglDestroyContext",
        "eglDestroySurface",
        "eglGetConfigAttrib",
        "eglGetCurrentContext",
        "eglGetCurrentDisplay",
        "eglGetCurrentSurface",
        "eglGetDisplay",
        "eglGetError",
        "eglGetProcAddress",
        "eglInitialize",
        "eglMakeCurrent",
        "eglQueryContext",
        "eglQueryString",
        "eglQuerySurface",
        "eglSwapBuffers",
        "eglTerminate",
        "eglWaitClient",
    ] {
        let name = std::ffi::CString::new(name).unwrap();
        assert!(
            !eglGetProcAddress(name.as_ptr()).is_null(),
            "{} must resolve for Dawn",
            name.to_string_lossy()
        );
    }
}

#[path = "tests/current.rs"]
mod current_binding_tests;

#[path = "tests/hostile.rs"]
mod hostile_input_tests;

#[path = "tests/advertised.rs"]
mod advertised_extension_tests;

/// `eglGetDisplay(native_display)` is the ONLY place a wayland app hands the shim its own `wl_display*`,
/// and the app-surface presenter needs that connection: deriving it from the `wl_surface` proxy takes
/// `wl_proxy_get_display`, a Wayland 1.23+ symbol that 24.04-era guests (libwayland 1.22) do not export.
/// Discarding it there is what latched the presenter unavailable and sent every frame through a readback
/// present onto the shim's own mirror window. `EGL_DEFAULT_DISPLAY` is null and must stay legal — Chrome's
/// GPU process passes it — so a null never clears a connection already learnt.
#[test]
fn egl_get_display_keeps_the_app_wl_display_and_a_null_never_clears_it() {
    const APP_DISPLAY: usize = 0xDEAD_D150;
    assert_eq!(
        eglGetDisplay(APP_DISPLAY as *mut c_void),
        DISPLAY_TOKEN as *mut c_void
    );
    assert_eq!(
        GlobalState::access(|s| s.app_display),
        APP_DISPLAY,
        "the app's own wl_display must reach the app-surface presenter"
    );

    // EGL_DEFAULT_DISPLAY: still a valid display, and it must not forget what we already know.
    assert_eq!(
        eglGetDisplay(core::ptr::null_mut()),
        DISPLAY_TOKEN as *mut c_void
    );
    assert_eq!(GlobalState::access(|s| s.app_display), APP_DISPLAY);
}

/// The present-route counters are the guard this driver did not have: `swap` decided the route every
/// frame and discarded it, so a window whose frames had all degraded to a `glReadPixels` readback still
/// presented correct pixels and passed every rung that only judges pixels. `eglQueryString` reports them
/// so a probe can assert the route, and an unrecognized name must still be `EGL_BAD_PARAMETER`.
#[test]
fn present_route_is_queryable_and_counts_the_degraded_readback() {
    let dpy = DISPLAY_TOKEN as *mut c_void;
    let read = || {
        let answer = eglQueryString(dpy, EGL_PRESENT_ROUTE_HL);
        assert!(!answer.is_null(), "the route query must answer");
        unsafe { std::ffi::CStr::from_ptr(answer) }
            .to_str()
            .expect("route is utf-8")
            .to_owned()
    };

    let before = read();
    assert!(
        before.starts_with("native=") && before.contains(" readback="),
        "unexpected route string {before:?}"
    );
    let readback_before = present::PresentStats::counts().1;

    // A degraded window frame must move the readback counter — that is the whole assertion a rung makes.
    present::PresentStats::record_readback(0xABCD);
    assert_eq!(present::PresentStats::counts().1, readback_before + 1);
    assert_eq!(read(), format!("native={} readback={}", present::PresentStats::counts().0, readback_before + 1));

    // The new name must not have weakened the spec'd rejection of an unknown one.
    assert!(eglQueryString(dpy, 0x1234).is_null());
    assert_eq!(eglGetError(), EGL_BAD_PARAMETER);
}
