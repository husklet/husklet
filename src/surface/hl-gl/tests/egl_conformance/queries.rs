//! `eglQueryString`, `eglQuerySurface`, `eglQueryContext`, `eglSurfaceAttrib`, `eglBindTexImage` and
//! `eglCopyBuffers`: the attribute sets EGL specifies and the errors it specifies for the invalid cases.
//!
//! These entry points are how a toolkit learns what it is holding. A missing attribute answered with
//! `EGL_BAD_ATTRIBUTE`, or an unsupported operation answered with `EGL_TRUE`, both mislead silently: the
//! first makes the caller think the surface is broken, the second makes it think work happened.

use super::*;

/// The attributes EGL 1.4 §3.5.6 table 3.5 defines for `eglQuerySurface`. Every one must be answerable for
/// a valid surface; values for pbuffer-only attributes are unspecified on a window surface but the QUERY
/// itself must still succeed.
const SURFACE_ATTRIBUTES: &[(i32, &str)] = &[
    (EGL_CONFIG_ID, "EGL_CONFIG_ID"),
    (EGL_WIDTH, "EGL_WIDTH"),
    (EGL_HEIGHT, "EGL_HEIGHT"),
    (EGL_RENDER_BUFFER, "EGL_RENDER_BUFFER"),
    (EGL_SWAP_BEHAVIOR, "EGL_SWAP_BEHAVIOR"),
    (EGL_MULTISAMPLE_RESOLVE, "EGL_MULTISAMPLE_RESOLVE"),
    (EGL_LARGEST_PBUFFER, "EGL_LARGEST_PBUFFER"),
    (EGL_TEXTURE_FORMAT, "EGL_TEXTURE_FORMAT"),
    (EGL_TEXTURE_TARGET, "EGL_TEXTURE_TARGET"),
    (EGL_MIPMAP_TEXTURE, "EGL_MIPMAP_TEXTURE"),
    (EGL_MIPMAP_LEVEL, "EGL_MIPMAP_LEVEL"),
    (EGL_HORIZONTAL_RESOLUTION, "EGL_HORIZONTAL_RESOLUTION"),
    (EGL_VERTICAL_RESOLUTION, "EGL_VERTICAL_RESOLUTION"),
    (EGL_PIXEL_ASPECT_RATIO, "EGL_PIXEL_ASPECT_RATIO"),
    (EGL_VG_COLORSPACE, "EGL_VG_COLORSPACE"),
    (EGL_VG_ALPHA_FORMAT, "EGL_VG_ALPHA_FORMAT"),
];

/// The attributes EGL 1.4 §3.7.4 table 3.6 defines for `eglQueryContext`.
const CONTEXT_ATTRIBUTES: &[(i32, &str)] = &[
    (EGL_CONFIG_ID, "EGL_CONFIG_ID"),
    (EGL_CONTEXT_CLIENT_TYPE, "EGL_CONTEXT_CLIENT_TYPE"),
    (EGL_CONTEXT_MAJOR_VERSION, "EGL_CONTEXT_CLIENT_VERSION"),
    (EGL_RENDER_BUFFER, "EGL_RENDER_BUFFER"),
];

#[test]
fn query_string_is_consistent_with_the_initialized_version() {
    let _serial = serial();
    let egl = Egl::bring_up();

    let version = cstr((egl.query_string)(egl.display, EGL_VERSION));
    assert!(
        version.starts_with("1.4"),
        "eglQueryString(EGL_VERSION) must begin with the major.minor eglInitialize reported (1.4), \
         got {version:?}; a client gating EGL 1.5 features parses this string"
    );
    assert!(
        !cstr((egl.query_string)(egl.display, EGL_VENDOR)).is_empty(),
        "EGL_VENDOR must not be empty"
    );
    let apis = cstr((egl.query_string)(egl.display, EGL_CLIENT_APIS));
    assert!(
        apis.split_whitespace().any(|api| api == "OpenGL_ES"),
        "EGL_CLIENT_APIS must name OpenGL_ES, got {apis:?}"
    );
}

#[test]
fn query_string_rejects_an_unrecognized_name() {
    let _serial = serial();
    let egl = Egl::bring_up();

    // EGL 1.4 §3.3: an unrecognized name returns NULL and generates EGL_BAD_PARAMETER. An empty string
    // would tell a caller the query succeeded and the answer is "nothing".
    egl.clear_error();
    let answer = (egl.query_string)(egl.display, 0x7FFF);
    assert!(
        answer.is_null(),
        "eglQueryString with an unrecognized name must return NULL, got {:?}",
        cstr(answer)
    );
    assert_eq!(
        (egl.get_error)(),
        EGL_BAD_PARAMETER,
        "an unrecognized eglQueryString name is EGL_BAD_PARAMETER"
    );
}

