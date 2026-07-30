//! The surface / context / make-current matrix: every context version served and refused, NULL / empty /
//! populated attribute lists, window + pbuffer + surfaceless bindings, and the `eglMakeCurrent`
//! transitions with their specified errors.
//!
//! Two of today's defects were exactly here — a refused NULL attribute list blocked 22 of 24 conformance
//! cases, and a refused `EGL_CONTEXT_FLAGS_KHR = 0` denied a context at every version. The matrix is
//! enumerated rather than sampled because each cell is one line of an application's bring-up, and a
//! failure in any of them looks to the application like "this driver has no usable GL".

use super::*;

/// A context request and whether the driver must serve it. The served set is what
/// `glGetString(GL_VERSION)` and `eglQueryContext` then have to agree with.
struct VersionRequest {
    major: i32,
    minor: i32,
    served: bool,
}

const fn served(major: i32, minor: i32) -> VersionRequest {
    VersionRequest {
        major,
        minor,
        served: true,
    }
}

const fn refused(major: i32, minor: i32) -> VersionRequest {
    VersionRequest {
        major,
        minor,
        served: false,
    }
}

/// ES 2.0, 3.0 and 3.1 are the profiles this driver documents; ES 1.x has no fixed-function pipeline here
/// and ES 3.2 / 4.x do not exist for it.
const VERSIONS: &[VersionRequest] = &[
    served(2, 0),
    served(3, 0),
    served(3, 1),
    refused(1, 0),
    refused(1, 1),
    refused(3, 2),
    refused(4, 0),
];

#[test]
fn every_documented_context_version_is_served_and_the_rest_are_refused_with_bad_match() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let shim = Shim::get();
    let get_string = f!(
        shim.gles,
        "glGetString",
        extern "C" fn(u32) -> *const c_char
    );
    let config = egl.configs()[0];

    for request in VERSIONS {
        let attributes = [
            EGL_CONTEXT_MAJOR_VERSION,
            request.major,
            EGL_CONTEXT_MINOR_VERSION,
            request.minor,
            EGL_NONE,
        ];
        egl.clear_error();
        let context = (egl.create_context)(
            egl.display,
            config,
            core::ptr::null_mut(),
            attributes.as_ptr(),
        );
        if !request.served {
            assert!(
                context.is_null(),
                "ES {}.{} is not implemented but eglCreateContext succeeded",
                request.major,
                request.minor
            );
            let error = (egl.get_error)();
            assert!(
                error == EGL_BAD_MATCH || error == EGL_BAD_ATTRIBUTE,
                "an unservable version must be EGL_BAD_MATCH (no matching config) or \
                 EGL_BAD_ATTRIBUTE (out-of-range value), got 0x{error:04x} for ES {}.{}",
                request.major,
                request.minor
            );
            continue;
        }
        assert!(
            !context.is_null(),
            "ES {}.{} must be servable; eglCreateContext failed with 0x{:04x}",
            request.major,
            request.minor,
            (egl.get_error)()
        );
        assert_eq!(
            (egl.make_current)(
                egl.display,
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                context
            ),
            EGL_TRUE
        );
        let reported = cstr(get_string(GL_VERSION));
        let expected = format!("OpenGL ES {}.{}", request.major, request.minor);
        assert!(
            reported.starts_with(&expected),
            "a context created for ES {}.{} reports {reported:?}; a toolkit gating on the version \
             string sees a different profile than it asked for",
            request.major,
            request.minor
        );
        let mut queried = -1;
        assert_eq!(
            (egl.query_context)(
                egl.display,
                context,
                EGL_CONTEXT_MAJOR_VERSION,
                &mut queried
            ),
            EGL_TRUE
        );
        assert_eq!(
            queried, request.major,
            "eglQueryContext must agree with GL_VERSION"
        );
        (egl.make_current)(
            egl.display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        assert_eq!((egl.destroy_context)(egl.display, context), EGL_TRUE);
    }
}

