//! Deep **HOSTILE** robustness battery for the hl-cuda driver — the arithmetic-overflow / host-abort /
//! unbounded-allocation class. This is the CUDA counterpart to the executor + Vulkan hostile sweeps: the
//! error-CODE surface (bad launch dims, OOM budget) is already covered by `tests/robustness_demo.rs`
//! (task #172); this file targets the class that battery does NOT — inputs that, unguarded, would
//! `panic` / `add`-overflow (debug) / wrap / allocate a multi-GiB `Vec` on an attacker-controlled length.
//!
//! For every abuse we assert the driver returns the proper typed `GpuError` → `CUresult`
//! (`OutOfBounds`/`Invalid` → `CUDA_ERROR_INVALID_VALUE`, `ResourceLimit` → `CUDA_ERROR_OUT_OF_MEMORY`)
//! WITHOUT a panic, a wrap, or an unbounded host allocation — and then that a VALID call on the same
//! driver still works and computes the right bytes (the guards never poison the healthy path).
//!
//! Class covered (none duplicating #172's error-code demos):
//!   1. `cuMemcpyHtoD` — write past the allocation / interior overrun → `OutOfBounds`, never a silent OOB.
//!   2. `cuMemcpyDtoH` — `n` near `usize::MAX` → `OutOfBounds` BEFORE the sink's `vec![0u8; n]` (which
//!      would OOM-abort), plus interior/over-length reads.
//!   3. `cuMemcpyDtoD` — `size`/offsets near `u64::MAX` → checked (`soff + n` never add-overflows), both
//!      ends bounded.
//!   4. `cuMemsetD8/16/32` — huge element `count` → checked `width * count` + bounded fill (no multi-GiB
//!      `Vec`), pre-expanded `memset` bounded too.
//!   5. use-after-free / double-free / bogus handles across every copy/memset/free path → typed error.
//!   6. `*Async` copies/memset — stream validation AND the same bounds check both enforced.
//!   7. `cuMemAlloc` / `cuMemAllocPitch` / `cuMemAllocHost` — checked budget + checked pitch math + a
//!      bounded pinned-host allocation (no `vec![0u8; huge]`).
//!   8. `cuLaunchKernel` — a stale/never-created function handle and a dangling pointer arg never panic.

use hl_cuda::adapter::ptx;
use hl_cuda::service::{allocate, launch, load_module, synchronize, transfer};
use hl_cuda::model::stream::Stream;
use hl_cuda::{result, CudaContext, CudaDeviceDesc, DevicePtr, Function, KernelArg};

use hl_gpu::protocol::model::capability::{
    command_bits, format_bits, shader_payload, ALL_COMMANDS, COLOR_FORMATS,
};
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{
    BufferId, CommandSink, CpuExecutor, FeatureRequest, GpuError, InProcessCommandSink, WIRE_VERSION,
};

// --------------------------------------------------------------------------------------------------
// shared harness — identical wiring to tests/robustness_demo.rs (real CpuExecutor, so valid ops compute).
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

fn ctx() -> CudaContext {
    CudaContext::new(CudaDeviceDesc::apple_default(8 << 30))
}

fn readback(sink: &mut InProcessCommandSink<CpuExecutor>, ctx: &CudaContext, p: DevicePtr, len: usize) -> Vec<u8> {
    let (buf, off): (BufferId, u64) = transfer::memcpy_dtoh(ctx, p).unwrap();
    sink.read_buffer(buf, off, len).unwrap()
}

/// Assert `err` is the exact `OutOfBounds` typed error AND maps to the `CUDA_ERROR_INVALID_VALUE` a real
/// driver returns — never a faked success, never a panic having escaped.
fn assert_oob(err: &GpuError) {
    assert!(matches!(err, GpuError::OutOfBounds), "expected OutOfBounds, got {err:?}");
    assert_eq!(result::cu_result_from_gpu_error(err), result::CUDA_ERROR_INVALID_VALUE);
    assert_ne!(result::cu_result_from_gpu_error(err), result::CUDA_SUCCESS, "NOT a faked success");
}