#[test]
fn query_surface_answers_every_specified_attribute_on_window_and_pbuffer() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let config = egl.configs()[0];

    let pbuffer_attributes = [EGL_WIDTH, 128, EGL_HEIGHT, 64, EGL_NONE];
    let pbuffer = (egl.create_pbuffer_surface)(egl.display, config, pbuffer_attributes.as_ptr());
    assert!(!pbuffer.is_null());
    let window = (egl.create_window_surface)(
        egl.display,
        config,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!window.is_null());

    for (surface, kind) in [(pbuffer, "pbuffer"), (window, "window")] {
        let mut missing = Vec::new();
        for (attribute, name) in SURFACE_ATTRIBUTES {
            let mut value = i32::MIN;
            egl.clear_error();
            if (egl.query_surface)(egl.display, surface, *attribute, &mut value) != EGL_TRUE {
                missing.push(format!("{name} -> 0x{:04x}", (egl.get_error)()));
            }
        }
        assert!(
            missing.is_empty(),
            "eglQuerySurface on a {kind} surface refuses attributes EGL 1.4 table 3.5 defines: \
             {missing:?}"
        );

        let mut render_buffer = i32::MIN;
        (egl.query_surface)(egl.display, surface, EGL_RENDER_BUFFER, &mut render_buffer);
        assert_eq!(
            render_buffer, EGL_BACK_BUFFER,
            "a double-buffered {kind} surface renders to EGL_BACK_BUFFER"
        );
        let mut behavior = i32::MIN;
        (egl.query_surface)(egl.display, surface, EGL_SWAP_BEHAVIOR, &mut behavior);
        assert!(
            behavior == EGL_BUFFER_PRESERVED || behavior == EGL_BUFFER_DESTROYED,
            "EGL_SWAP_BEHAVIOR must be one of the two specified values, got 0x{behavior:04x}"
        );
        let mut config_id = i32::MIN;
        (egl.query_surface)(egl.display, surface, EGL_CONFIG_ID, &mut config_id);
        assert_eq!(
            config_id,
            egl.attrib(config, EGL_CONFIG_ID),
            "a {kind} surface must report the config it was created on"
        );
    }

    assert_eq!((egl.destroy_surface)(egl.display, pbuffer), EGL_TRUE);
    assert_eq!((egl.destroy_surface)(egl.display, window), EGL_TRUE);
}

#[test]
fn query_surface_distinguishes_a_bad_attribute_from_a_bad_surface() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let config = egl.configs()[0];
    let window = (egl.create_window_surface)(
        egl.display,
        config,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!window.is_null());

    // An attribute that is not a surface attribute is EGL_BAD_ATTRIBUTE; an unknown surface handle is
    // EGL_BAD_SURFACE. The two must not be confused — a caller branches on them differently.
    let mut value = 999;
    egl.clear_error();
    assert_eq!(
        (egl.query_surface)(egl.display, window, 0x7FFF, &mut value),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_ATTRIBUTE);
    assert_eq!(
        value, 999,
        "a failed query must not write the out-parameter"
    );
    egl.clear_error();
    assert_eq!(
        (egl.query_surface)(
            egl.display,
            0xDEAD_usize as *mut c_void,
            EGL_WIDTH,
            &mut value
        ),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_SURFACE);

    assert_eq!((egl.destroy_surface)(egl.display, window), EGL_TRUE);
}

#[test]
fn query_context_answers_every_specified_attribute_and_the_version_it_was_given() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let config = egl.configs()[0];
    let attributes = [
        EGL_CONTEXT_MAJOR_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION,
        1,
        EGL_NONE,
    ];
    let context = (egl.create_context)(
        egl.display,
        config,
        core::ptr::null_mut(),
        attributes.as_ptr(),
    );
    assert!(!context.is_null());

    let mut missing = Vec::new();
    for (attribute, name) in CONTEXT_ATTRIBUTES {
        let mut value = i32::MIN;
        egl.clear_error();
        if (egl.query_context)(egl.display, context, *attribute, &mut value) != EGL_TRUE {
            missing.push(format!("{name} -> 0x{:04x}", (egl.get_error)()));
        }
    }
    assert!(
        missing.is_empty(),
        "eglQueryContext refuses attributes EGL 1.4 table 3.6 defines: {missing:?}"
    );

    let query = |attribute: i32| {
        let mut value = i32::MIN;
        assert_eq!(
            (egl.query_context)(egl.display, context, attribute, &mut value),
            EGL_TRUE
        );
        value
    };
    assert_eq!(
        query(EGL_CONTEXT_CLIENT_TYPE),
        EGL_OPENGL_ES_API as i32,
        "a GLES context must report EGL_OPENGL_ES_API"
    );
    assert_eq!(query(EGL_CONTEXT_MAJOR_VERSION), 3);
    assert_eq!(query(EGL_CONTEXT_MINOR_VERSION), 1);
    assert_eq!(
        query(EGL_CONFIG_ID),
        egl.attrib(config, EGL_CONFIG_ID),
        "a context must report the config it was created on"
    );

    assert_eq!((egl.destroy_context)(egl.display, context), EGL_TRUE);
}

