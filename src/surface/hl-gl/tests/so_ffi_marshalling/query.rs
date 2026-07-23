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

    // With EGL_NO_DISPLAY (null) the vendor/version/client-API identity strings are the driver's fixed ids.
    let nodpy = core::ptr::null_mut();
    assert_eq!(cstr(egl_query_string(nodpy, EGL_VENDOR_Q)), "hl-gl");
    assert_eq!(cstr(egl_query_string(nodpy, EGL_VERSION_Q)), "1.5 hl-gl");
    assert_eq!(
        cstr(egl_query_string(nodpy, EGL_CLIENT_APIS_Q)),
        "OpenGL_ES"
    );

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

    // An unrecognized name is the spec-legal empty (non-null, so an app's strlen/strstr is safe).
    let unknown = egl_query_string(nodpy, 0xBEEF);
    assert!(
        !unknown.is_null(),
        "an unknown eglQueryString name returns \"\" (non-null), never null"
    );
    assert_eq!(cstr(unknown), "");

    // A real (initialized) display => the per-DISPLAY set, which advertises the context extensions
    // (distinct from the client set — proving the string is keyed on the display argument, not constant).
    let dpy = surfaceless_display(&sh);
    let disp_ext = cstr(egl_query_string(dpy, EGL_EXTENSIONS_Q));
    assert!(
        disp_ext.contains("EGL_KHR_create_context"),
        "display ext advertises create_context: {disp_ext:?}"
    );
    assert_ne!(
        disp_ext, client_ext,
        "display and client extension strings differ"
    );
}

// ==================================================================================================
// 2) eglGetConfigAttrib — pointer-out with the driver's REAL config attributes + error paths
// ==================================================================================================
