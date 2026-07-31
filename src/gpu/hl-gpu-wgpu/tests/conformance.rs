//! wgpu conformance — the SAME frozen cases as `hl-gpu/tests/conformance.rs`, driven through the real
//! runtime pipeline (validate → account → dispatch → execute) but against a `WgpuExecutor` running on the
//! headless software Vulkan device (lavapipe / `llvmpipe`) instead of the CPU oracle. Every asserted value
//! is IDENTICAL to the oracle's, proving the wgpu backend reproduces it — now with real SPIR-V/WGSL
//! shaders EXECUTING on the device (the compute vecadd kernel and a SPIR-V vertex+fragment triangle, which
//! the pure-CPU oracle cannot run).
//!
//! A single lavapipe device is shared across the cases behind a mutex (device bring-up is the expensive
//! part; the cases themselves are independent, each with its own fresh runtime `Session`).

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    ComputePipelineDesc, Extent3d, Origin3d, RenderPipelineDesc, ShaderRef, TextureDesc,
    TextureSubresource,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureAspect, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, Param, CMP_GE, KERNEL_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuError, GpuExecutor, Limits,
    Session, ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// -------------------------------------------------------------------------------------------------
// shared device + runtime-pipeline harness
// -------------------------------------------------------------------------------------------------

static EXEC: OnceLock<Mutex<WgpuExecutor>> = OnceLock::new();

/// The process-wide wgpu executor bound to the headless software Vulkan adapter.
fn exec() -> MutexGuard<'static, WgpuExecutor> {
    EXEC.get_or_init(|| {
        Mutex::new(
            WgpuExecutor::new(DeviceConfig::default())
                .expect("acquire a wgpu adapter (is a Vulkan ICD / lavapipe reachable?)"),
        )
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

/// Submit `cmds` as one batch through validate → account → dispatch against `exec`, returning the
/// `Session` so its `resources` can be read back. Copy alignment is byte-addressable (1), matching the
/// oracle harness, so the suite's unaligned copies validate.
fn run_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds).expect("conformance program must run cleanly");
    s
}

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc {
        size,
        usage,
        label: String::new(),
    }
}

fn tex(w: u32, h: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}

fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}

// -------------------------------------------------------------------------------------------------
// adapter identity: prove we bound the software Vulkan device
// -------------------------------------------------------------------------------------------------

#[path = "conformance/buffer.rs"]
mod buffer;
#[path = "conformance/compute.rs"]
mod compute;
#[path = "conformance/fill.rs"]
mod fill;
#[path = "conformance/graphics.rs"]
mod graphics;
#[path = "conformance/texture.rs"]
mod texture;
