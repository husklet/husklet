//! CUDA **robustness / error-path / determinism** demo battery — the honest-error counterpart to
//! `tests/compute_demo.rs` (which is pure compute-correctness). Every demo drives a real CUDA workload
//! through the REAL hl-cuda driver lowering → an in-process [`InProcessCommandSink`] over the reference
//! [`CpuExecutor`], and then asserts one of:
//!   * an invalid operation returns the **specific, honest `CUresult`/`cudaError_t`** a real driver
//!     returns (never `cudaSuccess` — a faked success is the anti-pattern these demos exist to catch), or
//!   * a valid operation is **bit-exact** and **deterministic** across repeats / issue interleavings.
//!
//! Batteries:
//!   1. `error_bad_launch`            — an over-`maxThreadsPerBlock` / zero-dim launch returns
//!                                       `CUDA_ERROR_INVALID_VALUE`, emits NOTHING, computes nothing.
//!   2. `stream_event_ordering`       — record an event on stream A, make stream B wait on it; the
//!                                       dependent kernel observes A's result, bit-exact. Bad handles error.
//!   3. `async_overlap_correctness`   — async H2D + kernel + async D2H on a stream, synchronized; the
//!                                       final host data is bit-exact (overlap never corrupts).
//!   4. `oom_alloc_rejected`          — an allocation past the modeled device budget returns
//!                                       `CUDA_ERROR_OUT_OF_MEMORY` / `cudaErrorMemoryAllocation`, mints
//!                                       no state, and never null-derefs or fakes success.
//!   5. `large_reduction_determinism` — a large sum + max reduction run twice yields identical bit-exact
//!                                       results, matching an independent CPU reference.
//!   6. `concurrent_kernels_determinism` — kernels across multiple streams produce identical bit-exact
//!                                       output regardless of the order they are issued in.

use hl_cuda::adapter::ptx;
use hl_cuda::model::event::Event;
use hl_cuda::model::stream::Stream;
use hl_cuda::service::{allocate, event, launch, load_module, synchronize, transfer};
use hl_cuda::{result, CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::capability::{command_bits, format_bits, shader_payload, ALL_COMMANDS, COLOR_FORMATS};
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION};

// --------------------------------------------------------------------------------------------------
// shared harness — identical wiring to tests/compute_demo.rs + tests/e2e.rs.
// --------------------------------------------------------------------------------------------------

fn harness() -> InProcessCommandSink<CpuExecutor> {
    let mut exec = CpuExecutor::new();
    exec.set_kernel_compiler(|desc: &KernelDescriptor| ptx::compile(&desc.ptx, &desc.entry, desc.block));
    let mut sink = InProcessCommandSink::new(exec);
    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: command_bits(ALL_COMMANDS),
        texture_formats: format_bits(COLOR_FORMATS),
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
    raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}
