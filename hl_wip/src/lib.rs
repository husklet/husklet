//! hl_wip — the staging COMPOSITION ROOT (mirrors the future top-level `hl` crate).
//!
//! This crate carries NO library code of its own: it exists only to host the integration, real-app, and
//! real-software tests in `tests/`, which compose the REAL driver + runtime + compositor crates
//! end-to-end. It is the single point where "we compose these things" — the consolidation of the three
//! former throwaway crates (`hl_wip-integration`, `hl_wip-realapp`, `hl_wip-realsw`) into one host so the
//! dependency graph is sane.
//!
//! The tests:
//!   * `tests/plug.rs`          — `engine.add(Cuda::new()) / Vulkan::new() / Gl::new()` composes all three
//!                                driver plugs into the `hl_jit::Drivers` registry (mounts + env seam).
//!   * `tests/lower.rs`         — all three drivers lower onto ONE shared `InProcessCommandSink<CpuExecutor>`.
//!   * `tests/realapp_cuda.rs`  — a real app `dlopen`s the staged `libcuda.so.1` and runs a vecadd over a
//!                                unix socket served by a host `CpuExecutor`.
//!   * `tests/gl_eglinfo.rs`    — the real Khronos `eglinfo` binary queries OUR staged EGL shim.
//!   * `tests/vk_loader_icd.rs` — the real Khronos Vulkan loader drives OUR ICD from a real C program.
//!   * `tests/cuda_c_vecadd.rs` — a real C CUDA program computes a vecadd through OUR staged `libcuda.so`.
//!
//! Shared host-side plumbing (the `$HL_GPU_EXEC` executor socket server + staged-shim locators) lives in
//! `tests/common/mod.rs`. The empty lib target just gives the package something to link.
