//! EGL_EXT_device_query / device_base ABI — the path GDK's Wayland EGL bring-up drives (via libepoxy)
//! at startup, run against the staged `libEGL.so.1` through a `dlopen`, exactly as GTK4 does.
//!
//! GTK4 aborted in GDK's Wayland EGL bring-up with "No provider of eglQueryDisplayAttribEXT found.
//! Requires one of: EGL_EXT_device_base / EGL_EXT_device_query / …" because our libEGL neither exposed
//! `eglQueryDisplayAttribEXT` nor advertised the device extensions GDK queries. This test asserts:
//!   * `EGL_EXT_device_query` / `EGL_EXT_device_base` appear in the client AND per-display extension
//!     strings (GDK/epoxy check the string before resolving the entry points),
//!   * `eglGetProcAddress` resolves all four device entry points non-null (a caller invokes the returned
//!     pointer WITHOUT a null check — a null is a jump-through-null crash),
//!   * `eglQueryDevicesEXT` reports our single software device (count-only + bounded copy, no OOB store),
//!   * `eglQueryDisplayAttribEXT(EGL_DEVICE_EXT)` yields that device handle,
//!   * an unknown display attribute is `EGL_BAD_ATTRIBUTE` + `EGL_FALSE` (no deref / fabricated value),
//!   * a foreign `EGLDeviceEXT` handle is `EGL_BAD_DEVICE_EXT` (no deref crash).

#![cfg(target_os = "linux")]

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;

// ---- EGL enum values the caller passes (mirror the shim's constants) ----
const EGL_EXTENSIONS: i32 = 0x3055;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_TRUE: u32 = 1;
const EGL_FALSE: u32 = 0;
const EGL_SUCCESS: i32 = 0x3000;
const EGL_BAD_ATTRIBUTE: i32 = 0x3004;
const EGL_BAD_DEVICE_EXT: i32 = 0x322B;
const EGL_DEVICE_EXT: i32 = 0x322C;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}
const RTLD_NOW: c_int = 2;

fn stage_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the egl device-query test: {other}"),
    };
    PathBuf::from(home).join(".hl/gl").join(arch)
}

fn open(path: &PathBuf) -> *mut c_void {
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
    if h.is_null() {
        let e = unsafe { dlerror() };
        let msg = if e.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(e) }
                .to_string_lossy()
                .into_owned()
        };
        panic!("dlopen {} failed: {msg}", path.display());
    }
    h
}

fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    let p = unsafe { dlsym(handle, c.as_ptr()) };
    assert!(
        !p.is_null(),
        "symbol {name} not found in the dlopened object"
    );
    p
}

fn proc(get_proc: extern "C" fn(*const c_char) -> *mut c_void, name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    get_proc(c.as_ptr())
}

