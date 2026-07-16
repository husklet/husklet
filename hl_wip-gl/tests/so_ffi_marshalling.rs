//! `.so`-integration **marshalling** coverage — the second half of the C-ABI boundary the in-process
//! `hl_gl` service tests cannot reach, complementing `tests/so_ffi_coverage.rs`. Where that file drives the
//! `glGet*` / `glIs*` getter families, this one pins the EGL config/query/API entry points and the GLES
//! array-in draw + buffer-upload path — the exact symbols libepoxy / ANGLE / GTK resolve out of the staged
//! `libEGL.so.1` + `libGLESv2.so.2` via `dlopen`, marshalled through their real C signatures:
//!
//!   * STRING RETURNS         — `eglQueryString` (vendor / version / client-APIs / client+display EXTENSIONS)
//!   * POINTER-OUT + errors   — `eglGetConfigAttrib` (real 8/8/8/8 + 24/8 sizes; BAD_CONFIG / BAD_ATTRIBUTE
//!                              / BAD_PARAMETER without writing `value`)
//!   * IN-OUT arrays + count  — `eglChooseConfig` / `eglGetConfigs` (attrib-list in; `configs[]` + `num`
//!                              out; null-array count-only; bounded copy; null-`num` → BAD_PARAMETER)
//!   * SCALAR in/out          — `eglBindAPI` / `eglQueryAPI` (per-thread API bind + read-back; a non-GLES
//!                              API → EGL_FALSE + BAD_PARAMETER)
//!   * FUNCTION-PTR return    — `eglGetProcAddress` (a core name resolves to a *callable* pointer that
//!                              returns the right value; an unknown name / null → null)
//!   * ARRAY-IN contents      — `glBufferData` + `glBufferSubData` ptr+size upload, read back BYTE-FOR-BYTE
//!                              through `glMapBufferRange`'s host-storage pointer
//!   * ARRAY-IN draw path     — `glVertexAttribPointer` + `glDrawArrays` / `glDrawElements` over both
//!                              VBO-backed and CLIENT-side vertex/index arrays (the shim reads guest memory
//!                              through the marshalled pointer), plus their `GL_INVALID_*` error paths
//!   * POINTER-OUT (pixels)   — `glReadPixels` argument + error-path marshalling (7 args; bad enum / value)
//!   * POINTER-OUT (attrib)   — `glGetVertexAttrib{i,f}v` null-safe out-param stores
//!
//! Loading / serialization / arch-skip mirror `so_ffi_coverage.rs` exactly (see that file's header): libEGL
//! FIRST with RTLD_GLOBAL so `libGLESv2`'s `DT_NEEDED` binds it and `hl_shim_state_ptr` resolves; every test
//! grabs `SERIAL` because the gl*/egl* objects share ONE process-global `State`.

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::Mutex;

// ---- dynamic-loader FFI ---------------------------------------------------------------------------
extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

/// Serialize every test — they all drive the ONE process-global GL `State`.
static SERIAL: Mutex<()> = Mutex::new(());

// ==================================================================================================
// GL / EGL enum values a C caller passes raw (mirror the shim's constants)
// ==================================================================================================
const GL_NO_ERROR: u32 = 0;
const GL_INVALID_ENUM: u32 = 0x0500;
const GL_INVALID_VALUE: u32 = 0x0501;
const GL_INVALID_OPERATION: u32 = 0x0502;

const GL_VENDOR: u32 = 0x1F00;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const GL_STATIC_DRAW: u32 = 0x88E4;
const GL_MAP_READ_BIT: u32 = 0x0001;
const GL_FLOAT: u32 = 0x1406;
const GL_TRIANGLES: u32 = 0x0004;
const GL_UNSIGNED_SHORT: u32 = 0x1403;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_RGBA: u32 = 0x1908;
const GL_VERTEX_ATTRIB_ARRAY_ENABLED: u32 = 0x8622;

