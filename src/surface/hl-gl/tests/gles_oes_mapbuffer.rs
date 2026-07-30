//! `GL_OES_mapbuffer` entry points, driven against the staged shim through a real `dlopen`.
//!
//! The extension is the GLES 2 way to map a buffer: `glMapBufferOES(target, GL_WRITE_ONLY_OES)` maps the
//! whole buffer, `glUnmapBufferOES` releases it, and `glGetBufferPointerv(GL_BUFFER_MAP_POINTER)` reports the
//! live mapping. glmark2's `buffer` scene needs it. Extension functions are resolved through
//! `eglGetProcAddress` (they are not part of this driver's exported ABI census), which is how every real
//! GL loader finds them.

#![cfg(target_os = "linux")]

use core::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;

const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_TRUE: u32 = 1;
const EGL_NONE: i32 = 0x3038;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_STATIC_DRAW: u32 = 0x88E4;
const GL_MAP_READ_BIT: u32 = 0x0001;
const GL_MAP_WRITE_BIT: u32 = 0x0002;
const GL_WRITE_ONLY_OES: u32 = 0x88B9;
const GL_BUFFER_MAP_POINTER: u32 = 0x88BD;
const GL_NO_ERROR: u32 = 0;
const GL_INVALID_ENUM: u32 = 0x0500;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

fn stage_dir() -> PathBuf {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => panic!("unsupported host arch for the mapbuffer test: {other}"),
    };
    PathBuf::from(env!("HL_GL_STAGE_GL")).join(arch)
}

fn open(path: &PathBuf) -> *mut c_void {
    let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    assert!(!handle.is_null(), "dlopen {} failed", path.display());
    handle
}

fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = std::ffi::CString::new(name).unwrap();
    let p = unsafe { dlsym(handle, c.as_ptr()) };
    assert!(!p.is_null(), "symbol {name} not found");
    p
}

