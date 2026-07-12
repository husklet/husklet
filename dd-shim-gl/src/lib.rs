//! dd-shim-gl — the guest GLES2/EGL shim, in Rust.
//!
//! Builds the single shared object deployed as `libEGL.so.1` (with `libGLESv2.so.2` /
//! `libwayland-egl.so.1` as thin DT_NEEDED stubs). A real GLES2 app links `-lEGL -lGLESv2` and runs
//! unmodified: every `egl*` / `gl*` symbol below is exported with the exact Khronos C ABI. On swap the
//! front-end lowers accumulated state into a `dd-gpu` IR stream and ships it, via
//! [`dd_shim_common::transport`], to the host executor — the same IR the host decodes with the SAME
//! Rust code (no hand-rolled second encoder, unlike the retiring C shim).
//!
//! ## Coverage
//! The exported entry-point *surface* is code-generated from the Khronos registry (`build.rs` +
//! `registry/`), so it is the complete GLES2+EGL symbol set. Entry points in
//! [`build::IMPLEMENTED`](../build.rs) have real hand-written bodies (in [`egl`] / [`gles`]); the rest
//! are generated spec-faithful default stubs (correct ABI, benign return, debug-traced) that are
//! ported to real bodies incrementally — the shrinking long tail.

// The generated entry-point surface uses the GL/EGL C names verbatim (glActiveTexture, …).
#![allow(non_snake_case)]

// The shared IR + transport foundation. Re-exported so this crate's modules (and readers) see that the
// IR type is dd-gpu's, not a local copy.
pub use dd_shim_common as common;

pub mod egl;
pub mod frame;
pub mod gles;
pub mod glconst;
pub mod lower;
pub mod state;
pub mod stub;
pub mod wireenc;

// The generated C-ABI export surface (every GLES2/EGL entry point not in `IMPLEMENTED`).
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported GLES2+EGL entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = GLES2_ENTRYPOINTS + EGL_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_complete_and_large() {
        // The registry-driven surface must be the full GLES2+EGL set, not a hand-picked few.
        assert!(GLES2_ENTRYPOINTS >= 140, "GLES2 surface too small: {GLES2_ENTRYPOINTS}");
        assert!(EGL_ENTRYPOINTS >= 40, "EGL surface too small: {EGL_ENTRYPOINTS}");
        // Every entry point is either hand-implemented or a generated stub.
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_COUNT, TOTAL_ENTRYPOINTS);
    }

    // Count of hand-written entry points (kept in sync with build.rs IMPLEMENTED via the census).
    const IMPLEMENTED_COUNT: usize = TOTAL_ENTRYPOINTS - GENERATED_STUBS;
}