// EGL config attributes.
const EGL_BUFFER_SIZE: i32 = 0x3020;
const EGL_ALPHA_SIZE: i32 = 0x3021;
const EGL_BLUE_SIZE: i32 = 0x3022;
const EGL_GREEN_SIZE: i32 = 0x3023;
const EGL_RED_SIZE: i32 = 0x3024;
const EGL_DEPTH_SIZE: i32 = 0x3025;
const EGL_STENCIL_SIZE: i32 = 0x3026;
const EGL_CONFIG_ID: i32 = 0x3028;
const EGL_SURFACE_TYPE: i32 = 0x3033;
const EGL_NONE: i32 = 0x3038;
const EGL_COLOR_BUFFER_TYPE: i32 = 0x303F;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_RGB_BUFFER: i32 = 0x308E;
const EGL_WINDOW_BIT: i32 = 0x0004;
const EGL_PBUFFER_BIT: i32 = 0x0001;
const EGL_OPENGL_ES2_BIT: i32 = 0x0004;
const EGL_OPENGL_ES3_BIT: i32 = 0x0040;

// EGL query-string names.
const EGL_VENDOR_Q: i32 = 0x3053;
const EGL_VERSION_Q: i32 = 0x3054;
const EGL_EXTENSIONS_Q: i32 = 0x3055;
const EGL_CLIENT_APIS_Q: i32 = 0x308D;

// EGL API enums + errors.
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_OPENVG_API: u32 = 0x30A1;
const EGL_OPENGL_API: u32 = 0x30A2;
const EGL_SUCCESS: i32 = 0x3000;
const EGL_BAD_ATTRIBUTE: i32 = 0x3004;
const EGL_BAD_CONFIG: i32 = 0x3005;
const EGL_BAD_PARAMETER: i32 = 0x300C;
const EGL_TRUE: u32 = 1;
const EGL_FALSE: u32 = 0;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;

// ==================================================================================================
// loader helpers (identical contract to so_ffi_coverage.rs)
// ==================================================================================================
fn stage_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the so-ffi test: {other}"),
    };
    PathBuf::from(home).join(".hl/gl").join(arch)
}

fn dlopen_global(path: &PathBuf) -> *mut c_void {
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    if h.is_null() {
        let e = unsafe { dlerror() };
        let msg = if e.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(e) }.to_string_lossy().into_owned()
        };
        panic!("dlopen {} failed: {msg}", path.display());
    }
    h
}

fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    let p = unsafe { dlsym(handle, c.as_ptr()) };
    assert!(!p.is_null(), "symbol {name} not resolvable in the dlopened object (a real ABI gap)");
    p
}

macro_rules! f {
    ($h:expr, $name:literal, $ty:ty) => {
        unsafe { core::mem::transmute::<*mut c_void, $ty>(sym($h, $name)) }
    };
}

struct Shim {
    gles: *mut c_void,
    egl: *mut c_void,
}

fn load() -> Option<Shim> {
    let dir = stage_dir();
    let egl_path = dir.join("libEGL.so.1");
    let gles_path = dir.join("libGLESv2.so.2");
    if !egl_path.exists() || !gles_path.exists() {
        eprintln!("staged shim missing under {} — skipping (guest std not installed)", dir.display());
        return None;
    }
    std::env::set_var("HL_GL_NO_WAYLAND", "1");
    let egl = dlopen_global(&egl_path);
    let gles = dlopen_global(&gles_path);
    // Drain both read-and-clear error registers to a clean slate (run order is nondeterministic).
    f!(gles, "glGetError", extern "C" fn() -> u32)();
    f!(egl, "eglGetError", extern "C" fn() -> i32)();
    Some(Shim { gles, egl })
}

