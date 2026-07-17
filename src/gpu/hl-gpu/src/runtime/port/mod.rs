//! The boundary traits the runtime drives through (inward contracts): [`executor::GpuExecutor`] — the
//! injected host executor (CPU / wgpu) — and [`clock::Clock`] — the pacing/timeline time source. Neither
//! references a platform/GPU type; both are object-safe so the runtime holds them behind `dyn`.

pub mod clock;
pub mod executor;
