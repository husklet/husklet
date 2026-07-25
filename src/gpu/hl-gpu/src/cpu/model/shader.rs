//! The CPU-native shader module. The software oracle's shader ABI is a neutral **kernel program** it can
//! interpret on the CPU; opaque SPIR-V is accepted (validated at create time) but never run here — that
//! needs a real GPU executor. Ported from the `ShaderModule` enum in `hl-gpu/src/software.rs`.

use crate::protocol::model::kernel::KernelProgram;

pub enum ShaderModule {
    /// A compiled compute kernel this executor can actually run on the CPU (see [`crate::cpu::interp`]).
    Kernel(Box<KernelProgram>),
    /// Opaque SPIR-V — accepted but not run here (needs a Metal/Vulkan/wgpu executor).
    Spirv,
}