fn cstr(p: *const c_char) -> String {
    assert!(!p.is_null(), "an EGL/GL string getter returned null (an app dereferences it unconditionally)");
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Bring up the surfaceless display (the path eglinfo / egl_surfaceless_config.rs drives) + initialize it.
fn surfaceless_display(sh: &Shim) -> *mut c_void {
    let egl_get_proc = f!(sh.egl, "eglGetProcAddress", extern "C" fn(*const c_char) -> *mut c_void);
    let egl_initialize =
        f!(sh.egl, "eglInitialize", extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32);
    let get_platform_display: extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void = unsafe {
        let c = std::ffi::CString::new("eglGetPlatformDisplayEXT").unwrap();
        let p = egl_get_proc(c.as_ptr());
        assert!(!p.is_null(), "eglGetProcAddress(eglGetPlatformDisplayEXT) resolves");
        core::mem::transmute(p)
    };
    let dpy =
        get_platform_display(EGL_PLATFORM_SURFACELESS_MESA, core::ptr::null_mut(), core::ptr::null());
    assert!(!dpy.is_null(), "surfaceless EGLDisplay is non-null");
    assert_eq!(egl_initialize(dpy, &mut 0, &mut 0), EGL_TRUE, "eglInitialize succeeds");
    dpy
}

// ==================================================================================================
// 1) eglQueryString — string returns (vendor / version / client-APIs / client & display EXTENSIONS)
// ==================================================================================================
#[test]
fn egl_query_string_returns_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let egl_query_string = f!(sh.egl, "eglQueryString", extern "C" fn(*mut c_void, i32) -> *const c_char);

    // With EGL_NO_DISPLAY (null) the vendor/version/client-API identity strings are the driver's fixed ids.
    let nodpy = core::ptr::null_mut();
    assert_eq!(cstr(egl_query_string(nodpy, EGL_VENDOR_Q)), "hl-gl");
    assert_eq!(cstr(egl_query_string(nodpy, EGL_VERSION_Q)), "1.5 hl-gl");
    assert_eq!(cstr(egl_query_string(nodpy, EGL_CLIENT_APIS_Q)), "OpenGL_ES");

    // A null display => CLIENT extensions: the platform-base + wayland-platform set a toolkit probes BEFORE
    // opening a display. Advertising EGL_EXT_platform_wayland is what routes a Wayland app to the window path.
    let client_ext = cstr(egl_query_string(nodpy, EGL_EXTENSIONS_Q));
    assert!(client_ext.contains("EGL_EXT_platform_base"), "client ext advertises platform_base: {client_ext:?}");
    assert!(client_ext.contains("EGL_EXT_platform_wayland"), "client ext advertises platform_wayland");
    assert!(client_ext.is_ascii(), "the extension string is plain ASCII");

    // An unrecognized name is the spec-legal empty (non-null, so an app's strlen/strstr is safe).
    let unknown = egl_query_string(nodpy, 0xBEEF);
    assert!(!unknown.is_null(), "an unknown eglQueryString name returns \"\" (non-null), never null");
    assert_eq!(cstr(unknown), "");

    // A real (initialized) display => the per-DISPLAY set, which advertises the context extensions
    // (distinct from the client set — proving the string is keyed on the display argument, not constant).
    let dpy = surfaceless_display(&sh);
    let disp_ext = cstr(egl_query_string(dpy, EGL_EXTENSIONS_Q));
    assert!(disp_ext.contains("EGL_KHR_create_context"), "display ext advertises create_context: {disp_ext:?}");
    assert_ne!(disp_ext, client_ext, "display and client extension strings differ");
}

