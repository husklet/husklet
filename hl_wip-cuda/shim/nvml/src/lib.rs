//! Guest cdylib deployed as `libnvidia-ml.so.1` — the NVML drop-in.
//!
//! The exported `nvml*` surface is code-generated from `registry/nvml.manifest` (`build.rs`), extracted
//! from the clean-room oracle `hl-gpu/nvml/nvml_shim.c`, so it can never drift from that 62-entry surface.
//! The init/shutdown/error/count/handle/name/version basics have real bodies in [`nvml`] so a probe
//! enumerates the single simulated device; the rest are benign `NVML_SUCCESS` stubs ([`stub`]). The
//! soname `libnvidia-ml.so.1` is baked by `build.rs`.

#![allow(non_snake_case)]

pub mod nvml;
pub mod stub;

// The generated C-ABI export surface: every `nvml*` entry point not hand-written in `nvml`.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported NVML entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = NVML_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_complete_and_matches_the_census() {
        assert_eq!(NVML_ENTRYPOINTS, 62, "NVML surface drifted from the oracle's 62 exports");
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_ENTRYPOINTS, TOTAL_ENTRYPOINTS);
    }
}