fn bytes_to_i32s(raw: &[u8]) -> Vec<i32> {
    raw.chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn readback(sink: &mut InProcessCommandSink<CpuExecutor>, ctx: &CudaContext, p: DevicePtr, len: usize) -> Vec<u8> {
    let (buf, off): (BufferId, u64) = transfer::memcpy_dtoh(ctx, p).unwrap();
    sink.read_buffer(buf, off, len).unwrap()
}

fn upload(sink: &mut InProcessCommandSink<CpuExecutor>, ctx: &mut CudaContext, bytes: &[u8]) -> DevicePtr {
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

// ==================================================================================================
// 1. error_bad_launch — an invalid launch config returns CUDA_ERROR_INVALID_VALUE, not cudaSuccess.
//    (Regression guard for the driver faking success on a config real hardware could never dispatch.)
// ==================================================================================================

#[test]
fn error_bad_launch_returns_honest_error() {
    let n = 256usize;
    let x: Vec<f32> = (0..n).map(|i| i as f32 * 0.5).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, AFFINE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "affine").unwrap();

    let dx = upload(&mut sink, &mut ctx, &f32s_to_bytes(&x));
    let dy = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let good_args = vec![KernelArg::Ptr(dx), KernelArg::Ptr(dy), sc_f32(3.0), sc_f32(1.0), sc_i32(n as i32)];

    // A legal launch first: proves the kernel + harness are healthy and computes a real result.
    launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (256, 1, 1), &good_args).unwrap();
    assert_eq!(sink.executor().dispatches, 1);
    let got = bytes_to_f32s(&readback(&mut sink, &ctx, dy, n * 4));
    let want: Vec<f32> = x.iter().map(|v| 3.0f32.mul_add(*v, 1.0)).collect();
    assert_eq!(got, want, "the valid launch computed the real affine result");

    // maxThreadsPerBlock is 1024 on the modeled device; 32*32*2 = 2048 threads is over the limit → a real
    // driver returns CUDA_ERROR_INVALID_VALUE. It must NOT silently run (which the software oracle would
    // otherwise happily do — a fake success).
    let err = launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (32, 32, 2), &good_args).unwrap_err();
    assert_eq!(
        result::cu_result_from_gpu_error(&err),
        result::CUDA_ERROR_INVALID_VALUE,
        "over-maxThreadsPerBlock launch → CUDA_ERROR_INVALID_VALUE"
    );
    assert_eq!(result::cudart_from_gpu_error(&err), result::CUDART_ERROR_INVALID_VALUE);
    assert_ne!(result::cu_result_from_gpu_error(&err), result::CUDA_SUCCESS, "NOT a faked success");

    // A zero block dimension is equally invalid.
    let zerr = launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (256, 0, 1), &good_args).unwrap_err();
    assert_eq!(result::cu_result_from_gpu_error(&zerr), result::CUDA_ERROR_INVALID_VALUE);
    // …and so is a zero grid dimension.
    let gerr = launch::launch(&mut ctx, &mut sink, func, (0, 1, 1), (256, 1, 1), &good_args).unwrap_err();
    assert_eq!(result::cu_result_from_gpu_error(&gerr), result::CUDA_ERROR_INVALID_VALUE);

    // Crucially: none of the three rejected launches reached the sink — the dispatch count is still 1, so
    // the bad launches emitted NOTHING and computed NOTHING (no partial/garbage write into `dy`).
    assert_eq!(sink.executor().dispatches, 1, "rejected launches emit no dispatch");
    let after = bytes_to_f32s(&readback(&mut sink, &ctx, dy, n * 4));
    assert_eq!(after, want, "the output buffer is untouched by the rejected launches");
}

// ==================================================================================================
// 2. stream_event_ordering — record an event on stream A, make stream B wait; B's dependent kernel
//    observes A's result, bit-exact. Bad event/stream handles surface honest errors.
// ==================================================================================================

