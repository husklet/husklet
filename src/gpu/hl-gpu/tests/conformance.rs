//! Executor-neutral GPU conformance suite for the v2 stack — the semantic oracle for the hl-gpu IR,
//! driven through the NEW path (a runtime [`Session`] + a [`CpuExecutor`], submitting each batch via the
//! runtime pipeline validate → account → dispatch → execute) and asserting the exact observable result
//! (buffer readback bytes, texture pixel readback).
//!
//! Every asserted value here is IDENTICAL to what the shipping `hl-gpu/tests/conformance.rs` asserts today
//! against its `SoftwareBackend`: this proves the ported [`CpuExecutor`] reproduces the oracle byte-for-
//! byte. A future real executor (`hl-gpu-wgpu`) pointed at the same suite must match these same values.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ComputePipelineDesc,
    ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat,
};
use hl_gpu::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, Param, CMP_EQ, CMP_GE, CMP_GT, CMP_LE, CMP_LT, CMP_NE,
    CVT_F32_FROM_S32, CVT_F32_FROM_U32, CVT_S32_FROM_F32, CVT_S32_FROM_F32_RNI, CVT_U32_FROM_F32,
    CVT_U32_FROM_F32_RNI, KERNEL_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind, TextureId,
};

// -------------------------------------------------------------------------------------------------
// harness: drive a program through the real runtime pipeline against a CpuExecutor
// -------------------------------------------------------------------------------------------------

/// Submit `cmds` as one batch through validate → account → dispatch against `exec`, returning the
/// [`Session`] (so its `resources` can be read back). Panics on any pipeline error (a conformance program
/// is expected to be well-formed). The connection's copy alignment is set byte-addressable because the CPU
/// oracle is byte-addressable (matching the direct-replay path `hl-gpu/tests/conformance.rs` uses).
fn run_batch(exec: &mut hl_gpu::CpuExecutor, cmds: &[Cmd]) -> Session {
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

/// Non-empty placeholder shader words for a `PtxKernel` `CreateShader`; the compiled program itself is
/// injected via [`hl_gpu::CpuExecutor::define_kernel`] (the PTX front-end is a driver concern, not here).
fn kernel_words() -> Vec<u32> {
    vec![KERNEL_MAGIC, 0]
}

// -------------------------------------------------------------------------------------------------
// buffer: write + readback
// -------------------------------------------------------------------------------------------------

#[path = "conformance/buffer.rs"]
mod buffer;
#[path = "conformance/compute.rs"]
mod compute;
#[path = "conformance/kernel_arith.rs"]
mod kernel_arith;
#[path = "conformance/texture.rs"]
mod texture;