// ==================================================================================================
// 1. cuMemcpyHtoD — a write that runs past the allocation end is OutOfBounds, never a silent OOB write
//    and never an `offset + len` add-overflow. Exact-fit (incl. interior) still works.
// ==================================================================================================

#[test]
fn htod_over_length_and_interior_overrun_are_out_of_bounds() {
    let mut sink = harness();
    let mut c = ctx();
    let p = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();

    // Exact-fit write of the whole 64-byte allocation succeeds.
    transfer::memcpy_htod(&mut c, &mut sink, p, &vec![0xABu8; 64]).unwrap();

    // One byte past the end → OutOfBounds (would be a silent OOB WriteBuffer without the guard).
    assert_oob(&transfer::memcpy_htod(&mut c, &mut sink, p, &vec![0u8; 65]).unwrap_err());

    // An interior pointer whose write overruns the allocation end (48 + 32 > 64) → OutOfBounds.
    assert_oob(&transfer::memcpy_htod(&mut c, &mut sink, DevicePtr(p.0 + 48), &vec![0u8; 32]).unwrap_err());

    // An interior write that exactly reaches the end (48 + 16 == 64) still works.
    transfer::memcpy_htod(&mut c, &mut sink, DevicePtr(p.0 + 48), &vec![0u8; 16]).unwrap();

    // The rejected writes touched nothing: [0,48) is still 0xAB, only the accepted tail write cleared [48,64).
    let got = readback(&mut sink, &c, p, 64);
    assert!(got[..48].iter().all(|&b| b == 0xAB), "the head is untouched by the rejected OOB writes");
    assert!(got[48..].iter().all(|&b| b == 0), "only the in-bounds tail write landed");
}

// ==================================================================================================
// 2. cuMemcpyDtoH — a huge `n` must be bounded against the source allocation BEFORE the sink's
//    `vec![0u8; n]` readback buffer is allocated (that path would OOM-abort on n≈usize::MAX). Interior
//    and over-length reads are OutOfBounds; exact/interior-fit reads return the correct bytes.
// ==================================================================================================

#[test]
fn dtoh_huge_len_is_bounded_before_any_readback_allocation() {
    let mut sink = harness();
    let mut c = ctx();
    let p = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    let payload: Vec<u8> = (0..64u8).collect();
    transfer::memcpy_htod(&mut c, &mut sink, p, &payload).unwrap();

    // n = usize::MAX: without the up-front bound, `read_buffer` allocates `vec![0u8; usize::MAX]` and the
    // process aborts. The driver bounds `n` against the 64-byte source first → OutOfBounds, no allocation.
    assert_oob(&transfer::read_dtoh(&c, &mut sink, p, usize::MAX).unwrap_err());
    // One byte past the allocation end.
    assert_oob(&transfer::read_dtoh(&c, &mut sink, p, 65).unwrap_err());
    // Interior read overrunning the end (40 + 25 > 64).
    assert_oob(&transfer::read_dtoh(&c, &mut sink, DevicePtr(p.0 + 40), 25).unwrap_err());

    // Exact and interior-fit reads return the true device bytes.
    assert_eq!(transfer::read_dtoh(&c, &mut sink, p, 64).unwrap(), payload);
    assert_eq!(transfer::read_dtoh(&c, &mut sink, DevicePtr(p.0 + 40), 24).unwrap(), payload[40..].to_vec());
}

// ==================================================================================================
// 3. cuMemcpyDtoD — `size`/offsets near u64::MAX are checked: `soff + n` / `doff + n` never add-overflow,
//    both source and destination are bounded, and a valid on-device copy still moves the right bytes.
// ==================================================================================================