#[test]
fn null_empty_and_flag_bearing_attribute_lists_all_produce_a_context() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let config = egl.configs()[0];
    let empty = [EGL_NONE];
    // The flags word every libepoxy-based toolkit passes, defaulting to 0, and the no-error attribute the
    // advertised `EGL_KHR_create_context_no_error` promises to accept.
    let flags = [
        EGL_CONTEXT_MAJOR_VERSION,
        3,
        EGL_CONTEXT_FLAGS_KHR,
        0,
        EGL_NONE,
    ];
    let no_error = [
        EGL_CONTEXT_MAJOR_VERSION,
        3,
        EGL_CONTEXT_OPENGL_NO_ERROR_KHR,
        0,
        EGL_NONE,
    ];

    for (label, list) in [
        ("NULL", core::ptr::null()),
        ("EGL_NONE only", empty.as_ptr()),
        ("EGL_CONTEXT_FLAGS_KHR = 0", flags.as_ptr()),
        ("EGL_CONTEXT_OPENGL_NO_ERROR_KHR = 0", no_error.as_ptr()),
    ] {
        egl.clear_error();
        let context = (egl.create_context)(egl.display, config, core::ptr::null_mut(), list);
        assert!(
            !context.is_null(),
            "eglCreateContext with a {label} attribute list failed with 0x{:04x}",
            (egl.get_error)()
        );
        assert_eq!((egl.destroy_context)(egl.display, context), EGL_TRUE);
    }

    // `EGL_KHR_no_config_context` is advertised, so EGL_NO_CONFIG_KHR (a null config) must be accepted.
    egl.clear_error();
    let no_config = (egl.create_context)(
        egl.display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(
        !no_config.is_null(),
        "EGL_KHR_no_config_context is advertised but a null config failed with 0x{:04x}",
        (egl.get_error)()
    );
    assert_eq!((egl.destroy_context)(egl.display, no_config), EGL_TRUE);

    // An unrecognized attribute is EGL_BAD_ATTRIBUTE, and no context is created.
    egl.clear_error();
    let bad = [0x7FFF, 1, EGL_NONE];
    assert!(
        (egl.create_context)(egl.display, config, core::ptr::null_mut(), bad.as_ptr()).is_null()
    );
    assert_eq!((egl.get_error)(), EGL_BAD_ATTRIBUTE);

    // A config handle this driver never handed out is EGL_BAD_CONFIG.
    egl.clear_error();
    assert!((egl.create_context)(
        egl.display,
        0xDEAD_usize as *mut c_void,
        core::ptr::null_mut(),
        core::ptr::null()
    )
    .is_null());
    assert_eq!((egl.get_error)(), EGL_BAD_CONFIG);
}

