//! hl-shim-gl — the guest GLES2/EGL shim, in Rust.
//!
//! Builds the single shared object deployed as `libEGL.so.1` (with `libGLESv2.so.2` /
//! `libwayland-egl.so.1` as thin DT_NEEDED stubs). A real GLES2 app links `-lEGL -lGLESv2` and runs
//! unmodified: every `egl*` / `gl*` symbol below is exported with the exact Khronos C ABI. On swap the
//! front-end lowers accumulated state into a `hl-gpu` IR stream and ships it, via
//! [`hl_shim::transport`], to the host executor — the same IR the host decodes with the SAME
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
// IR type is hl-gpu's, not a local copy.
pub use hl_shim as common;

pub mod egl;
pub mod frame;
pub mod gles;
pub mod glconst;
pub mod lower;
pub mod state;
pub mod stub;
pub mod tiletrace;
pub mod translate;
pub mod wayland;
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
        // Census identity: every entry point is either a hand-written body or a generated one.
        assert_eq!(GENERATED_STUBS + CAP_FULL, TOTAL_ENTRYPOINTS);
        // The three capability levels partition the whole exported surface.
        assert_eq!(CAP_FULL + CAP_PARTIAL + CAP_STUB, TOTAL_ENTRYPOINTS);
    }

    /// The completeness gate (Phase 0 exit): the generated inventory names EVERY exported call, exactly
    /// once, with a definite full/partial/stub capability level — so no symbol can be advertised without
    /// a capability record.
    #[test]
    fn inventory_covers_every_exported_symbol() {
        assert_eq!(
            CAPABILITIES.len(),
            TOTAL_ENTRYPOINTS,
            "inventory must have one record per exported entry point"
        );
        // No duplicate records.
        let mut names: Vec<&str> = CAPABILITIES.iter().map(|c| c.name).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate capability records");

        // Level counts match the census constants, and each level's invariants hold.
        let (mut full, mut partial, mut stub) = (0, 0, 0);
        for c in CAPABILITIES {
            match c.level {
                CapLevel::Full => {
                    full += 1;
                    // A `full` symbol must have a real hand-written body -> no gl_error to raise.
                    assert_eq!(c.gl_error, 0, "{} is full but carries an error", c.name);
                    assert!(!c.since.is_empty(), "{} is full but has no `since`", c.name);
                }
                CapLevel::Partial => {
                    partial += 1;
                    assert_eq!(c.gl_error, 0, "{} is a partial no-op but carries an error", c.name);
                }
                CapLevel::Stub => {
                    stub += 1;
                    // A `stub` MUST raise an API-correct error (never a silent success).
                    assert_ne!(c.gl_error, 0, "{} is a stub but raises no error", c.name);
                }
            }
        }
        assert_eq!((full, partial, stub), (CAP_FULL, CAP_PARTIAL, CAP_STUB));
    }
}