#[test]
fn stream_event_ordering_dependent_work_observes_producer() {
    let n = 1024usize;
    let x: Vec<f32> = (0..n).map(|i| i as f32 * 0.25 - 7.0).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, AFFINE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "affine").unwrap();

    let stream_a: Stream = ctx.streams.create();
    let stream_b: Stream = ctx.streams.create();
    let ev: Event = event::event_create(&mut ctx);

    // Producer on stream A: async-upload x, then r = 2*x + 0.
    let d_in = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    transfer::memcpy_htod_async(&mut ctx, &mut sink, stream_a, d_in, &f32s_to_bytes(&x)).unwrap();
    let d_r = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let prod_args = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_r), sc_f32(2.0), sc_f32(0.0), sc_i32(n as i32)];
    launch::launch(&mut ctx, &mut sink, func, (4, 1, 1), (256, 1, 1), &prod_args).unwrap();

    // Record the producer's completion on stream A; stream B must wait for it before its consumer runs.
    event::event_record(&mut ctx, ev, stream_a).unwrap();
    assert!(event::event_query(&ctx, ev).unwrap(), "event is complete after record (synchronous model)");
    event::stream_wait_event(&ctx, stream_b, ev).unwrap();

    // Consumer on stream B: o = 1*r + 1 = r + 1. If ordering were NOT honored, r would still be zero here.
    let d_out = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let cons_args = vec![KernelArg::Ptr(d_r), KernelArg::Ptr(d_out), sc_f32(1.0), sc_f32(1.0), sc_i32(n as i32)];
    launch::launch(&mut ctx, &mut sink, func, (4, 1, 1), (256, 1, 1), &cons_args).unwrap();

    event::event_synchronize(&ctx, ev).unwrap();
    synchronize::stream_synchronize(&mut ctx, &mut sink, stream_b).unwrap();

    let got = bytes_to_f32s(&transfer::read_dtoh_async(&ctx, &mut sink, stream_b, d_out, n * 4).unwrap());
    let want: Vec<f32> = x.iter().map(|v| (2.0f32 * v) + 1.0).collect();
    assert_eq!(got, want, "consumer on B observed producer-on-A's result: o = 2*x + 1");

    // Honest handle validation: a bogus event / stream handle is a hard error, never a silent success.
    assert!(event::event_record(&mut ctx, Event(9999), stream_a).is_err(), "record on bad event errors");
    assert!(event::event_record(&mut ctx, ev, Stream(9999)).is_err(), "record on bad stream errors");
    assert!(event::stream_wait_event(&ctx, stream_b, Event(9999)).is_err(), "wait on bad event errors");
    assert!(event::event_query(&ctx, Event(9999)).is_err(), "query on bad event errors");

    // Clean teardown validates the destroy path too.
    event::event_destroy(&mut ctx, ev).unwrap();
    assert!(event::event_destroy(&mut ctx, ev).is_err(), "double-destroy is rejected");
}

// ==================================================================================================
// 3. async_overlap_correctness — async H2D + kernel + async D2H on a stream, synchronized; the final
//    host data is bit-exact. Overlap of the queued ops must not corrupt the result.
// ==================================================================================================

#[test]
fn async_overlap_produces_bit_exact_result() {
    let n = 4096usize;
    let a: Vec<i32> = (0..n).map(|i| (i as i32 * 3) - 1000).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, ISCALE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "iscale").unwrap();

    let stream: Stream = ctx.streams.create();
    let d_in = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let d_out = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();

    // async H2D → kernel (out = 5*in + 7) → async D2H, all on one stream, then a single stream barrier.
    transfer::memcpy_htod_async(&mut ctx, &mut sink, stream, d_in, &i32s_to_bytes(&a)).unwrap();
    let args = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_out), sc_i32(5), sc_i32(7), sc_i32(n as i32)];
    launch::launch(&mut ctx, &mut sink, func, (32, 1, 1), (128, 1, 1), &args).unwrap();
    let raw = transfer::read_dtoh_async(&ctx, &mut sink, stream, d_out, n * 4).unwrap();
    synchronize::stream_synchronize(&mut ctx, &mut sink, stream).unwrap();

    let got = bytes_to_i32s(&raw);
    let want: Vec<i32> = a.iter().map(|v| v.wrapping_mul(5).wrapping_add(7)).collect();
    assert_eq!(got, want, "async H2D/kernel/D2H overlap produced the exact result, uncorrupted");
    assert_eq!(sink.executor().dispatches, 1);
}

// ==================================================================================================
// 4. oom_alloc_rejected — an allocation past the modeled device budget returns the OOM code, mints no
//    state, and never fakes success. (Regression guard for the allocator minting impossible pointers.)
// ==================================================================================================