fn cstr(p: *const c_char) -> String {
    assert!(!p.is_null(), "EGL string getter returned null");
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn device_query_extension_end_to_end() {
    let dir = stage_dir();
    let egl_path = dir.join("libEGL.so.1");
    if !egl_path.exists() {
        // The x86_64 guest shim is skipped on a host without its rust std (see build.rs); nothing to drive.
        eprintln!(
            "staged libEGL missing at {} — skipping (guest std not installed)",
            dir.display()
        );
        return;
    }
    // Do not touch a live compositor from any surface path.
    std::env::set_var("HL_GL_NO_WAYLAND", "1");

    let egl = open(&egl_path);

    let egl_query_string: extern "C" fn(*mut c_void, i32) -> *const c_char =
        unsafe { std::mem::transmute(sym(egl, "eglQueryString")) };
    let egl_get_proc_address: extern "C" fn(*const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(sym(egl, "eglGetProcAddress")) };
    let egl_initialize: extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglInitialize")) };
    let egl_get_error: extern "C" fn() -> i32 =
        unsafe { std::mem::transmute(sym(egl, "eglGetError")) };

    // 1) The CLIENT extension string (EGL_NO_DISPLAY) advertises the device family GDK/epoxy require to
    //    resolve eglQueryDisplayAttribEXT before display init.
    let client_ext = cstr(egl_query_string(core::ptr::null_mut(), EGL_EXTENSIONS));
    assert!(
        client_ext.contains("EGL_EXT_device_query"),
        "client ext advertises device_query: {client_ext:?}"
    );
    assert!(
        client_ext.contains("EGL_EXT_device_base"),
        "client ext advertises device_base: {client_ext:?}"
    );

    // 2) REGRESSION: every device entry point the advertised extensions promise MUST resolve non-null — a
    //    caller (libepoxy) calls the returned pointer without a null check.
    for name in [
        "eglQueryDisplayAttribEXT",
        "eglQueryDeviceAttribEXT",
        "eglQueryDeviceStringEXT",
        "eglQueryDevicesEXT",
    ] {
        assert!(
            !proc(egl_get_proc_address, name).is_null(),
            "eglGetProcAddress({name}) must resolve"
        );
    }

    // 3) Bring up a display (surfaceless path) and confirm the per-DISPLAY string advertises the same set —
    //    GDK requires one of the device_* set on the initialized display.
    let get_platform_display_ext: extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void =
        unsafe { std::mem::transmute(proc(egl_get_proc_address, "eglGetPlatformDisplayEXT")) };
    let dpy = get_platform_display_ext(
        EGL_PLATFORM_SURFACELESS_MESA,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert!(
        !dpy.is_null(),
        "eglGetPlatformDisplayEXT(SURFACELESS) returns a display"
    );
    let (mut major, mut minor) = (0i32, 0i32);
    assert_eq!(
        egl_initialize(dpy, &mut major, &mut minor),
        EGL_TRUE,
        "display initializes"
    );
    let display_ext = cstr(egl_query_string(dpy, EGL_EXTENSIONS));
    assert!(
        display_ext.contains("EGL_EXT_device_query"),
        "display ext advertises device_query: {display_ext:?}"
    );
    assert!(
        display_ext.contains("EGL_EXT_device_base"),
        "display ext advertises device_base: {display_ext:?}"
    );

    // 4) eglQueryDevicesEXT contract — null array reports the count, then a bounded copy fills the array.
    let egl_query_devices: extern "C" fn(i32, *mut *mut c_void, *mut i32) -> u32 =
        unsafe { std::mem::transmute(proc(egl_get_proc_address, "eglQueryDevicesEXT")) };
    let mut count = -1i32;
    assert_eq!(
        egl_query_devices(0, core::ptr::null_mut(), &mut count),
        EGL_TRUE
    );
    assert!(
        count >= 1,
        "at least one device is reported (count={count})"
    );
    let mut buf: [*mut c_void; 4] = [core::ptr::null_mut(); 4];
    let mut got = -1i32;
    assert_eq!(
        egl_query_devices(buf.len() as i32, buf.as_mut_ptr(), &mut got),
        EGL_TRUE
    );
    assert_eq!(got, count, "the bounded copy returns every reported device");
    let device = buf[0];
    assert!(!device.is_null(), "the first device handle is written");
    // max_devices 0 with a non-null array is EGL_BAD_PARAMETER (no OOB store).
    let mut nope = -1i32;
    assert_eq!(
        egl_query_devices(0, buf.as_mut_ptr(), &mut nope),
        EGL_FALSE,
        "max_devices 0 + array is rejected"
    );

    // 5) eglQueryDisplayAttribEXT(EGL_DEVICE_EXT) yields exactly that device handle.
    let egl_query_display_attrib: extern "C" fn(*mut c_void, i32, *mut isize) -> u32 =
        unsafe { std::mem::transmute(proc(egl_get_proc_address, "eglQueryDisplayAttribEXT")) };
    let mut dev_attr: isize = 0;
    assert_eq!(
        egl_query_display_attrib(dpy, EGL_DEVICE_EXT, &mut dev_attr),
        EGL_TRUE,
        "EGL_DEVICE_EXT is answered"
    );
    assert_eq!(
        dev_attr as usize, device as usize,
        "the display's device == the enumerated device"
    );

    // 6) An UNKNOWN display attribute is EGL_BAD_ATTRIBUTE + EGL_FALSE (never a fabricated value / a deref).
    let _ = egl_get_error(); // drain the EGL_BAD_PARAMETER the step-4 rejection legitimately left pending
    assert_eq!(
        egl_get_error(),
        EGL_SUCCESS,
        "error register is clear before the bad query"
    );
    let mut junk: isize = -12345;
    assert_eq!(
        egl_query_display_attrib(dpy, 0x0BAD_BEEFu32 as i32, &mut junk),
        EGL_FALSE,
        "unknown display attribute fails cleanly"
    );
    assert_eq!(
        egl_get_error(),
        EGL_BAD_ATTRIBUTE,
        "and raises EGL_BAD_ATTRIBUTE"
    );

    // 7) eglQueryDeviceStringEXT(EGL_EXTENSIONS) is a valid (possibly empty) string for our device; a
    //    FOREIGN device handle is EGL_BAD_DEVICE_EXT + null (no deref crash).
    let egl_query_device_string: extern "C" fn(*mut c_void, i32) -> *const c_char =
        unsafe { std::mem::transmute(proc(egl_get_proc_address, "eglQueryDeviceStringEXT")) };
    let s = egl_query_device_string(device, EGL_EXTENSIONS);
    assert!(!s.is_null(), "device EGL_EXTENSIONS string is non-null");
    let _ = cstr(s); // valid UTF-8-ish, NUL-terminated
    let bogus = 0xDEAD_0000usize as *mut c_void;
    assert!(
        egl_query_device_string(bogus, EGL_EXTENSIONS).is_null(),
        "foreign device string is null"
    );
    assert_eq!(
        egl_get_error(),
        EGL_BAD_DEVICE_EXT,
        "foreign device raises EGL_BAD_DEVICE_EXT"
    );

    // 8) eglQueryDeviceAttribEXT on a foreign device is EGL_BAD_DEVICE_EXT (no deref crash).
    let egl_query_device_attrib: extern "C" fn(*mut c_void, i32, *mut isize) -> u32 =
        unsafe { std::mem::transmute(proc(egl_get_proc_address, "eglQueryDeviceAttribEXT")) };
    let mut v: isize = 0;
    assert_eq!(
        egl_query_device_attrib(bogus, EGL_DEVICE_EXT, &mut v),
        EGL_FALSE,
        "foreign device attrib fails"
    );
    assert_eq!(
        egl_get_error(),
        EGL_BAD_DEVICE_EXT,
        "foreign device attrib raises EGL_BAD_DEVICE_EXT"
    );
}