#[test]
fn oes_mapbuffer_maps_writes_and_reports_its_pointer() {
    let dir = stage_dir();
    let (egl_path, gles_path) = (dir.join("libEGL.so.1"), dir.join("libGLESv2.so.2"));
    if !egl_path.exists() || !gles_path.exists() {
        eprintln!(
            "staged shim missing under {} — skipping (guest std not installed)",
            dir.display()
        );
        return;
    }
    std::env::set_var("HL_GL_NO_WAYLAND", "1");
    let egl = open(&egl_path);
    let gles = open(&gles_path);

    let get_proc_address: extern "C" fn(*const c_char) -> *mut c_void =
        unsafe { std::mem::transmute(sym(egl, "eglGetProcAddress")) };
    let initialize: extern "C" fn(*mut c_void, *mut i32, *mut i32) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglInitialize")) };
    let create_context: extern "C" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const i32,
    ) -> *mut c_void = unsafe { std::mem::transmute(sym(egl, "eglCreateContext")) };
    let make_current: extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> u32 =
        unsafe { std::mem::transmute(sym(egl, "eglMakeCurrent")) };
    let gen_buffers: extern "C" fn(i32, *mut u32) =
        unsafe { std::mem::transmute(sym(gles, "glGenBuffers")) };
    let bind_buffer: extern "C" fn(u32, u32) =
        unsafe { std::mem::transmute(sym(gles, "glBindBuffer")) };
    let buffer_data: extern "C" fn(u32, isize, *const c_void, u32) =
        unsafe { std::mem::transmute(sym(gles, "glBufferData")) };
    let get_buffer_pointerv: extern "C" fn(u32, u32, *mut *mut c_void) =
        unsafe { std::mem::transmute(sym(gles, "glGetBufferPointerv")) };
    let get_error: extern "C" fn() -> u32 = unsafe { std::mem::transmute(sym(gles, "glGetError")) };

    let get_platform_display: extern "C" fn(u32, *mut c_void, *const i32) -> *mut c_void = unsafe {
        let name = std::ffi::CString::new("eglGetPlatformDisplayEXT").unwrap();
        std::mem::transmute(get_proc_address(name.as_ptr()))
    };
    let display = get_platform_display(
        EGL_PLATFORM_SURFACELESS_MESA,
        core::ptr::null_mut(),
        core::ptr::null(),
    );
    assert_eq!(initialize(display, &mut 0, &mut 0), EGL_TRUE);
    let attributes = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
    let context = create_context(
        display,
        core::ptr::null_mut(),
        core::ptr::null_mut(),
        attributes.as_ptr(),
    );
    assert_eq!(
        make_current(
            display,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            context
        ),
        EGL_TRUE
    );

    // The extension's two entry points must be resolvable — a loader calls the returned pointer directly.
    let resolve = |name: &str| {
        let c = std::ffi::CString::new(name).unwrap();
        let p = get_proc_address(c.as_ptr());
        assert!(!p.is_null(), "eglGetProcAddress({name}) must resolve");
        p
    };
    let map_buffer: extern "C" fn(u32, u32) -> *mut u8 =
        unsafe { std::mem::transmute(resolve("glMapBufferOES")) };
    let unmap_buffer: extern "C" fn(u32) -> u8 =
        unsafe { std::mem::transmute(resolve("glUnmapBufferOES")) };
    let map_range: extern "C" fn(u32, isize, isize, u32) -> *mut c_void =
        unsafe { std::mem::transmute(sym(gles, "glMapBufferRange")) };

    let mut buffer = 0u32;
    gen_buffers(1, &mut buffer);
    assert_ne!(buffer, 0);
    bind_buffer(GL_ARRAY_BUFFER, buffer);
    let source = [1u8, 2, 3, 4, 5, 6, 7, 8];
    buffer_data(
        GL_ARRAY_BUFFER,
        source.len() as isize,
        source.as_ptr().cast(),
        GL_STATIC_DRAW,
    );
    assert_eq!(get_error(), GL_NO_ERROR);

    // A non-GL_WRITE_ONLY_OES access is GL_INVALID_ENUM (the extension defines exactly one).
    assert!(map_buffer(GL_ARRAY_BUFFER, 0x88B8).is_null());
    assert_eq!(get_error(), GL_INVALID_ENUM);

    // The real map hands back a pointer INTO the buffer's storage holding the uploaded bytes.
    let mapped = map_buffer(GL_ARRAY_BUFFER, GL_WRITE_ONLY_OES);
    assert!(!mapped.is_null(), "glMapBufferOES returns a pointer");
    assert_eq!(get_error(), GL_NO_ERROR);
    let seen = unsafe { core::slice::from_raw_parts(mapped, source.len()) };
    assert_eq!(seen, &source, "the mapping exposes the uploaded bytes");
    unsafe { *mapped.add(0) = 0x42 };

    // GL_BUFFER_MAP_POINTER reports the live mapping (it reported null unconditionally before).
    let mut reported: *mut c_void = usize::MAX as *mut c_void;
    get_buffer_pointerv(GL_ARRAY_BUFFER, GL_BUFFER_MAP_POINTER, &mut reported);
    assert_eq!(
        reported,
        mapped.cast::<c_void>(),
        "GL_BUFFER_MAP_POINTER is the pointer glMapBufferOES returned"
    );
    assert_eq!(get_error(), GL_NO_ERROR);

    // An unknown pname is GL_INVALID_ENUM and writes null rather than a fabricated pointer.
    get_buffer_pointerv(GL_ARRAY_BUFFER, 0x0BAD, &mut reported);
    assert!(reported.is_null());
    assert_eq!(get_error(), GL_INVALID_ENUM);

    // Unmap clears the mapping. The flush it performs needs a host GPU service, which this process has no
    // access to, so only the mapping state is asserted here.
    let _ = unmap_buffer(GL_ARRAY_BUFFER);
    let mut after: *mut c_void = usize::MAX as *mut c_void;
    get_buffer_pointerv(GL_ARRAY_BUFFER, GL_BUFFER_MAP_POINTER, &mut after);
    assert!(after.is_null(), "an unmapped buffer reports a null pointer");

    // The ES3 core map over the same buffer: a whole-buffer GL_MAP_READ_BIT|GL_MAP_WRITE_BIT range is a
    // legal mapping and must hand back a pointer, not null. The one precondition is a CURRENT context —
    // every GL entry point resolves against the calling thread's share group and answers its zero value
    // when there is none, which is what makes this call look like it returns null on its own.
    let mapped_range = map_range(
        GL_ARRAY_BUFFER,
        0,
        source.len() as isize,
        GL_MAP_READ_BIT | GL_MAP_WRITE_BIT,
    );
    assert!(
        !mapped_range.is_null(),
        "glMapBufferRange over the whole buffer with READ|WRITE returns a pointer"
    );
    assert_eq!(get_error(), GL_NO_ERROR);
    let _ = unmap_buffer(GL_ARRAY_BUFFER);
}