#[test]
fn dtod_size_and_offsets_near_u64_max_are_checked_not_overflowed() {
    let mut sink = harness();
    let mut c = ctx();
    let a = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 128).unwrap();
    let a_bytes: Vec<u8> = (0..256).map(|i| (i & 0xff) as u8).collect();
    transfer::memcpy_htod(&mut c, &mut sink, a, &a_bytes).unwrap();

    // n = u64::MAX: a naive `soff + n` / `doff + n` bounds check would add-overflow (debug panic). The
    // guard compares against the (small) remaining bytes → OutOfBounds, no arithmetic overflow.
    assert_oob(&transfer::memcpy_dtod(&mut c, &mut sink, b, a, u64::MAX).unwrap_err());
    // n exceeds the DESTINATION (b is 128) even though the source could supply it.
    assert_oob(&transfer::memcpy_dtod(&mut c, &mut sink, b, a, 200).unwrap_err());
    // n exceeds the SOURCE measured from an interior source pointer (a+200 leaves only 56 bytes).
    assert_oob(&transfer::memcpy_dtod(&mut c, &mut sink, b, DevicePtr(a.0 + 200), 100).unwrap_err());

    // A valid interior→base copy of exactly 128 bytes works and moves a[16..144] into b.
    transfer::memcpy_dtod(&mut c, &mut sink, b, DevicePtr(a.0 + 16), 128).unwrap();
    assert_eq!(readback(&mut sink, &c, b, 128), a_bytes[16..144].to_vec());
}

// ==================================================================================================
// 4. cuMemsetD8/16/32 — a huge element `count` is bounded: `width * count` is a checked product, and the
//    fill is bounded against the destination BEFORE the `Vec` is built (no `width*n` overflow, no
//    multi-GiB fill). The pre-expanded `memset` is bounded too. Exact-fit fills work.
// ==================================================================================================

#[test]
fn memset_huge_count_is_checked_and_bounded_no_giant_vec() {
    let mut sink = harness();
    let mut c = ctx();
    let p = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();

    // width * count overflows u64 → checked_mul → OutOfBounds (never a wrap that under-allocates, never a
    // debug add/mul-overflow panic).
    assert_oob(&transfer::memset_elements(&mut c, &mut sink, p, 0xDEAD_BEEF, 4, usize::MAX).unwrap_err());
    // A product that does NOT overflow but far exceeds the 64-byte allocation (4 GiB) → OutOfBounds, and
    // crucially bounded BEFORE any `vec![0u8; 4GiB]` fill is attempted.
    assert_oob(&transfer::memset_elements(&mut c, &mut sink, p, 0, 4, 1 << 30).unwrap_err());
    // A bad element width is a typed error, not a slice-panic.
    assert!(transfer::memset_elements(&mut c, &mut sink, p, 0, 9, 1).is_err());
    // The pre-expanded byte-pattern memset is bounded against the allocation too.
    assert_oob(&transfer::memset(&mut c, &mut sink, p, &vec![0u8; 65]).unwrap_err());

    // Exact-fit fill: 16 × u32 = 64 bytes of 0x01010101 works and lands byte-for-byte.
    transfer::memset_elements(&mut c, &mut sink, p, 0x0101_0101, 4, 16).unwrap();
    assert_eq!(readback(&mut sink, &c, p, 64), vec![1u8; 64]);
}

// ==================================================================================================
// 5. use-after-free / double-free / bogus handles — every copy/memset/free path rejects a freed or
//    never-created device pointer with a typed error (safe no-op / hard error), never a panic.
// ==================================================================================================

