//! Build script — DEFERRED for the current staging pass.
//!
//! Its eventual job (OVERVIEW D1): cross-build the three guest shim cdylibs (cuda, cudart, nvml) for
//! aarch64 + x86_64, apply each soname/version-script, and install them under `~/.hl/…`. That wiring
//! belongs to the shim-cdylib pass and is intentionally NOT done here — this crate currently ships only
//! the Rust lowering layer (`src/`), which needs no build-time codegen. Keeping this a no-op lets the
//! standalone crate build + test on its own. See `src/lib.rs` "Scope of this staging pass".

fn main() {
    // Nothing to generate yet. The dual-arch shim cross-compile lands with `shim/`.
}
