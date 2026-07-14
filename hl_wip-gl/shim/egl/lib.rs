//! Guest cdylib: libEGL.so.1 — #[no_mangle] egl*/gl* C-ABI exports (the PRIMARY implementation object).
//! Thin trampolines forwarding to hl_gl::service bodies. Soname/version-script applied by build.rs.
//! A real GLES app links `-lEGL -lGLESv2` and runs UNMODIFIED against these symbols.
//! (was: hl-shim-gl/src/egl.rs + gles.rs exports.) DEFERRED this pass — small stub only.
#![allow(non_snake_case)]

/// e.g. eglSwapBuffers — forwards into the Rust driver's swap service.
#[no_mangle]
pub extern "C" fn eglSwapBuffers(_dpy: *mut core::ffi::c_void, _surface: *mut core::ffi::c_void) -> u32 {
    unimplemented!()
}