// ==================================================================================================
// 2) eglGetConfigAttrib — pointer-out with the driver's REAL config attributes + error paths
// ==================================================================================================
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

    // Select the driver's single config (match-all: null attrib list).
    let mut cfg: *mut c_void = core::ptr::null_mut();
    let mut n: i32 = -1;
    assert_eq!(egl_choose_config(dpy, core::ptr::null(), &mut cfg, 1, &mut n), EGL_TRUE);
    assert_eq!(n, 1, "the driver advertises exactly one config");
    assert!(!cfg.is_null(), "the selected EGLConfig handle is non-null");

    // Every attribute reads back its truthful value into the caller's `*value` out-param.
    let attr = |a: i32| {
        let mut v: i32 = i32::MIN;
        assert_eq!(egl_get_config_attrib(dpy, cfg, a, &mut v), EGL_TRUE, "attr {a:#x} succeeds");
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
    assert_eq!(attr(EGL_RENDERABLE_TYPE), EGL_OPENGL_ES2_BIT | EGL_OPENGL_ES3_BIT, "ES2|ES3");
    assert_eq!(attr(EGL_SURFACE_TYPE), EGL_WINDOW_BIT | EGL_PBUFFER_BIT, "window|pbuffer");

    // A foreign config handle → EGL_BAD_CONFIG, EGL_FALSE, and `value` is left UNTOUCHED (never a deref of
    // the unknown handle, never a fabricated 0).
    let _ = egl_get_error();
    let mut sentinel: i32 = 0x5A5A_5A5A;
    let bogus = 0xDEAD_0000usize as *mut c_void;
    assert_eq!(egl_get_config_attrib(dpy, bogus, EGL_RED_SIZE, &mut sentinel), EGL_FALSE);
    assert_eq!(egl_get_error(), EGL_BAD_CONFIG, "a foreign config raises EGL_BAD_CONFIG");
    assert_eq!(sentinel, 0x5A5A_5A5A, "a rejected query does not write `value`");

    // An unrecognized attribute on OUR config → EGL_BAD_ATTRIBUTE (not a silent 0).
    assert_eq!(egl_get_config_attrib(dpy, cfg, 0x1234, &mut sentinel), EGL_FALSE);
    assert_eq!(egl_get_error(), EGL_BAD_ATTRIBUTE, "an unknown attribute raises EGL_BAD_ATTRIBUTE");
    assert_eq!(sentinel, 0x5A5A_5A5A);

    // A null `value` out-param → EGL_BAD_PARAMETER without a deref.
    assert_eq!(egl_get_config_attrib(dpy, cfg, EGL_RED_SIZE, core::ptr::null_mut()), EGL_FALSE);
    assert_eq!(egl_get_error(), EGL_BAD_PARAMETER, "a null value ptr raises EGL_BAD_PARAMETER");
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
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT, EGL_NONE,
    ];

    // Count-only query: a NULL `configs` array returns the total available in `num_config`, writes nothing.
    let mut count: i32 = -1;
    assert_eq!(
        egl_choose_config(dpy, attribs.as_ptr(), core::ptr::null_mut(), 0, &mut count),
        EGL_TRUE
    );
    assert_eq!(count, 1, "count-only eglChooseConfig reports the total (1)");

    // Real array: fill up to config_size handles and report how many were written. A sentinel slot proves
    // the bounded copy did not overrun (only slot 0 is written).
    let mut configs: [*mut c_void; 4] = [0xF00Dusize as *mut c_void; 4];
    let mut num: i32 = -1;
    assert_eq!(
        egl_choose_config(dpy, attribs.as_ptr(), configs.as_mut_ptr(), configs.len() as i32, &mut num),
        EGL_TRUE
    );
    assert_eq!(num, 1, "one config written");
    assert!(!configs[0].is_null(), "slot 0 holds a real EGLConfig handle");
    assert_eq!(configs[1], 0xF00Dusize as *mut c_void, "slot 1 is untouched (bounded copy)");
    // The written handle is a real one: eglGetConfigAttrib(EGL_RED_SIZE) reads 8 through it.
    let mut red: i32 = -1;
    assert_eq!(egl_get_config_attrib(dpy, configs[0], EGL_RED_SIZE, &mut red), EGL_TRUE);
    assert_eq!(red, 8, "the enumerated config is the real 8-bit-red config");

    // config_size 0 with a real array writes nothing and reports 0 (a legal "give me none").
    let mut zero: i32 = -1;
    assert_eq!(egl_choose_config(dpy, core::ptr::null(), configs.as_mut_ptr(), 0, &mut zero), EGL_TRUE);
    assert_eq!(zero, 0, "config_size 0 writes zero configs");

    // A null `num_config` is required by the spec => EGL_BAD_PARAMETER, EGL_FALSE.
    let _ = egl_get_error();
    assert_eq!(
        egl_choose_config(dpy, core::ptr::null(), configs.as_mut_ptr(), 4, core::ptr::null_mut()),
        EGL_FALSE
    );
    assert_eq!(egl_get_error(), EGL_BAD_PARAMETER, "null num_config raises EGL_BAD_PARAMETER");

    // eglGetConfigs enumerates ALL configs with the SAME contract (no attrib filter).
    let mut total: i32 = -1;
    assert_eq!(egl_get_configs(dpy, core::ptr::null_mut(), 0, &mut total), EGL_TRUE);
    assert_eq!(total, 1, "eglGetConfigs count-only total is 1");
    let mut all: [*mut c_void; 2] = [core::ptr::null_mut(); 2];
    let mut got: i32 = -1;
    assert_eq!(egl_get_configs(dpy, all.as_mut_ptr(), all.len() as i32, &mut got), EGL_TRUE);
    assert_eq!(got, 1, "eglGetConfigs wrote one handle");
    assert!(!all[0].is_null());
    let _ = egl_get_error();
    assert_eq!(egl_get_configs(dpy, all.as_mut_ptr(), 2, core::ptr::null_mut()), EGL_FALSE);
    assert_eq!(egl_get_error(), EGL_BAD_PARAMETER, "eglGetConfigs null num_config → EGL_BAD_PARAMETER");
}