#[test]
fn query_context_distinguishes_a_bad_attribute_from_a_bad_context() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let context = (egl.create_context)(
        egl.display,
        egl.configs()[0],
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!context.is_null());

    let mut value = 777;
    egl.clear_error();
    assert_eq!(
        (egl.query_context)(egl.display, context, 0x7FFF, &mut value),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_ATTRIBUTE);
    egl.clear_error();
    assert_eq!(
        (egl.query_context)(
            egl.display,
            0xDEAD_usize as *mut c_void,
            EGL_CONTEXT_CLIENT_TYPE,
            &mut value
        ),
        EGL_FALSE
    );
    assert_eq!((egl.get_error)(), EGL_BAD_CONTEXT);

    assert_eq!((egl.destroy_context)(egl.display, context), EGL_TRUE);
}

#[test]
fn surface_attrib_rejects_an_unsettable_attribute() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let config = egl.configs()[0];
    let surface = (egl.create_window_surface)(
        egl.display,
        config,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(!surface.is_null());

    // EGL 1.4 §3.5.6: only EGL_MIPMAP_LEVEL, EGL_MULTISAMPLE_RESOLVE and EGL_SWAP_BEHAVIOR are settable;
    // anything else is EGL_BAD_ATTRIBUTE. Accepting an unknown attribute tells the caller its request was
    // honoured when nothing was recorded.
    egl.clear_error();
    assert_eq!(
        (egl.surface_attrib)(egl.display, surface, 0x7FFF, 0),
        EGL_FALSE,
        "eglSurfaceAttrib must reject an attribute that is not settable"
    );
    assert_eq!((egl.get_error)(), EGL_BAD_ATTRIBUTE);

    assert_eq!((egl.destroy_surface)(egl.display, surface), EGL_TRUE);
}

#[test]
fn surface_attrib_rejects_a_surface_this_driver_never_handed_out() {
    let _serial = serial();
    let egl = Egl::bring_up();
    egl.clear_error();
    assert_eq!(
        (egl.surface_attrib)(
            egl.display,
            0xDEAD_usize as *mut c_void,
            EGL_SWAP_BEHAVIOR,
            EGL_BUFFER_DESTROYED
        ),
        EGL_FALSE,
        "eglSurfaceAttrib on a handle this driver never handed out must fail"
    );
    assert_eq!((egl.get_error)(), EGL_BAD_SURFACE);
}

#[test]
fn bind_tex_image_is_refused_because_no_config_offers_a_texture_bound_pbuffer() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let bind: extern "C" fn(*mut c_void, *mut c_void, i32) -> u32 =
        unsafe { core::mem::transmute(resolve(&egl, "eglBindTexImage")) };

    let config = egl.configs()[0];
    assert_eq!(
        egl.attrib(config, EGL_BIND_TO_TEXTURE_RGBA),
        EGL_FALSE as i32,
        "no config here claims render-to-texture pbuffers"
    );
    let pbuffer = pbuffer_32(&egl, config);

    // EGL 1.4 §3.6.1: eglBindTexImage on a surface whose EGL_TEXTURE_FORMAT is EGL_NO_TEXTURE — which is
    // the only possibility when no config claims EGL_BIND_TO_TEXTURE_* — is EGL_BAD_SURFACE. A silent
    // EGL_TRUE makes a caller believe the pbuffer is now readable as a texture, and sample garbage.
    egl.clear_error();
    assert_eq!(
        bind(egl.display, pbuffer, EGL_BACK_BUFFER),
        EGL_FALSE,
        "eglBindTexImage must not report success for a surface with no texture format"
    );
    assert_eq!((egl.get_error)(), EGL_BAD_SURFACE);
    assert_eq!((egl.destroy_surface)(egl.display, pbuffer), EGL_TRUE);
}

#[test]
fn copy_buffers_is_refused_because_this_driver_has_no_native_pixmap() {
    let _serial = serial();
    let egl = Egl::bring_up();
    let copy: extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> u32 =
        unsafe { core::mem::transmute(resolve(&egl, "eglCopyBuffers")) };
    let config = egl.configs()[0];
    let pbuffer = pbuffer_32(&egl, config);

    // EGL 1.4 §3.9.2: no native pixmap exists here, so a copy target can only be invalid —
    // EGL_BAD_NATIVE_PIXMAP. A silent EGL_TRUE makes the caller believe the pixels were copied out.
    egl.clear_error();
    assert_eq!(
        copy(egl.display, pbuffer, 0xDEAD_usize as *mut c_void),
        EGL_FALSE,
        "eglCopyBuffers must not report success without a native pixmap target"
    );
    assert_eq!((egl.get_error)(), EGL_BAD_NATIVE_PIXMAP);
    assert_eq!((egl.destroy_surface)(egl.display, pbuffer), EGL_TRUE);
}

/// An extension-style entry point address, asserting it resolves.
fn resolve(egl: &Egl, name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    let address = (egl.get_proc_address)(c.as_ptr());
    assert!(
        !address.is_null(),
        "{name} must resolve through eglGetProcAddress"
    );
    address
}

fn pbuffer_32(egl: &Egl, config: *mut c_void) -> *mut c_void {
    let attributes = [EGL_WIDTH, 32, EGL_HEIGHT, 32, EGL_NONE];
    let surface = (egl.create_pbuffer_surface)(egl.display, config, attributes.as_ptr());
    assert!(!surface.is_null());
    surface
}