#[test]
fn oom_alloc_rejected_with_honest_code() {
    // A deliberately tiny device budget: 1 MiB of modeled VRAM.
    let budget: u64 = 1 << 20;
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(budget));

    // A within-budget allocation succeeds and is real.
    let ok = allocate::mem_alloc(&mut ctx, &mut sink, 4096).unwrap();
    assert_eq!(ctx.mem.len(), 1);
    transfer::memcpy_htod(&mut ctx, &mut sink, ok, &i32s_to_bytes(&[1, 2, 3, 4])).unwrap();

    // A single request larger than the whole device budget is rejected with the honest OOM status — NOT a
    // minted pointer into memory the host could never back (a fake success), and no null-deref.
    let err = allocate::mem_alloc(&mut ctx, &mut sink, budget + 1).unwrap_err();
    assert_eq!(
        result::cu_result_from_gpu_error(&err),
        result::CUDA_ERROR_OUT_OF_MEMORY,
        "over-budget cuMemAlloc → CUDA_ERROR_OUT_OF_MEMORY"
    );
    assert_eq!(
        result::cudart_from_gpu_error(&err),
        result::CUDART_ERROR_MEMORY_ALLOCATION,
        "over-budget cudaMalloc → cudaErrorMemoryAllocation"
    );
    assert_ne!(result::cu_result_from_gpu_error(&err), result::CUDA_SUCCESS, "NOT a faked success");

    // The rejected allocation minted NO state — still exactly one live allocation.
    assert_eq!(ctx.mem.len(), 1, "the rejected alloc left the allocation table untouched");

    // A cumulative over-budget request (would push total past the budget) is also rejected.
    let big = allocate::mem_alloc(&mut ctx, &mut sink, budget).unwrap_err();
    assert_eq!(result::cu_result_from_gpu_error(&big), result::CUDA_ERROR_OUT_OF_MEMORY);
    assert_eq!(ctx.mem.len(), 1);

    // …and the allocator is still healthy afterward: a fresh within-budget alloc still works.
    let ok2 = allocate::mem_alloc(&mut ctx, &mut sink, 4096).unwrap();
    assert_ne!(ok2, ok, "post-OOM allocation is a fresh, distinct pointer");
    assert_eq!(ctx.mem.len(), 2);
}

// ==================================================================================================
// 5. large_reduction_determinism — a large sum + max reduction, run twice, is identical bit-exact and
//    matches an independent CPU reference.
// ==================================================================================================

/// Run the grid-wide sum + max reduction over `input` once, returning `(sum, max)`.
fn run_reduction(input: &[i32]) -> (i32, i32) {
    let n = input.len();
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    // grid sized so grid.x * 128 >= n; the `i >= n` guard masks the tail lanes.
    let blocks = ((n + 127) / 128) as u32;

    let sum_mod = load_module::module_load_data(&mut ctx, REDUCE_SUM_PTX.as_bytes()).unwrap();
    let sum_fn = load_module::module_get_function(&ctx, sum_mod, "reduce_sum").unwrap();
    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(input));
    let d_sum = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_sum, &0i32.to_le_bytes()).unwrap();
    let sum_args = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_sum), sc_i32(n as i32)];
    launch::launch(&mut ctx, &mut sink, sum_fn, (blocks, 1, 1), (128, 1, 1), &sum_args).unwrap();
    let sum = bytes_to_i32s(&readback(&mut sink, &ctx, d_sum, 4))[0];

    let max_mod = load_module::module_load_data(&mut ctx, REDUCE_MAX_PTX.as_bytes()).unwrap();
    let max_fn = load_module::module_get_function(&ctx, max_mod, "reduce_max").unwrap();
    let d_max = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_max, &i32::MIN.to_le_bytes()).unwrap();
    let max_args = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_max), sc_i32(n as i32)];
    launch::launch(&mut ctx, &mut sink, max_fn, (blocks, 1, 1), (128, 1, 1), &max_args).unwrap();
    let max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, 4))[0];

    (sum, max)
}