// ==================================================================================================
// 4) eglBindAPI / eglQueryAPI — scalar in/out, per-thread; a non-GLES API is rejected
// ==================================================================================================
#[test]
fn egl_bind_and_query_api_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let egl_bind_api = f!(sh.egl, "eglBindAPI", extern "C" fn(u32) -> u32);
    let egl_query_api = f!(sh.egl, "eglQueryAPI", extern "C" fn() -> u32);
    let egl_get_error = f!(sh.egl, "eglGetError", extern "C" fn() -> i32);

    // The default bound API is GLES (the only API this driver serves).
    assert_eq!(egl_query_api(), EGL_OPENGL_ES_API, "default bound API is EGL_OPENGL_ES_API");

    // Binding GLES succeeds and reads back.
    assert_eq!(egl_bind_api(EGL_OPENGL_ES_API), EGL_TRUE);
    assert_eq!(egl_query_api(), EGL_OPENGL_ES_API);

    // A non-GLES API is EGL_FALSE + EGL_BAD_PARAMETER, and the bound API is unchanged (never silently taken).
    let _ = egl_get_error();
    assert_eq!(egl_bind_api(EGL_OPENGL_API), EGL_FALSE, "EGL_OPENGL_API is not served");
    assert_eq!(egl_get_error(), EGL_BAD_PARAMETER);
    assert_eq!(egl_bind_api(EGL_OPENVG_API), EGL_FALSE, "EGL_OPENVG_API is not served");
    assert_eq!(egl_get_error(), EGL_BAD_PARAMETER);
    assert_eq!(egl_query_api(), EGL_OPENGL_ES_API, "a rejected bind leaves the API unchanged");
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

    let egl_get_proc = f!(sh.egl, "eglGetProcAddress", extern "C" fn(*const c_char) -> *mut c_void);
    let get = |name: &str| {
        let c = std::ffi::CString::new(name).unwrap();
        egl_get_proc(c.as_ptr())
    };

    // Core egl*/gl* names all resolve to a non-null pointer (a null for a core name would crash an app that
    // loads its entry points through eglGetProcAddress instead of the dynamic linker).
    for name in ["eglGetError", "eglInitialize", "eglChooseConfig", "glClear", "glDrawArrays", "glGetString"] {
        assert!(!get(name).is_null(), "eglGetProcAddress({name}) resolves a core entry point");
    }

    // The resolved pointer is not merely non-null — it is CALLABLE and behaves. `glGetString` resolved out
    // of libEGL shares the process-global State, so calling it returns the driver's vendor id.
    let gl_get_string: extern "C" fn(u32) -> *const c_char =
        unsafe { core::mem::transmute(get("glGetString")) };
    assert_eq!(cstr(gl_get_string(GL_VENDOR)), "hl-gl", "the resolved glGetString is callable + correct");

    // `eglGetError` resolved through the trampoline agrees with the directly-linked symbol (same function).
    let egl_get_error_via_proc: extern "C" fn() -> i32 =
        unsafe { core::mem::transmute(get("eglGetError")) };
    let egl_get_error = f!(sh.egl, "eglGetError", extern "C" fn() -> i32);
    let _ = egl_get_error();
    assert_eq!(egl_get_error_via_proc(), EGL_SUCCESS, "the resolved eglGetError reads the clean state");

    // An unknown / null name is the spec-legal null.
    assert!(get("glThisIsNotARealEntryPoint").is_null(), "an unadvertised name resolves to null");
    assert!(egl_get_proc(core::ptr::null()).is_null(), "a null procname resolves to null (no deref)");
}

