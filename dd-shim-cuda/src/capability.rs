//! The generated CUDA Driver API **capability inventory** — the machine-checkable census that tags
//! every exported `cu*` entry point as `full`, `partial`, or `unsupported`, and records the exact
//! `CUresult` an unsupported (or out-of-supported-domain) path returns.
//!
//! This is Phase 0's "make completeness measurable" deliverable for CUDA (see `docs/codex-rendering.md`
//! §6 Phase 0 and §2.3): a bare `IMPLEMENTED` name list proves only that a symbol resolves, not that its
//! semantics exist. The inventory is *generated* by `build.rs` from the manifest + a classification
//! table, and the crate asserts against it at test time (`capability::CAPABILITIES` covers every
//! manifest entry). Runtime debug output and `docs/rendering/SHIM_RUST_ARCHITECTURE.md` draw from the
//! same census, so the advertised surface and the truthful surface cannot drift.
//!
//! The classification is deliberately conservative: an entry is `full` only when its observable CUDA
//! semantics are actually implemented for the modeled single-device / synchronous-executor model; it is
//! `partial` when it works within a bounded supported domain (e.g. `cuLaunchKernel` only for the modeled
//! PTX subset — see [`dd_gpu::ptx`]); and `unsupported` when it always returns a defined CUDA error
//! because the feature (peer access, textures/surfaces, `.global` symbols, LUID) is not modeled.

/// Capability level of an exported entry point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cap {
    /// Observable CUDA semantics fully implemented for the modeled device.
    Full,
    /// Works within a bounded supported domain; outside it, returns `cuda_error` (or degrades to a
    /// documented no-op). See the entry's `note` for the domain.
    Partial,
    /// Always returns the defined CUDA error `cuda_error` — the feature is not modeled.
    Unsupported,
}

/// One entry in the capability inventory.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    /// The exported `cu*` symbol name.
    pub name: &'static str,
    /// full / partial / unsupported.
    pub cap: Cap,
    /// The `CUresult` an unsupported (or out-of-domain `partial`) path returns; `0` (`CUDA_SUCCESS`)
    /// for `full` entries and for `partial` entries that degrade to a benign no-op rather than erroring.
    pub cuda_error: i32,
    /// Human-readable supported-domain / reason note (empty for plain `full`).
    pub note: &'static str,
}

/// The single source of truth for the advertised CUDA Driver version (`major*1000 + minor*10`).
/// `cuDriverGetVersion` must report exactly this; the inventory test asserts the identity so the
/// library never advertises a version the modeled surface does not back. 12020 == CUDA 12.2 (ABI).
pub const SUPPORTED_DRIVER_VERSION: i32 = 12020;

/// The compute capability the modeled device advertises (`sm_86`), matching the PTX subset's target and
/// `dd_gpu::cuda::CudaDeviceDesc::apple_default`. Advertised as ABI; the *executed* PTX is the bounded
/// subset enumerated by the `partial` launch entries, not the full sm_86 ISA.
pub const SUPPORTED_COMPUTE_CAPABILITY: (u32, u32) = (8, 6);

// The generated inventory (`CAPABILITIES`, `CAP_FULL`, `CAP_PARTIAL`, `CAP_UNSUPPORTED`) is emitted by
// build.rs and `include!`d at the crate root (see lib.rs) so it can name `crate::capability::{Entry,Cap}`.
