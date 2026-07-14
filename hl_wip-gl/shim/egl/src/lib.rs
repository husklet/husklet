//! Guest cdylib deployed as `libEGL.so.1` — the GLES2/EGL drop-in (the PRIMARY implementation object).
//!
//! A GLES app that links `-lEGL -lGLESv2` loads these `egl*` + `gl*` symbols as its driver. The exported
//! surface is code-generated from `registry/gles2_egl.manifest` (`build.rs`) so it can never drift from
//! the golden 402-entry set. The EGL lifecycle + the GLES core render path have real hand-written bodies
//! in [`driver`] that marshal the C ABI and call the `hl_gl` lowering services; GL is DEFERRED-lowering,
//! so `gl*` calls RECORD into a process-global [`GlContext`] ([`state`]) and `eglSwapBuffers` flushes the
//! frame IR through a [`hl_gpu::RemoteCommandSink`] over `$HL_GPU_EXEC`. The long tail are benign,
//! correct-ABI default stubs ([`stub`]) ported to real bodies incrementally without changing the surface.
//!
//! The soname `libEGL.so.1` is baked by `build.rs`; the thin `libGLESv2.so.2` forwarding stub the guest
//! also `DT_NEEDED`s resolves every `gl*` symbol back to this object.

// The generated + hand-written entry-point surface uses the GL/EGL C names verbatim (eglInitialize, …).
#![allow(non_snake_case)]

pub mod driver;
pub mod state;
pub mod stub;

// The generated C-ABI export surface: every `egl*`/`gl*` entry point not hand-written in `driver`.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported GLES2/EGL entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = GLES_EGL_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_complete_and_matches_the_census() {
        assert_eq!(TOTAL_ENTRYPOINTS, 402, "GLES2/EGL surface drifted from the golden 402");
        assert_eq!(GLES2_ENTRYPOINTS, 358);
        assert_eq!(EGL_ENTRYPOINTS, 44);
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_ENTRYPOINTS, TOTAL_ENTRYPOINTS);
    }
}
