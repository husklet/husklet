//! Guest cdylib source for BOTH GL/EGL shim objects — this ONE crate is cross-built TWICE (selected by
//! `$HL_SHIM_ROLE`, see the crate `build.rs`), matching real Mesa's split:
//!   * role `egl`  → `libEGL.so.1` exports the `egl*` set (44) + the shared-state accessor
//!     `hl_shim_state_ptr`; it OWNS the process-global [`GlContext`] ([`state`]).
//!   * role `gles` → `libGLESv2.so.2` exports the `gl*` set (422) in its OWN dynsym; built
//!     `cfg(gles_client)`, it has NO state of its own and `DT_NEEDED`s libEGL to import the accessor.
//! A version script pins each object's exported surface; the union covers the whole 466-entry set with no
//! duplication. A GLES app links `-lEGL -lGLESv2` and gets its `egl*` from libEGL, its `gl*` from
//! libGLESv2, and both drive the SAME context (glDraw records; eglSwapBuffers flushes).
//!
//! The exported surface is code-generated from `registry/gles2_egl.manifest` (`build.rs`) so it can never
//! drift from the golden. The EGL lifecycle + the GLES core render path have real hand-written bodies in
//! [`driver`] that marshal the C ABI and call the `hl_gl` lowering services; GL is DEFERRED-lowering, so
//! `gl*` calls RECORD into the shared [`GlContext`] ([`state`]) and `eglSwapBuffers` flushes the frame IR
//! through a [`hl_gpu::RemoteCommandSink`] over `$HL_GPU_EXEC`. The long tail are benign, correct-ABI
//! default stubs ([`stub`]) ported to real bodies incrementally without changing the surface.

// The generated + hand-written entry-point surface uses the GL/EGL C names verbatim (eglInitialize, …).
#![allow(non_snake_case)]

pub mod driver;
pub mod image;
pub mod logging;
pub mod state;
pub mod stub;
#[allow(dead_code)] // Foundation for the pending State transport integration.
mod transport;

// The generated C-ABI export surface: every `egl*`/`gl*` entry point not hand-written in `driver`.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported GLES2/EGL entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = GLES_EGL_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_complete_and_matches_the_census() {
        assert_eq!(
            TOTAL_ENTRYPOINTS, 466,
            "GLES2/EGL surface drifted from the golden 466"
        );
        assert_eq!(GLES2_ENTRYPOINTS, 422);
        assert_eq!(EGL_ENTRYPOINTS, 44);
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_ENTRYPOINTS, TOTAL_ENTRYPOINTS);
    }
}