// ==================================================================================================
// 6) Array-IN buffer upload — glBufferData / glBufferSubData ptr+size, read back BYTE-FOR-BYTE via the
//    host-storage pointer glMapBufferRange hands out.
// ==================================================================================================
#[test]
fn gl_buffer_upload_contents_marshal_through_map() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_gen_buffers = f!(sh.gles, "glGenBuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_buffer = f!(sh.gles, "glBindBuffer", extern "C" fn(u32, u32));
    let gl_buffer_data = f!(sh.gles, "glBufferData", extern "C" fn(u32, isize, *const c_void, u32));
    let gl_buffer_sub_data =
        f!(sh.gles, "glBufferSubData", extern "C" fn(u32, isize, isize, *const c_void));
    let gl_map_buffer_range =
        f!(sh.gles, "glMapBufferRange", extern "C" fn(u32, isize, isize, u32) -> *mut c_void);
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);

    let mut buf: u32 = 0;
    gl_gen_buffers(1, &mut buf);
    assert!(buf != 0);
    gl_bind_buffer(GL_ARRAY_BUFFER, buf);

    // glBufferData: a 64-byte array-in upload (distinct byte pattern so a wrong offset/size shows up).
    let src: Vec<u8> = (0u16..64).map(|i| (i.wrapping_mul(7) & 0xFF) as u8).collect();
    gl_buffer_data(GL_ARRAY_BUFFER, src.len() as isize, src.as_ptr() as *const c_void, GL_STATIC_DRAW);
    assert_eq!(gl_get_error(), GL_NO_ERROR);

    // glBufferSubData: overwrite bytes [16,24) with a second array-in payload.
    let patch: [u8; 8] = [0xAA, 0xBB, 0xCC, 0xDD, 0x11, 0x22, 0x33, 0x44];
    gl_buffer_sub_data(GL_ARRAY_BUFFER, 16, patch.len() as isize, patch.as_ptr() as *const c_void);
    assert_eq!(gl_get_error(), GL_NO_ERROR);

    // glMapBufferRange returns a pointer INTO the buffer's host storage; read the whole 64 bytes back.
    let p = gl_map_buffer_range(GL_ARRAY_BUFFER, 0, 64, GL_MAP_READ_BIT);
    assert!(!p.is_null(), "glMapBufferRange returns a non-null host pointer");
    let mapped = unsafe { std::slice::from_raw_parts(p as *const u8, 64) };
    let mut expect = src.clone();
    expect[16..24].copy_from_slice(&patch);
    assert_eq!(mapped, &expect[..], "glBufferData + glBufferSubData contents marshalled byte-for-byte");

    // Error paths on the SAME entry point (out-of-range map + unbound target).
    let _ = gl_get_error();
    assert!(gl_map_buffer_range(GL_ARRAY_BUFFER, 0, 128, GL_MAP_READ_BIT).is_null(), "over-size map fails");
    assert_eq!(gl_get_error(), GL_INVALID_VALUE, "offset+length > size → GL_INVALID_VALUE");
    gl_bind_buffer(GL_ARRAY_BUFFER, 0);
    let _ = gl_get_error();
    assert!(gl_map_buffer_range(GL_ARRAY_BUFFER, 0, 8, GL_MAP_READ_BIT).is_null(), "no bound buffer → null");
    assert_eq!(gl_get_error(), GL_INVALID_OPERATION, "mapping with no bound buffer → GL_INVALID_OPERATION");
}

