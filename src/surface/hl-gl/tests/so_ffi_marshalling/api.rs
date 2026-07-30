use super::*;

#[test]
fn egl_bind_and_query_api_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let egl_bind_api = f!(sh.egl, "eglBindAPI", extern "C" fn(u32) -> u32);
    let egl_query_api = f!(sh.egl, "eglQueryAPI", extern "C" fn() -> u32);
    let egl_get_error = f!(sh.egl, "eglGetError", extern "C" fn() -> i32);

    // The default bound API is GLES (the only API this driver serves).
    assert_eq!(
        egl_query_api(),
        EGL_OPENGL_ES_API,
        "default bound API is EGL_OPENGL_ES_API"
    );

    // Binding GLES succeeds and reads back.
    assert_eq!(egl_bind_api(EGL_OPENGL_ES_API), EGL_TRUE);
    assert_eq!(egl_query_api(), EGL_OPENGL_ES_API);

    // A non-GLES API is EGL_FALSE + EGL_BAD_PARAMETER, and the bound API is unchanged (never silently taken).
    let _ = egl_get_error();
    assert_eq!(
        egl_bind_api(EGL_OPENGL_API),
        EGL_FALSE,
        "EGL_OPENGL_API is not served"
    );
    assert_eq!(egl_get_error(), EGL_BAD_PARAMETER);
    assert_eq!(
        egl_bind_api(EGL_OPENVG_API),
        EGL_FALSE,
        "EGL_OPENVG_API is not served"
    );
    assert_eq!(egl_get_error(), EGL_BAD_PARAMETER);
    assert_eq!(
        egl_query_api(),
        EGL_OPENGL_ES_API,
        "a rejected bind leaves the API unchanged"
    );
    // A successful path clears no lingering error.
    assert_eq!(egl_bind_api(EGL_OPENGL_ES_API), EGL_TRUE);
    assert_eq!(egl_get_error(), EGL_SUCCESS);
}

// ==================================================================================================
// 5) eglGetProcAddress — function-pointer return; a resolved core name is a CALLABLE pointer
// ==================================================================================================
#[test]
fn egl_get_proc_address_returns_callable_pointers() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let egl_get_proc = f!(
        sh.egl,
        "eglGetProcAddress",
        extern "C" fn(*const c_char) -> *mut c_void
    );
    let get = |name: &str| {
        let c = std::ffi::CString::new(name).unwrap();
        egl_get_proc(c.as_ptr())
    };

    // Core egl*/gl* names all resolve to a non-null pointer (a null for a core name would crash an app that
    // loads its entry points through eglGetProcAddress instead of the dynamic linker).
    for name in [
        "eglGetError",
        "eglInitialize",
        "eglChooseConfig",
        "glClear",
        "glDrawArrays",
        "glGetString",
        "glDebugMessageCallbackKHR",
        "glDebugMessageControlKHR",
        "glDebugMessageInsertKHR",
        "glGetDebugMessageLogKHR",
        "glGetObjectLabelKHR",
        "glGetObjectPtrLabelKHR",
        "glObjectLabelKHR",
        "glObjectPtrLabelKHR",
        "glPopDebugGroupKHR",
        "glPushDebugGroupKHR",
    ] {
        assert!(
            !get(name).is_null(),
            "eglGetProcAddress({name}) resolves a core entry point"
        );
    }

    // The resolved pointer is not merely non-null — it is CALLABLE and behaves. `glGetString` resolved out
    // of libEGL shares the process-global State, so calling it returns the driver's vendor id.
    let gl_get_string: extern "C" fn(u32) -> *const c_char =
        unsafe { core::mem::transmute(get("glGetString")) };
    assert_eq!(
        cstr(gl_get_string(GL_VENDOR)),
        "hl-gl",
        "the resolved glGetString is callable + correct"
    );

    // `eglGetError` resolved through the trampoline agrees with the directly-linked symbol (same function).
    let egl_get_error_via_proc: extern "C" fn() -> i32 =
        unsafe { core::mem::transmute(get("eglGetError")) };
    let egl_get_error = f!(sh.egl, "eglGetError", extern "C" fn() -> i32);
    let _ = egl_get_error();
    assert_eq!(
        egl_get_error_via_proc(),
        EGL_SUCCESS,
        "the resolved eglGetError reads the clean state"
    );

    // An unknown / null name is the spec-legal null.
    assert!(
        get("glThisIsNotARealEntryPoint").is_null(),
        "an unadvertised name resolves to null"
    );
    assert!(
        egl_get_proc(core::ptr::null()).is_null(),
        "a null procname resolves to null (no deref)"
    );
}

// ==================================================================================================
// 6) Array-IN buffer upload — glBufferData / glBufferSubData ptr+size, read back BYTE-FOR-BYTE via the
//    host-storage pointer glMapBufferRange hands out.
// ==================================================================================================
