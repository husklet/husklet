//! Build script — DEFERRED for the current staging pass (mirrors hl_wip-cuda's no-op build.rs).
//!
//! Its eventual job: cross-build the two guest shim cdylibs (egl, gles) for aarch64 + x86_64, bake each
//! soname (`libEGL.so.1` primary + the `libGLESv2.so.2` / `libwayland-egl.so.1` DT_NEEDED→libEGL
//! forwarding stubs), run the Khronos-registry entry-point codegen, and install under `~/.hl/gl/<arch>/`.
//! That wiring belongs to the shim-cdylib pass and is intentionally NOT done here — this crate currently
//! ships only the Rust lowering layer (`src/`), which needs no build-time codegen. Keeping this a no-op
//! lets the standalone crate build + test on its own. See `src/lib.rs` "Scope of this staging pass".

fn main() {
    // Nothing to generate yet. The dual-arch shim cross-compile + registry codegen land with `shim/`.
}
