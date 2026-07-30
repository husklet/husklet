//! CUDA **robustness / error-path / determinism** demo battery — the honest-error counterpart to
//! `tests/compute_demo.rs` (which is pure compute-correctness). Every demo drives a real CUDA workload
//! through the REAL hl-cuda driver lowering → an in-process [`InProcessCommandSink`] over the reference
//! [`CpuExecutor`], and then asserts one of:
//!   * an invalid operation returns the **specific, honest `CUresult`/`cudaError_t`** a real driver;
//!     never `cudaSuccess`, the fake-success anti-pattern these demos exist to catch; or
//!   * a valid operation is **bit-exact** and **deterministic** across repeats / issue interleavings.
//!
//! Batteries:
//!   1. `error_bad_launch`            — an over-`maxThreadsPerBlock` / zero-dim launch returns
//!      `CUDA_ERROR_INVALID_VALUE`, emits NOTHING, computes nothing.
//!   2. `stream_event_ordering`       — record an event on stream A, make stream B wait on it; the
//!      dependent kernel observes A's result, bit-exact. Bad handles error.
//!   3. `async_overlap_correctness`   — async H2D + kernel + async D2H on a stream, synchronized; the
//!      final host data is bit-exact (overlap never corrupts).
//!   4. `oom_alloc_rejected`          — an allocation past the modeled device budget returns
//!      `CUDA_ERROR_OUT_OF_MEMORY` / `cudaErrorMemoryAllocation`, mints
//!      no state, and never null-derefs or fakes success.
//!   5. `large_reduction_determinism` — a large sum + max reduction run twice yields identical bit-exact
//!      results, matching an independent CPU reference.
//!   6. `concurrent_kernels_determinism` — kernels across multiple streams produce identical bit-exact
//!      output regardless of the order they are issued in.

use hl_cuda::adapter::ptx;
use hl_cuda::model::event::Event;
use hl_cuda::model::stream::Stream;
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{result, CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::capability::{
    shader_payload, Capabilities, ALL_COMMANDS, COLOR_FORMATS,
};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{
    BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION,
};

// --------------------------------------------------------------------------------------------------
// shared harness — identical wiring to tests/compute_demo.rs + tests/e2e.rs.
// --------------------------------------------------------------------------------------------------

fn harness() -> InProcessCommandSink<CpuExecutor> {
    let mut exec = CpuExecutor::new();
    exec.set_kernel_compiler(|desc: &KernelDescriptor| {
        ptx::compile(&desc.ptx, &desc.entry, desc.block)
    });
    let mut sink = InProcessCommandSink::new(exec);
    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: Capabilities::command_bits(ALL_COMMANDS),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
        ..FeatureRequest::default()
    };
    sink.negotiate(&req).expect("negotiate against CpuExecutor");
    sink
}