#[test]
fn freed_and_bogus_pointers_are_rejected_across_every_path() {
    let mut sink = harness();
    let mut c = ctx();
    let p = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    allocate::mem_free(&mut c, &mut sink, p).unwrap();

    // Use-after-free on every path is a hard error (the pointer no longer resolves).
    assert!(transfer::memcpy_htod(&mut c, &mut sink, p, &[1, 2, 3, 4]).is_err());
    assert!(transfer::read_dtoh(&c, &mut sink, p, 4).is_err());
    assert!(transfer::memcpy_dtod(&mut c, &mut sink, p, p, 4).is_err());
    assert!(transfer::memset(&mut c, &mut sink, p, &[0u8; 4]).is_err());
    assert!(transfer::memset_elements(&mut c, &mut sink, p, 0, 4, 1).is_err());

    // Double free and a never-created pointer are rejected.
    assert!(allocate::mem_free(&mut c, &mut sink, p).is_err(), "double free rejected");
    let bogus = DevicePtr(0x9999_0000);
    assert!(allocate::mem_free(&mut c, &mut sink, bogus).is_err());
    assert!(transfer::memcpy_htod(&mut c, &mut sink, bogus, &[0u8; 4]).is_err());
    assert!(transfer::read_dtoh(&c, &mut sink, bogus, 4).is_err());
    assert!(transfer::memset_elements(&mut c, &mut sink, bogus, 0, 4, 1).is_err());

    // The allocator is still healthy: a fresh alloc + valid copy still works.
    let q = allocate::mem_alloc(&mut c, &mut sink, 16).unwrap();
    transfer::memcpy_htod(&mut c, &mut sink, q, &vec![9u8; 16]).unwrap();
    assert_eq!(readback(&mut sink, &c, q, 16), vec![9u8; 16]);
}

// ==================================================================================================
// 6. *Async copies/memset — a bogus stream is rejected AND the same bounds/overflow guard applies. A
//    valid stream + in-bounds op still works and synchronizes cleanly.
// ==================================================================================================

#[test]
fn async_ops_enforce_both_stream_validation_and_bounds() {
    let mut sink = harness();
    let mut c = ctx();
    let p = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    let good = c.streams.create();
    let bad = Stream(4242);

    // Bogus stream → invalid handle on each async path.
    assert!(transfer::memcpy_htod_async(&mut c, &mut sink, bad, p, &[0u8; 4]).is_err());
    assert!(transfer::read_dtoh_async(&c, &mut sink, bad, p, 4).is_err());
    assert!(transfer::memset_elements_async(&mut c, &mut sink, bad, p, 0, 4, 1).is_err());

    // Valid stream but out-of-bounds / overflowing length → OutOfBounds (bounds enforced on async too).
    assert_oob(&transfer::memcpy_htod_async(&mut c, &mut sink, good, p, &vec![0u8; 65]).unwrap_err());
    assert_oob(&transfer::memset_elements_async(&mut c, &mut sink, good, p, 0, 4, usize::MAX).unwrap_err());

    // A valid async fill + sync works and lands.
    transfer::memset_elements_async(&mut c, &mut sink, good, p, 0x0707_0707, 4, 16).unwrap();
    synchronize::stream_synchronize(&mut c, &mut sink, good).unwrap();
    assert_eq!(readback(&mut sink, &c, p, 64), vec![7u8; 64]);
}

// ==================================================================================================
// 7. allocation math — cuMemAlloc budget (checked_add), cuMemAllocPitch pitch math (checked add + mul),
//    cuMemAllocHost pinned-host bound (no `vec![0u8; huge]`). All return typed errors, none overflow.
// ==================================================================================================

#[test]
fn allocation_paths_use_checked_math_and_bounded_host_allocation() {
    let mut sink = harness();
    let mut c = ctx();

    // A single u64::MAX device alloc: `used + size` is a checked add in the budget guard → OOM, no wrap.
    let err = allocate::mem_alloc(&mut c, &mut sink, u64::MAX).unwrap_err();
    assert_eq!(result::cu_result_from_gpu_error(&err), result::CUDA_ERROR_OUT_OF_MEMORY);

    // Pitch math: width*height product overflow → typed error.
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, u64::MAX / 2, u64::MAX / 2, 4).is_err());
    // Pitch alignment overflow: `width_bytes + 511` near u64::MAX → checked add → typed error (would be a
    // debug add-overflow panic without the guard).
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, u64::MAX - 10, 4, 4).is_err());
    // Zero extents still rejected.
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, 0, 8, 4).is_err());

    // Pinned-host allocation is bounded against the budget BEFORE `vec![0u8; size]`: an over-budget /
    // near-usize::MAX size returns None (OOM analogue), never a multi-GiB host allocation / abort.
    let mut small = CudaContext::new(CudaDeviceDesc::apple_default(1 << 20)); // 1 MiB budget
    assert!(allocate::host_alloc(&mut small, (1 << 20) + 1).is_none(), "over-budget pinned host → None");
    assert!(allocate::host_alloc(&mut small, usize::MAX).is_none(), "usize::MAX pinned host → None");
    // A within-budget pinned host allocation is real and usable.
    let base = allocate::host_alloc(&mut small, 4096).expect("within-budget host alloc");
    assert!(base != 0);
    allocate::host_free(&mut small, base).unwrap();
}

