use super::*;

#[test]
fn egl_get_config_attrib_marshals_real_values_and_errors() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let dpy = surfaceless_display(&sh);
    let egl_choose_config = f!(
        sh.egl,
        "eglChooseConfig",
        extern "C" fn(*mut c_void, *const i32, *mut *mut c_void, i32, *mut i32) -> u32
    );
    let egl_get_config_attrib = f!(
        sh.egl,
        "eglGetConfigAttrib",
        extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32
    );
    let egl_get_error = f!(sh.egl, "eglGetError", extern "C" fn() -> i32);

    // Select the depth+stencil tranche explicitly — a match-all list would sort the SMALLEST depth /
    // stencil first (EGL 1.5 §3.4.1), which is a different, equally advertised config.
    let request: [i32; 5] = [EGL_DEPTH_SIZE, 24, EGL_STENCIL_SIZE, 8, EGL_NONE];
    let mut cfg: *mut c_void = core::ptr::null_mut();
    let mut n: i32 = -1;
    assert_eq!(
        egl_choose_config(dpy, request.as_ptr(), &mut cfg, 1, &mut n),
        EGL_TRUE
    );
    assert_eq!(n, 1, "one handle fits the caller's one-slot array");
    assert!(!cfg.is_null(), "the selected EGLConfig handle is non-null");

    // Every attribute reads back its truthful value into the caller's `*value` out-param.
    let attr = |a: i32| {
        let mut v: i32 = i32::MIN;
        assert_eq!(
            egl_get_config_attrib(dpy, cfg, a, &mut v),
            EGL_TRUE,
            "attr {a:#x} succeeds"
        );
        v
    };
    assert_eq!(attr(EGL_RED_SIZE), 8);
    assert_eq!(attr(EGL_GREEN_SIZE), 8);
    assert_eq!(attr(EGL_BLUE_SIZE), 8);
    assert_eq!(attr(EGL_ALPHA_SIZE), 8);
    assert_eq!(attr(EGL_BUFFER_SIZE), 32, "8+8+8+8 color buffer");
    assert_eq!(attr(EGL_DEPTH_SIZE), 24);
    assert_eq!(attr(EGL_STENCIL_SIZE), 8);
    assert_eq!(attr(EGL_CONFIG_ID), 1, "EGL config ids are 1-based");
    assert_eq!(attr(EGL_COLOR_BUFFER_TYPE), EGL_RGB_BUFFER);
    assert_eq!(
        attr(EGL_RENDERABLE_TYPE),
        EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES3_BIT,
        "ES2|ES3"
    );
    assert_eq!(
        attr(EGL_SURFACE_TYPE),
        EGL_WINDOW_BIT | EGL_PBUFFER_BIT,
        "window|pbuffer"
    );

    // A foreign config handle → EGL_BAD_CONFIG, EGL_FALSE, and `value` is left UNTOUCHED (never a deref of
    // the unknown handle, never a fabricated 0).
    let _ = egl_get_error();
    let mut sentinel: i32 = 0x5A5A_5A5A;
    let bogus = 0xDEAD_0000usize as *mut c_void;
    assert_eq!(
        egl_get_config_attrib(dpy, bogus, EGL_RED_SIZE, &mut sentinel),
        EGL_FALSE
    );
    assert_eq!(
        egl_get_error(),
        EGL_BAD_CONFIG,
        "a foreign config raises EGL_BAD_CONFIG"
    );
    assert_eq!(
        sentinel, 0x5A5A_5A5A,
        "a rejected query does not write `value`"
    );

    // An unrecognized attribute on OUR config → EGL_BAD_ATTRIBUTE (not a silent 0).
    assert_eq!(
        egl_get_config_attrib(dpy, cfg, 0x1234, &mut sentinel),
        EGL_FALSE
    );
    assert_eq!(
        egl_get_error(),
        EGL_BAD_ATTRIBUTE,
        "an unknown attribute raises EGL_BAD_ATTRIBUTE"
    );
    assert_eq!(sentinel, 0x5A5A_5A5A);

    // A null `value` out-param → EGL_BAD_PARAMETER without a deref.
    assert_eq!(
        egl_get_config_attrib(dpy, cfg, EGL_RED_SIZE, core::ptr::null_mut()),
        EGL_FALSE
    );
    assert_eq!(
        egl_get_error(),
        EGL_BAD_PARAMETER,
        "a null value ptr raises EGL_BAD_PARAMETER"
    );
}

