//! [`CpuExecutor`] — the reference [`GpuExecutor`], pure CPU with no platform deps. It is the semantic
//! **oracle**: it reproduces byte-for-byte the outputs the shipping `hl-gpu` `SoftwareBackend` produces,
//! so a real GPU executor (wgpu/Metal) is correct exactly when it matches this one on the conformance
//! suite.
//!
//! Ported from `SoftwareBackend` in `hl-gpu/src/software.rs`: its per-resource `create_*`/`destroy_*`,
//! `write_buffer`, `submit` (validate-then-execute), and `present`, collapsed onto the single batch
//! [`GpuExecutor::execute`] over the runtime-owned [`SessionResources`]. The one adaptation: the PTX text
//! front-end lives in the driver (`hl-cuda`), not here, so a `PtxKernel` shader consumes a **pre-compiled
//! [`KernelProgram`]** registered via [`CpuExecutor::define_kernel`] rather than parsing PTX at create time.
//!
//! [`SessionResources`]: crate::runtime::model::resources::SessionResources
//! [`KernelProgram`]: crate::protocol::model::kernel::KernelProgram

use std::collections::HashMap;

use crate::cpu::model::pipeline::Pipeline;
use crate::cpu::model::shader::ShaderModule;
use crate::cpu::model::{
    bind_group, buffer, buffer_mut, fence, fence_mut, surface, texture, BindGroupState, Buffer,
    GenRef, Sampler, Texture,
};
use crate::cpu::service::compute::run_dispatch;
use crate::cpu::service::copy;
use crate::cpu::service::raster;
use crate::protocol::model::capability::{
    shader_payload, Capabilities, PresentKind, COLOR_FORMATS, DEPTH_FORMATS,
};
use crate::protocol::model::command::{Cmd, CommandBuffer, Enc, ShaderPayloadKind, WIRE_VERSION};
use crate::protocol::model::descriptor::{
    BindResource, ComputePipelineDesc, RenderPipelineDesc, TextureDesc,
};
use crate::protocol::model::enums::{
    buffer_usage, texture_usage, IndexFormat, LoadOp, TextureDim, TextureFormat,
};
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{BufferId, FenceId, SurfaceId, TextureId};
use crate::protocol::model::kernel::{KernelDescriptor, KernelProgram, SPIRV_MAGIC};
use crate::runtime::model::resources::SessionResources;
use crate::runtime::port::executor::{GpuExecutor, Presentation};

mod api;
mod operation;
mod resource;
mod submit;
mod validation;

/// The pure CPU reference executor. Holds no resources of its own (those live in the runtime-owned
/// [`SessionResources`]); it carries only the pre-compiled kernels a `PtxKernel` shader resolves to and a
/// couple of work counters a test can read.
#[derive(Default)]
pub struct CpuExecutor {
    /// Pre-compiled kernels keyed by the shader id a later `CreateShader { PtxKernel, .. }` uses. The PTX
    /// text parser is a driver concern (`hl-cuda`), so the compiled program can be injected here directly
    /// (the `define_kernel` convenience) for tests that hand-build a [`KernelProgram`].
    kernels: HashMap<u32, KernelProgram>,
    /// Optional kernel front-end: compiles a driver-forwarded [`KernelDescriptor`] (source text + entry +
    /// block dims, decoded from a `CreateShader` KERNEL payload) into a [`KernelProgram`]. Injected by the
    /// composition root so the PTX parser stays a driver concern (`hl-cuda`) and never links into this
    /// crate. When set, a real (non-empty) descriptor on the wire is compiled directly — no `define_kernel`.
    #[allow(clippy::type_complexity)]
    kernel_compiler: Option<Box<dyn Fn(&KernelDescriptor) -> Result<KernelProgram>>>,
    /// Count of dispatches/draws seen — lets a test confirm compute/draw work reached the executor.
    pub dispatches: u64,
    pub draws: u64,
}

impl CpuExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pre-compiled [`KernelProgram`] under the shader id a subsequent
    /// `CreateShader { kind: PtxKernel, id, .. }` will carry. Stands in for the driver's PTX→kernel-IR
    /// front-end, which is not part of this crate.
    pub fn define_kernel(&mut self, shader_id: u32, program: KernelProgram) {
        self.kernels.insert(shader_id, program);
    }

    /// Inject the kernel front-end that compiles a driver-forwarded [`KernelDescriptor`] (decoded from a
    /// `CreateShader` KERNEL payload) into a [`KernelProgram`]. With this set, a `PtxKernel` shader whose
    /// payload carries a real descriptor is compiled on the fly — the real driver flow needs no
    /// `define_kernel`. Keeping the parser out here preserves this crate's freedom from any PTX/CUDA code.
    pub fn set_kernel_compiler<F>(&mut self, compiler: F)
    where
        F: Fn(&KernelDescriptor) -> Result<KernelProgram> + 'static,
    {
        self.kernel_compiler = Some(Box::new(compiler));
    }

    /// Read `out.len()` bytes back from buffer `id` at `offset` — the readback path a conformance test
    /// asserts on. Operates over the runtime-owned resources (the executor stores natives there).
    pub fn read_buffer(
        &self,
        resources: &SessionResources,
        id: BufferId,
        offset: u64,
        out: &mut [u8],
    ) -> Result<()> {
        let b = buffer(resources, id.0)?;
        let off = offset as usize;
        let end = offset
            .checked_add(out.len() as u64)
            .filter(|e| *e <= b.data.len() as u64)
            .ok_or(GpuError::OutOfBounds)? as usize;
        out.copy_from_slice(&b.data[off..end]);
        Ok(())
    }

    /// Read the whole tight-packed level-0 pixel plane of texture `id` (exactly `out.len()` bytes).
    pub fn read_texture(
        &self,
        resources: &SessionResources,
        id: TextureId,
        out: &mut [u8],
    ) -> Result<()> {
        let t = texture(resources, id.0)?;
        if out.len() != t.pixels.len() {
            return Err(GpuError::OutOfBounds);
        }
        out.copy_from_slice(&t.pixels);
        Ok(())
    }
}
