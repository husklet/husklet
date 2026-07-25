//! External, tech-named mechanisms the CUDA driver drives (OVERVIEW-v2 §2 `adapter/`).
//!
//! * [`ptx`] — the PTX-text front-end: `PTX → hl-GPU neutral kernel-IR`
//!   ([`hl_gpu::protocol::model::kernel::KernelProgram`]). Ported from `hl-gpu/src/ptx.rs` (the PARSER
//!   only — the CPU interpreter and the WGSL back-end stay host-side). Per OVERVIEW-v2 D3, the PTX
//!   parser lives in the driver, not the neutral protocol.
//! * [`fatbin`] — the clean-room nvcc fatbin container walker: unwrap `__cudaRegisterFatBinary` payloads
//!   and extract the embedded uncompressed PTX image for `cuModuleLoadData`.

pub mod fatbin;
pub mod ptx;
