use super::*;

#[test]
fn egl_query_string_returns_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let egl_query_string = f!(
        sh.egl,
        "eglQueryString",
        extern "C" fn(*mut c_void, i32) -> *const c_char
    );

    // EGL 1.4 §3.3 + EGL_EXT_client_extensions: vendor / version / client-APIs are PER-DISPLAY, so with
    // `EGL_NO_DISPLAY` they must be refused. This test previously asserted they answered, and its comment
    // stated that as though it were the specification — a conformance suite reading those strings takes a
    // null display for a working one and proceeds on it.
    let nodpy = core::ptr::null_mut();
    let egl_get_error = f!(sh.egl, "eglGetError", extern "C" fn() -> i32);
    for name in [EGL_VENDOR_Q, EGL_VERSION_Q, EGL_CLIENT_APIS_Q] {
        assert!(
            egl_query_string(nodpy, name).is_null(),
            "{name:#x} is per-display and must not answer without one"
        );
        assert_eq!(egl_get_error(), EGL_BAD_DISPLAY_Q, "{name:#x}");
    }

    // On a real display they are the driver's fixed identity strings.
    let dpy = surfaceless_display(&sh);
    assert_eq!(cstr(egl_query_string(dpy, EGL_VENDOR_Q)), "hl-gl");
    assert_eq!(cstr(egl_query_string(dpy, EGL_VERSION_Q)), "1.4 hl-gl");
    assert_eq!(cstr(egl_query_string(dpy, EGL_CLIENT_APIS_Q)), "OpenGL_ES");

    // A null display => CLIENT extensions: the platform-base + wayland-platform set a toolkit probes BEFORE
    // opening a display. Advertising EGL_EXT_platform_wayland is what routes a Wayland app to the window path.
    let client_ext = cstr(egl_query_string(nodpy, EGL_EXTENSIONS_Q));
    assert!(
        client_ext.contains("EGL_EXT_platform_base"),
        "client ext advertises platform_base: {client_ext:?}"
    );
    assert!(
        client_ext.contains("EGL_EXT_platform_wayland"),
        "client ext advertises platform_wayland"
    );
    assert!(client_ext.is_ascii(), "the extension string is plain ASCII");

    // EGL 1.4 §3.3: an unrecognized name returns NULL and generates EGL_BAD_PARAMETER. The empty string
    // this used to return told the caller the query had succeeded and the answer was "nothing".
    let unknown = egl_query_string(nodpy, 0xBEEF);
    assert!(
        unknown.is_null(),
        "an unknown eglQueryString name returns NULL"
    );

    // A real (initialized) display => the per-DISPLAY set, which advertises the context extensions
    // (distinct from the client set — proving the string is keyed on the display argument, not constant).
    let dpy = surfaceless_display(&sh);
    let disp_ext = cstr(egl_query_string(dpy, EGL_EXTENSIONS_Q));
    assert!(
        disp_ext.contains("EGL_KHR_create_context"),
        "display ext advertises create_context: {disp_ext:?}"
    );
    assert!(
        disp_ext.contains("EGL_KHR_image_base"),
        "display ext advertises the implemented KHR image entry points: {disp_ext:?}"
    );
    assert_ne!(
        disp_ext, client_ext,
        "display and client extension strings differ"
    );
}

// ==================================================================================================
// 2) eglGetConfigAttrib — pointer-out with the driver's REAL config attributes + error paths
// ==================================================================================================