#[test]
fn make_current_binds_releases_and_rebinds_across_surface_kinds() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let shim = Shim::get();
    let get_current_context = f!(
        shim.egl,
        "eglGetCurrentContext",
        extern "C" fn() -> *mut c_void
    );
    let get_current_display = f!(
        shim.egl,
        "eglGetCurrentDisplay",
        extern "C" fn() -> *mut c_void
    );
    let get_current_surface = f!(
        shim.egl,
        "eglGetCurrentSurface",
        extern "C" fn(i32) -> *mut c_void
    );
    const EGL_DRAW: i32 = 0x3059;
    const EGL_READ: i32 = 0x305A;

    let config = egl.configs()[0];
    let context = (egl.create_context)(
        egl.display,
        config,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!context.is_null());
    let window = (egl.create_window_surface)(
        egl.display,
        config,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!window.is_null());
    let pbuffer_attributes = [EGL_WIDTH, 96, EGL_HEIGHT, 48, EGL_NONE];
    let pbuffer = (egl.create_pbuffer_surface)(egl.display, config, pbuffer_attributes.as_ptr());
    assert!(!pbuffer.is_null());

    // Surfaceless: advertised as `EGL_KHR_surfaceless_context`, so a real context with EGL_NO_SURFACE
    // must bind.
    assert_eq!(
        (egl.make_current)(
            egl.display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            context
        ),
        EGL_TRUE,
        "EGL_KHR_surfaceless_context is advertised; a surfaceless binding must succeed"
    );
    assert_eq!(get_current_context(), context);
    assert_eq!(get_current_display(), egl.display);

    for (surface, kind) in [(window, "window"), (pbuffer, "pbuffer")] {
        assert_eq!(
            (egl.make_current)(egl.display, surface, surface, context),
            EGL_TRUE,
            "binding the {kind} surface failed with 0x{:04x}",
            (egl.get_error)()
        );
        assert_eq!(
            get_current_surface(EGL_DRAW),
            surface,
            "{kind}: EGL_DRAW must report it"
        );
        assert_eq!(
            get_current_surface(EGL_READ),
            surface,
            "{kind}: EGL_READ must report it"
        );
    }

    // Release, then rebind: the released state must report EGL_NO_CONTEXT / EGL_NO_DISPLAY.
    assert_eq!(
        (egl.make_current)(
            egl.display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut()
        ),
        EGL_TRUE
    );
    assert!(
        get_current_context().is_null(),
        "a released thread has no current context"
    );
    assert!(
        get_current_display().is_null(),
        "a released thread has no current display"
    );
    assert_eq!(
        (egl.make_current)(egl.display, window, window, context),
        EGL_TRUE,
        "rebinding after a release must succeed"
    );
    assert_eq!(get_current_context(), context);

    // EGL 1.4 §3.7.3 errors. A context with only one of draw/read is EGL_BAD_MATCH.
    egl.clear_error();
    assert_eq!(
        (egl.make_current)(egl.display, window, core::ptr::null_mut(), context),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_MATCH);
    // EGL_NO_CONTEXT with a real surface is EGL_BAD_MATCH.
    egl.clear_error();
    assert_eq!(
        (egl.make_current)(egl.display, window, window, core::ptr::null_mut()),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_MATCH);
    // A context handle this driver never handed out is EGL_BAD_CONTEXT.
    egl.clear_error();
    assert_eq!(
        (egl.make_current)(
            egl.display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            0xDEAD_usize as *mut c_void
        ),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_CONTEXT);
    // A surface handle this driver never handed out is EGL_BAD_SURFACE.
    egl.clear_error();
    assert_eq!(
        (egl.make_current)(
            egl.display,
            0xBEEF_usize as *mut c_void,
            0xBEEF_usize as *mut c_void,
            context
        ),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_SURFACE);
    // A failed transition must leave the previous binding intact.
    assert_eq!(
        get_current_context(),
        context,
        "a failed eglMakeCurrent must not release the thread's binding"
    );

    (egl.make_current)(
        egl.display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    assert_eq!((egl.destroy_surface)(egl.display, window), EGL_TRUE);
    assert_eq!((egl.destroy_surface)(egl.display, pbuffer), EGL_TRUE);
    assert_eq!((egl.destroy_context)(egl.display, context), EGL_TRUE);
    // A second destroy of the same handle is EGL_BAD_CONTEXT / EGL_BAD_SURFACE, never a silent success.
    egl.clear_error();
    assert_eq!((egl.destroy_context)(egl.display, context), EGL_FALSE);
    assert_eq!((egl.get_error)(), EGL_BAD_CONTEXT);
}

#[test]
fn a_context_current_on_another_thread_is_bad_access() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let config = egl.configs()[0];
    let context = (egl.create_context)(
        egl.display,
        config,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!context.is_null());
    assert_eq!(
        (egl.make_current)(
            egl.display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            context
        ),
        EGL_TRUE
    );

    // EGL 1.4 §3.7.3: binding a context that is current to another thread is EGL_BAD_ACCESS. Two
    // threads sharing one context by accident is a data race the driver must refuse, not absorb.
    let display = egl.display as usize;
    let handle = context as usize;
    let outcome = std::thread::spawn(move || {
        let other = Egl::bring_up();
        let error = (other.make_current)(
            display as *mut c_void,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            handle as *mut c_void,
        );
        (error, (other.get_error)())
    })
    .join()
    .expect("the second thread must not panic");
    assert_eq!(
        outcome,
        (EGL_FALSE, EGL_BAD_ACCESS),
        "a context already current on another thread must be refused with EGL_BAD_ACCESS"
    );

    (egl.make_current)(
        egl.display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        core::ptr::null_mut(),
    );
    assert_eq!((egl.destroy_context)(egl.display, context), EGL_TRUE);
}
