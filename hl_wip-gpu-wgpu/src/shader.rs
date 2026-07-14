//! Native shader modules — the point where each protocol shader payload becomes something wgpu compiles.
//!
//! A SPIR-V (or GLSL) *graphics* payload is translated to WGSL by naga at create time and handed to
//! `create_shader_module`, so the guest's real vertex/fragment SPIR-V executes on the device. A neutral
//! *kernel* payload (`PtxKernel`) is kept as its compiled [`KernelProgram`] and lowered to a WGSL compute
//! entry point when a compute pipeline references it (see `pipeline.rs`). Legacy MSL / demo-builtin
//! payloads have no honest WGSL translation and are rejected rather than silently substituted.

use hl_gpu::protocol::model::command::ShaderPayloadKind;
use hl_gpu::protocol::model::kernel::{KernelDescriptor, KernelProgram};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::wgsl;
use crate::WgpuExecutor;

/// The wgpu-native backing of one protocol shader module.
pub enum ShaderNative {
    /// A naga-translated graphics module (vertex/fragment), ready for a render pipeline.
    Graphics(wgpu::ShaderModule),
    /// A compiled compute kernel, lowered to WGSL lazily at compute-pipeline creation.
    Kernel(Box<KernelProgram>),
}

/// Downcast a live shader id to its native handle.
pub fn native<'a>(res: &'a SessionResources, id: u32) -> Result<&'a ShaderNative> {
    res.shaders
        .get(id)?
        .downcast_ref::<ShaderNative>()
        .ok_or(GpuError::Invalid("wgpu: shader native type mismatch"))
}

impl WgpuExecutor {
    pub(crate) fn create_shader(
        &self,
        res: &mut SessionResources,
        id: u32,
        kind: ShaderPayloadKind,
        words: &[u32],
    ) -> Result<()> {
        if words.is_empty() {
            return Err(GpuError::Invalid("empty shader module"));
        }
        let native = match kind {
            ShaderPayloadKind::PtxKernel => {
                // Mirror the CPU oracle: a real driver-produced descriptor (non-empty source) compiles on
                // the fly via the injected front-end; an empty placeholder resolves to a `define_kernel`
                // pre-registered program (the hand-built kernels the conformance suite injects).
                let prog = match KernelDescriptor::from_words(words) {
                    Some(Ok(desc)) if !desc.ptx.is_empty() => {
                        let compiler = self.kernel_compiler.as_ref().ok_or(GpuError::Unsupported(
                            "wgpu: PtxKernel payload needs a kernel compiler (set_kernel_compiler)",
                        ))?;
                        compiler(&desc)?
                    }
                    _ => self.kernels.get(&id).cloned().ok_or(GpuError::Unsupported(
                        "wgpu: no compiled kernel registered for PtxKernel shader id",
                    ))?,
                };
                ShaderNative::Kernel(Box::new(prog))
            }
            ShaderPayloadKind::SpirV => {
                let src = wgsl::spirv_to_wgsl(words)?;
                let module = self.gpu.device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("hl-spirv"),
                    source: wgpu::ShaderSource::Wgsl(src.into()),
                });
                ShaderNative::Graphics(module)
            }
            ShaderPayloadKind::LegacyMsl | ShaderPayloadKind::DemoBuiltin => {
                return Err(GpuError::Unsupported("wgpu: legacy MSL / demo-builtin payloads (no WGSL)"))
            }
        };
        res.shaders.insert(id, Box::new(native))
    }
}
