//! `cpu` — the reference [`GpuExecutor`], pure CPU with no platform deps: the semantic **oracle** the
//! architecture mandates (§3/§4 of the v2 overview). It reproduces byte-for-byte the outputs the shipping
//! `hl-gpu` `SoftwareBackend` produces, so a real GPU executor (wgpu/Metal) is correct exactly when it
//! matches this one on the executor-neutral conformance suite (`tests/conformance.rs`).
//!
//! Layering (v2 doctrine): [`model`] owns the CPU-native storage objects (stored behind protocol ids in
//! the runtime-owned `SessionResources`) + their downcast accessors; [`service`] owns the per-operation
//! work (raster/copy/compute); [`interp`] is the neutral kernel-IR interpreter; [`format`] holds the
//! pixel/texel rules; [`executor`] is the batch `execute` loop + submit-time validation. It depends inward
//! on `runtime::port` + `protocol`; nothing depends on it except a composition root.
//!
//! Ported from `hl-gpu/src/software.rs` (executor + storage + raster/copy) and the interpreter tail of
//! `hl-gpu/src/ptx.rs` (the kernel IR interpreter — NOT the PTX text front-end, a driver concern).

mod executor;
mod format;
mod interp;
mod model;
mod service;

pub use executor::CpuExecutor;