fn f32s_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn i32s_to_bytes(v: &[i32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn bytes_to_f32s(raw: &[u8]) -> Vec<f32> {
    raw.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}
fn bytes_to_i32s(raw: &[u8]) -> Vec<i32> {
    raw.chunks_exact(4)
        .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn readback(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &CudaContext,
    p: DevicePtr,
    len: usize,
) -> Vec<u8> {
    let (buf, off): (BufferId, u64) = ctx.device_location(p).unwrap();
    sink.read_buffer(buf, off, len).unwrap()
}

fn upload(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &mut CudaContext,
    bytes: &[u8],
) -> DevicePtr {
    let p = allocate::mem_alloc(ctx, sink, bytes.len() as u64).unwrap();
    transfer::memcpy_htod(ctx, sink, p, bytes).unwrap();
    p
}

fn sc_i32(v: i32) -> KernelArg {
    KernelArg::Scalar(v.to_le_bytes().to_vec())
}
fn sc_f32(v: f32) -> KernelArg {
    KernelArg::Scalar(v.to_le_bytes().to_vec())
}

/// `out[i] = a * in[i] + b` (f32), one thread per element with an `i >= n` guard. Params are declared in
/// the natural nvcc order `(in*, out*, a, b, n)` → offsets `u64@0, u64@8, f32@16, f32@20, u32@24`.
const AFFINE_PTX: &str = r#"
    .visible .entry affine(
        .param .u64 af_in,
        .param .u64 af_out,
        .param .f32 af_a,
        .param .f32 af_b,
        .param .u32 af_n
    )
    {
        ld.param.u64  %rin, [af_in];
        ld.param.u64  %rout, [af_out];
        ld.param.f32  %fa, [af_a];
        ld.param.f32  %fb, [af_b];
        ld.param.u32  %rn, [af_n];
        mov.u32 %rntid, %ntid.x; mov.u32 %rctaid, %ctaid.x; mov.u32 %rtid, %tid.x;
        mad.lo.s32 %i, %rctaid, %rntid, %rtid;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        add.s64 %pout, %gout, %off;
        ld.global.f32 %v, [%pin];
        fma.rn.f32 %r, %fa, %v, %fb;
        st.global.f32 [%pout], %r;
    DONE: ret;
    }
"#;

/// `out[i] = k * in[i] + c` (s32), one thread per element with an `i >= n` guard. Params
/// `(in*, out*, k, c, n)` → offsets `u64@0, u64@8, u32@16, u32@20, u32@24`.
const ISCALE_PTX: &str = r#"
    .visible .entry iscale(
        .param .u64 is_in,
        .param .u64 is_out,
        .param .u32 is_k,
        .param .u32 is_c,
        .param .u32 is_n
    )
    {
        ld.param.u64  %rin, [is_in];
        ld.param.u64  %rout, [is_out];
        ld.param.u32  %rk, [is_k];
        ld.param.u32  %rc, [is_c];
        ld.param.u32  %rn, [is_n];
        mov.u32 %rntid, %ntid.x; mov.u32 %rctaid, %ctaid.x; mov.u32 %rtid, %tid.x;
        mad.lo.s32 %i, %rctaid, %rntid, %rtid;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        add.s64 %pout, %gout, %off;
        ld.global.u32 %v, [%pin];
        mad.lo.s32 %r, %v, %rk, %rc;
        st.global.u32 [%pout], %r;
    DONE: ret;
    }
"#;

/// `out[0] += in[i]` for every in-bounds lane, accumulated grid-wide via `red.global.add.u32`.
const REDUCE_SUM_PTX: &str = r#"
    .visible .entry reduce_sum(
        .param .u64 rs_in, .param .u64 rs_out, .param .u32 rs_n
    )
    {
        ld.param.u64  %rin, [rs_in];
        ld.param.u64  %rout, [rs_out];
        ld.param.u32  %rn, [rs_n];
        mov.u32 %rntid, %ntid.x; mov.u32 %rctaid, %ctaid.x; mov.u32 %rtid, %tid.x;
        mad.lo.s32 %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32 %pg, %ri, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %ri, 4;
        add.s64 %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        cvta.to.global.u64 %gout, %rout;
        red.global.add.u32 [%gout], %v;
    DONE: ret;
    }
"#;

/// `out[0] = max(out[0], in[i])` grid-wide via signed `red.global.max.s32`.
const REDUCE_MAX_PTX: &str = r#"
    .visible .entry reduce_max(
        .param .u64 rm_in, .param .u64 rm_out, .param .u32 rm_n
    )
    {
        ld.param.u64  %rin, [rm_in];
        ld.param.u64  %rout, [rm_out];
        ld.param.u32  %rn, [rm_n];
        mov.u32 %rntid, %ntid.x; mov.u32 %rctaid, %ctaid.x; mov.u32 %rtid, %tid.x;
        mad.lo.s32 %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32 %pg, %ri, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %ri, 4;
        add.s64 %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        cvta.to.global.u64 %gout, %rout;
        red.global.max.s32 [%gout], %v;
    DONE: ret;
    }
"#;

#[path = "robustness_demo/allocation.rs"]
mod allocation;
#[path = "robustness_demo/concurrency.rs"]
mod concurrency;
#[path = "robustness_demo/launch_error.rs"]
mod launch_error;
#[path = "robustness_demo/ordering.rs"]
mod ordering;
#[path = "robustness_demo/overlap.rs"]
mod overlap;
#[path = "robustness_demo/reduction.rs"]
mod reduction;