// ==================================================================================================
// 7) Array-IN draw path — glVertexAttribPointer + glDrawArrays / glDrawElements over VBO-backed AND
//    client-side vertex/index arrays (the shim reads guest memory through the marshalled pointer), plus
//    their GL_INVALID_* error paths. Recording succeeds without a live executor (IR lowers at swap).
// ==================================================================================================
#[test]
fn gl_draw_array_in_paths_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_gen_buffers = f!(sh.gles, "glGenBuffers", extern "C" fn(i32, *mut u32));
    let gl_bind_buffer = f!(sh.gles, "glBindBuffer", extern "C" fn(u32, u32));
    let gl_buffer_data = f!(sh.gles, "glBufferData", extern "C" fn(u32, isize, *const c_void, u32));
    let gl_gen_vertex_arrays = f!(sh.gles, "glGenVertexArrays", extern "C" fn(i32, *mut u32));
    let gl_bind_vertex_array = f!(sh.gles, "glBindVertexArray", extern "C" fn(u32));
    let gl_vertex_attrib_pointer = f!(
        sh.gles,
        "glVertexAttribPointer",
        extern "C" fn(u32, i32, u32, u8, i32, *const c_void)
    );
    let gl_enable_vaa = f!(sh.gles, "glEnableVertexAttribArray", extern "C" fn(u32));
    let gl_draw_arrays = f!(sh.gles, "glDrawArrays", extern "C" fn(u32, i32, i32));
    let gl_draw_elements =
        f!(sh.gles, "glDrawElements", extern "C" fn(u32, i32, u32, *const c_void));
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);

    // GLES3 requires a bound VAO to draw.
    let mut vao: u32 = 0;
    gl_gen_vertex_arrays(1, &mut vao);
    gl_bind_vertex_array(vao);

    // ---- VBO-backed vertex array: glVertexAttribPointer's `pointer` is a byte OFFSET (0), not a client
    //      pointer, so the draw records cleanly against the bound buffer.
    let mut vbo: u32 = 0;
    gl_gen_buffers(1, &mut vbo);
    gl_bind_buffer(GL_ARRAY_BUFFER, vbo);
    // 3 vertices × vec2 f32 = 24 bytes.
    let verts: [f32; 6] = [-0.8, -0.8, 0.8, -0.8, 0.0, 0.8];
    gl_buffer_data(GL_ARRAY_BUFFER, 24, verts.as_ptr() as *const c_void, GL_STATIC_DRAW);
    gl_vertex_attrib_pointer(0, 2, GL_FLOAT, 0, 8, core::ptr::null());
    gl_enable_vaa(0);
    let _ = gl_get_error();
    gl_draw_arrays(GL_TRIANGLES, 0, 3);
    assert_eq!(gl_get_error(), GL_NO_ERROR, "a valid VBO-backed glDrawArrays records without error");

    // ---- VBO-backed index array: glDrawElements' `indices` is a byte OFFSET into the bound element buffer.
    let mut ebo: u32 = 0;
    gl_gen_buffers(1, &mut ebo);
    gl_bind_buffer(GL_ELEMENT_ARRAY_BUFFER, ebo);
    let idx: [u16; 3] = [0, 1, 2];
    gl_buffer_data(GL_ELEMENT_ARRAY_BUFFER, 6, idx.as_ptr() as *const c_void, GL_STATIC_DRAW);
    let _ = gl_get_error();
    gl_draw_elements(GL_TRIANGLES, 3, GL_UNSIGNED_SHORT, core::ptr::null());
    assert_eq!(gl_get_error(), GL_NO_ERROR, "a valid VBO-backed glDrawElements records without error");

    // ---- CLIENT-side arrays: no bound buffer, so the shim reads guest memory THROUGH the marshalled
    //      pointer (the ABI path GTK's client-array draws take). A real Rust array backs each pointer.
    gl_bind_buffer(GL_ARRAY_BUFFER, 0);
    gl_bind_buffer(GL_ELEMENT_ARRAY_BUFFER, 0);
    let client_verts: [f32; 6] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0];
    gl_vertex_attrib_pointer(0, 2, GL_FLOAT, 0, 8, client_verts.as_ptr() as *const c_void);
    gl_enable_vaa(0);
    let client_idx: [u16; 3] = [0, 1, 2];
    let _ = gl_get_error();
    gl_draw_elements(GL_TRIANGLES, 3, GL_UNSIGNED_SHORT, client_idx.as_ptr() as *const c_void);
    assert_eq!(gl_get_error(), GL_NO_ERROR, "a client-array glDrawElements reads guest memory + records");
    gl_draw_arrays(GL_TRIANGLES, 0, 3);
    assert_eq!(gl_get_error(), GL_NO_ERROR, "a client-array glDrawArrays reads guest memory + records");

    // ---- Error-path marshalling on the draw entry points.
    let _ = gl_get_error();
    gl_draw_arrays(GL_TRIANGLES, 0, -1);
    assert_eq!(gl_get_error(), GL_INVALID_VALUE, "a negative glDrawArrays count → GL_INVALID_VALUE");
    gl_draw_elements(GL_TRIANGLES, -1, GL_UNSIGNED_SHORT, core::ptr::null());
    assert_eq!(gl_get_error(), GL_INVALID_VALUE, "a negative glDrawElements count → GL_INVALID_VALUE");
}

