//! Build script — DEFERRED for the current staging pass.
//!
//! Its eventual job (OVERVIEW D1): cross-build the guest Vulkan ICD shim cdylib (`libvk_hl.so.1` + its
//! `icd.json`) for aarch64 + x86_64, apply the soname/version-script, generate the `#[no_mangle]` vk*
//! export tail from the registry manifests, and install each under `~/.hl/vulkan/<arch>/`. That wiring
//! belongs to the shim-cdylib pass and is intentionally NOT done here — this crate currently ships only
//! the Rust lowering layer (`src/`), which needs no build-time codegen. Keeping this a no-op lets the
//! standalone crate build + test on its own. See `src/lib.rs` "Scope of this staging pass".

fn main() {
    // Nothing to generate yet. The dual-arch ICD shim cross-compile + export generation lands with `shim/`.
}