// ==================================================================================================
// 8. cuLaunchKernel — a stale / never-created function handle and a dangling pointer arg must never
//    panic; they surface a typed error and dispatch nothing, and a valid launch on the same driver still
//    computes the real result.
// ==================================================================================================

const AFFINE_PTX: &str = r#"
    .visible .entry affine(
        .param .u64 af_in, .param .u64 af_out, .param .f32 af_a, .param .f32 af_b, .param .u32 af_n
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

#[test]
fn launch_with_stale_or_dangling_handles_never_panics_but_a_valid_launch_computes() {
    let mut sink = harness();
    let mut c = ctx();
    let n = 64usize;
    let x: Vec<f32> = (0..n).map(|i| i as f32).collect();
    let xb: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();

    let dx = allocate::mem_alloc(&mut c, &mut sink, (n * 4) as u64).unwrap();
    transfer::memcpy_htod(&mut c, &mut sink, dx, &xb).unwrap();
    let dy = allocate::mem_alloc(&mut c, &mut sink, (n * 4) as u64).unwrap();
    let args = vec![
        KernelArg::Ptr(dx),
        KernelArg::Ptr(dy),
        KernelArg::Scalar(2.0f32.to_le_bytes().to_vec()),
        KernelArg::Scalar(1.0f32.to_le_bytes().to_vec()),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];

    // A never-created function handle (module/entry that were never loaded) must not panic — it surfaces
    // a typed error and dispatches nothing (never a faked success on an empty/garbage kernel).
    let stale = Function { module: 9999, entry: 7 };
    let r = launch::launch(&mut c, &mut sink, stale, (1, 1, 1), (64, 1, 1), &args);
    assert!(r.is_err(), "stale function handle is a typed error, got {r:?}");
    assert_eq!(sink.executor().dispatches, 0, "the stale launch dispatched nothing");

    // A dangling pointer argument (freed) is likewise a hard error that dispatches nothing.
    let dead = allocate::mem_alloc(&mut c, &mut sink, (n * 4) as u64).unwrap();
    allocate::mem_free(&mut c, &mut sink, dead).unwrap();
    let module = load_module::module_load_data(&mut c, AFFINE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&c, module, "affine").unwrap();
    let bad_args = vec![
        KernelArg::Ptr(dead), // dangling
        KernelArg::Ptr(dy),
        KernelArg::Scalar(2.0f32.to_le_bytes().to_vec()),
        KernelArg::Scalar(1.0f32.to_le_bytes().to_vec()),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    assert!(launch::launch(&mut c, &mut sink, func, (1, 1, 1), (64, 1, 1), &bad_args).is_err());
    assert_eq!(sink.executor().dispatches, 0, "the dangling-arg launch dispatched nothing");

    // A VALID launch on the same driver still computes the real affine result: y = 2*x + 1.
    launch::launch(&mut c, &mut sink, func, (1, 1, 1), (64, 1, 1), &args).unwrap();
    assert_eq!(sink.executor().dispatches, 1);
    let raw = readback(&mut sink, &c, dy, n * 4);
    let got: Vec<f32> = raw.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
    let want: Vec<f32> = x.iter().map(|v| 2.0f32.mul_add(*v, 1.0)).collect();
    assert_eq!(got, want, "the valid launch is unaffected by the rejected hostile launches");
}
