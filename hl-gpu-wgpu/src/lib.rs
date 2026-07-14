//! `hl-gpu-wgpu` — a [`hl_gpu::GpuBackend`] executor built on `wgpu` + `naga`.
//!
//! The whole implementation is `#[cfg(target_os = "macos")]`: `wgpu`/`naga`/`pollster` are declared only
//! under the macOS target in `Cargo.toml`, so on the Linux dev host this crate compiles to an empty lib
//! and the headless hl-gpu / hl-display core keep building with no new compiled dependencies. On macOS
//! the backend resolves the hl-GPU command IR against a real Metal device through wgpu.
//!
//! See [`backend::WgpuBackend`] for the executor and `shader::translate` for the naga lowering.

#![cfg_attr(not(target_os = "macos"), allow(unused))]

#[cfg(target_os = "macos")]
mod backend;
#[cfg(target_os = "macos")]
mod shader;

#[cfg(target_os = "macos")]
pub use backend::WgpuBackend;

/// The value of the `HL_GPU_BACKEND` selection env var, lowercased. `"wgpu"` selects this backend;
/// anything else (including unset) keeps the default bespoke `metal_backend` replay. Kept here (not in
/// hl-display) so the selection contract lives with the backend it names and both the executor wiring
/// and tests read one source of truth.
pub const ENV_SELECT: &str = "HL_GPU_BACKEND";

/// `true` when `HL_GPU_BACKEND=wgpu` is set in the environment — the flag the display executor branches
/// on to construct a [`WgpuBackend`] instead of the default Metal replay.
pub fn selected() -> bool {
    std::env::var(ENV_SELECT)
        .map(|v| v.trim().eq_ignore_ascii_case("wgpu"))
        .unwrap_or(false)
}