#[test]
fn large_reduction_is_deterministic_and_matches_cpu() {
    let n = 100_000usize;
    // A mix of signs so the signed max and wrapping sum are both non-trivial.
    let input: Vec<i32> = (0..n).map(|i| ((i as i32).wrapping_mul(2654435761u32 as i32)) % 100_003 - 50_000).collect();

    let (sum1, max1) = run_reduction(&input);
    let (sum2, max2) = run_reduction(&input);

    // Determinism: two independent runs are bit-identical.
    assert_eq!((sum1, max1), (sum2, max2), "the reduction is deterministic across runs");

    // Correctness: matches an independent CPU reference (wrapping add is associative → order-independent).
    let cpu_sum = input.iter().fold(0i32, |acc, v| acc.wrapping_add(*v));
    let cpu_max = *input.iter().max().unwrap();
    assert_eq!(sum1, cpu_sum, "grid sum matches the CPU reference over {n} elements");
    assert_eq!(max1, cpu_max, "grid max matches the CPU reference over {n} elements");
}

// ==================================================================================================
// 6. concurrent_kernels_determinism — kernels across multiple streams produce identical bit-exact
//    output regardless of the order they are issued in.
// ==================================================================================================

/// Run three independent `iscale` kernels (one per stream/buffer) with the per-stream coefficients
/// `coeffs[s] = (k, c)`, issuing them in the order given by `order`. Returns the three output arrays.
fn run_three_streams(input: &[i32], coeffs: [(i32, i32); 3], order: [usize; 3]) -> [Vec<i32>; 3] {
    let n = input.len();
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, ISCALE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "iscale").unwrap();

    let streams: Vec<Stream> = (0..3).map(|_| ctx.streams.create()).collect();
    let d_in: Vec<DevicePtr> = (0..3).map(|_| upload(&mut sink, &mut ctx, &i32s_to_bytes(input))).collect();
    let d_out: Vec<DevicePtr> =
        (0..3).map(|_| allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap()).collect();

    // Issue the three kernels in the requested interleaving. A deterministic engine must not care.
    for &s in &order {
        let (k, c) = coeffs[s];
        transfer::memcpy_htod_async(&mut ctx, &mut sink, streams[s], d_in[s], &i32s_to_bytes(input)).unwrap();
        let args = vec![KernelArg::Ptr(d_in[s]), KernelArg::Ptr(d_out[s]), sc_i32(k), sc_i32(c), sc_i32(n as i32)];
        launch::launch(&mut ctx, &mut sink, func, (16, 1, 1), (128, 1, 1), &args).unwrap();
    }
    for &s in &streams {
        synchronize::stream_synchronize(&mut ctx, &mut sink, s).unwrap();
    }

    [
        bytes_to_i32s(&readback(&mut sink, &ctx, d_out[0], n * 4)),
        bytes_to_i32s(&readback(&mut sink, &ctx, d_out[1], n * 4)),
        bytes_to_i32s(&readback(&mut sink, &ctx, d_out[2], n * 4)),
    ]
}

#[test]
fn concurrent_kernels_are_order_independent_and_deterministic() {
    let n = 2048usize;
    let input: Vec<i32> = (0..n).map(|i| i as i32 - 1024).collect();
    let coeffs = [(2, 1), (3, -5), (7, 100)];

    // Same work, two different issue interleavings.
    let forward = run_three_streams(&input, coeffs, [0, 1, 2]);
    let reverse = run_three_streams(&input, coeffs, [2, 1, 0]);

    // Independent CPU reference per stream.
    let cpu: [Vec<i32>; 3] = std::array::from_fn(|s| {
        let (k, c) = coeffs[s];
        input.iter().map(|v| v.wrapping_mul(k).wrapping_add(c)).collect()
    });

    for s in 0..3 {
        assert_eq!(forward[s], cpu[s], "stream {s} forward-issue output matches CPU reference");
        assert_eq!(reverse[s], cpu[s], "stream {s} reverse-issue output matches CPU reference");
        assert_eq!(forward[s], reverse[s], "stream {s} output is identical regardless of issue order");
    }
}