// ==================================================================================================
// 8) glReadPixels + glGetVertexAttrib{i,f}v — pointer-out argument + error-path marshalling
// ==================================================================================================
#[test]
fn gl_readpixels_and_vertex_attrib_getters_marshal() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(sh) = load() else { return };

    let gl_read_pixels = f!(
        sh.gles,
        "glReadPixels",
        extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void)
    );
    let gl_get_vertex_attribiv =
        f!(sh.gles, "glGetVertexAttribiv", extern "C" fn(u32, u32, *mut i32));
    let gl_get_vertex_attribfv =
        f!(sh.gles, "glGetVertexAttribfv", extern "C" fn(u32, u32, *mut f32));
    let gl_get_error = f!(sh.gles, "glGetError", extern "C" fn() -> u32);

    // glReadPixels error-path marshalling (these branches validate BEFORE touching the sink, so they are
    // executor-independent): a non-UNSIGNED_BYTE type and a bad format are GL_INVALID_ENUM; negative extent
    // is GL_INVALID_VALUE; a null client pointer (no PBO bound) is GL_INVALID_VALUE.
    let mut px = [0u8; 16];
    let _ = gl_get_error();
    gl_read_pixels(0, 0, 2, 2, GL_RGBA, GL_FLOAT, px.as_mut_ptr() as *mut c_void);
    assert_eq!(gl_get_error(), GL_INVALID_ENUM, "a non-UNSIGNED_BYTE type → GL_INVALID_ENUM");
    gl_read_pixels(0, 0, 2, 2, 0xBEEF, GL_UNSIGNED_BYTE, px.as_mut_ptr() as *mut c_void);
    assert_eq!(gl_get_error(), GL_INVALID_ENUM, "an unknown format → GL_INVALID_ENUM");
    gl_read_pixels(0, 0, -1, 2, GL_RGBA, GL_UNSIGNED_BYTE, px.as_mut_ptr() as *mut c_void);
    assert_eq!(gl_get_error(), GL_INVALID_VALUE, "a negative width → GL_INVALID_VALUE");
    gl_read_pixels(0, 0, 2, 2, GL_RGBA, GL_UNSIGNED_BYTE, core::ptr::null_mut());
    assert_eq!(gl_get_error(), GL_INVALID_VALUE, "a null pixels ptr (no PBO) → GL_INVALID_VALUE");
    // A zero-area read is a spec-legal no-op that writes nothing and raises no error.
    gl_read_pixels(0, 0, 0, 0, GL_RGBA, GL_UNSIGNED_BYTE, px.as_mut_ptr() as *mut c_void);
    assert_eq!(gl_get_error(), GL_NO_ERROR, "a zero-area glReadPixels is a no-op");

    // glGetVertexAttrib{i,f}v: null-safe out-param stores (the reference-model default is 0), and the
    // sentinel is overwritten with 0 for a non-null pointer.
    let mut iv: i32 = 0x7EAD;
    gl_get_vertex_attribiv(0, GL_VERTEX_ATTRIB_ARRAY_ENABLED, &mut iv);
    assert_eq!(iv, 0, "glGetVertexAttribiv writes the modeled 0 default");
    let mut fv: f32 = -12.5;
    gl_get_vertex_attribfv(0, GL_VERTEX_ATTRIB_ARRAY_ENABLED, &mut fv);
    assert_eq!(fv, 0.0, "glGetVertexAttribfv writes the modeled 0.0 default");
    // Null out-params are ignored without a deref.
    gl_get_vertex_attribiv(0, GL_VERTEX_ATTRIB_ARRAY_ENABLED, core::ptr::null_mut());
    gl_get_vertex_attribfv(0, GL_VERTEX_ATTRIB_ARRAY_ENABLED, core::ptr::null_mut());
    assert_eq!(gl_get_error(), GL_NO_ERROR, "null-safe attribute getters raise no error");
}