// ==================================================================================================
// 3) eglChooseConfig / eglGetConfigs — attrib-list IN, configs[] + count OUT (the enumeration contract)
// ==================================================================================================
#[test]
fn egl_choose_and_get_configs_marshal_arrays_and_count() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let dpy = surfaceless_display(&sh);
    let egl_choose_config = f!(
        sh.egl,
        "eglChooseConfig",
        extern "C" fn(*mut c_void, *const i32, *mut *mut c_void, i32, *mut i32) -> u32
    );
    let egl_get_configs = f!(
        sh.egl,
        "eglGetConfigs",
        extern "C" fn(*mut c_void, *mut *mut c_void, i32, *mut i32) -> u32
    );
    let egl_get_config_attrib = f!(
        sh.egl,
        "eglGetConfigAttrib",
        extern "C" fn(*mut c_void, *mut c_void, i32, *mut i32) -> u32
    );
    let egl_get_error = f!(sh.egl, "eglGetError", extern "C" fn() -> i32);

    // A populated attrib-list IN-pointer (the shim receives *const i32; the request matches our config).
    let attribs: [i32; 11] = [
        EGL_RED_SIZE,
        8,
        EGL_GREEN_SIZE,
        8,
        EGL_BLUE_SIZE,
        8,
        EGL_ALPHA_SIZE,
        8,
        EGL_RENDERABLE_TYPE,
        EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES3_BIT,
        EGL_NONE,
    ];

    // Count-only query: a NULL `configs` array returns the total available in `num_config`, writes nothing.
    let mut count: i32 = -1;
    assert_eq!(
        egl_choose_config(dpy, attribs.as_ptr(), core::ptr::null_mut(), 0, &mut count),
        EGL_TRUE
    );
    assert_eq!(
        count,
        hl_gl::service::config::NUM_CONFIGS,
        "count-only eglChooseConfig reports every matching config, not just the first"
    );

    // Real array: fill up to config_size handles and report how many were written. A sentinel slot proves
    // the bounded copy did not overrun (only slot 0 is written).
    let mut configs: [*mut c_void; 4] = [0xF00Dusize as *mut c_void; 4];
    let mut num: i32 = -1;
    assert_eq!(
        egl_choose_config(
            dpy,
            attribs.as_ptr(),
            configs.as_mut_ptr(),
            configs.len() as i32,
            &mut num
        ),
        EGL_TRUE
    );
    assert_eq!(num, hl_gl::service::config::NUM_CONFIGS, "every match written");
    for slot in configs.iter().take(num as usize) {
        assert!(!slot.is_null(), "each slot holds a real EGLConfig handle");
    }
    assert_eq!(
        configs[num as usize], 0xF00Dusize as *mut c_void,
        "the slot past the last match is untouched (bounded copy)"
    );
    // The written handle is a real one: eglGetConfigAttrib(EGL_RED_SIZE) reads 8 through it.
    let mut red: i32 = -1;
    assert_eq!(
        egl_get_config_attrib(dpy, configs[0], EGL_RED_SIZE, &mut red),
        EGL_TRUE
    );
    assert_eq!(red, 8, "the enumerated config is the real 8-bit-red config");

    // config_size 0 with a real array writes nothing and reports 0 (a legal "give me none").
    let mut zero: i32 = -1;
    assert_eq!(
        egl_choose_config(dpy, core::ptr::null(), configs.as_mut_ptr(), 0, &mut zero),
        EGL_TRUE
    );
    assert_eq!(zero, 0, "config_size 0 writes zero configs");

    // A null `num_config` is required by the spec => EGL_BAD_PARAMETER, EGL_FALSE.
    let _ = egl_get_error();
    assert_eq!(
        egl_choose_config(
            dpy,
            core::ptr::null(),
            configs.as_mut_ptr(),
            4,
            core::ptr::null_mut()
        ),
        EGL_FALSE
    );
    assert_eq!(
        egl_get_error(),
        EGL_BAD_PARAMETER,
        "null num_config raises EGL_BAD_PARAMETER"
    );

    // eglGetConfigs enumerates ALL configs with the SAME contract (no attrib filter).
    let mut total: i32 = -1;
    assert_eq!(
        egl_get_configs(dpy, core::ptr::null_mut(), 0, &mut total),
        EGL_TRUE
    );
    assert_eq!(
        total,
        hl_gl::service::config::NUM_CONFIGS,
        "eglGetConfigs count-only reports the whole table"
    );
    let mut all: [*mut c_void; 2] = [core::ptr::null_mut(); 2];
    let mut got: i32 = -1;
    assert_eq!(
        egl_get_configs(dpy, all.as_mut_ptr(), all.len() as i32, &mut got),
        EGL_TRUE
    );
    // A capacity SMALLER than the table is a bounded copy, not an error.
    assert_eq!(got, all.len() as i32, "eglGetConfigs filled the array");
    assert!(all.iter().all(|config| !config.is_null()));
    let _ = egl_get_error();
    assert_eq!(
        egl_get_configs(dpy, all.as_mut_ptr(), 2, core::ptr::null_mut()),
        EGL_FALSE
    );
    assert_eq!(
        egl_get_error(),
        EGL_BAD_PARAMETER,
        "eglGetConfigs null num_config → EGL_BAD_PARAMETER"
    );
}

// ==================================================================================================
// 4) eglBindAPI / eglQueryAPI — scalar in/out, per-thread; a non-GLES API is rejected
// ==================================================================================================
