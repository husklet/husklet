//! CUDA **real-GPGPU-algorithm** demo battery — a second tier of genuine parallel algorithms (sort,
//! spectral transform, graph traversal, sparse linear algebra, stream compaction, Monte-Carlo, k-means,
//! running reductions) beyond the primitive-pattern battery in `tests/gpgpu_patterns.rs`. Each is driven
//! through the REAL hl-cuda PTX front-end → the reference [`CpuExecutor`] kernel-IR interpreter → device
//! readback → asserted **bit-exact, element by element** against an independent CPU reference computed in
//! the test.
//!
//! Everything is integer or fixed-point (the DFT uses an integer twiddle table; Monte-Carlo uses a
//! deterministic integer LCG — NO clock, NO float rng), so the assertions carry ZERO float tolerance. The
//! kernels exercise the structure real applications rely on: multi-pass ping-pong grids (merge sort),
//! irregular CSR / scatter access (SpMV, BFS, compaction), cross-block atomics (BFS frontier count,
//! k-means accumulation, Monte-Carlo hit count), power-of-two modular twiddle indexing (DFT), and shared
//! memory + `bar.sync` running-reduction scans (prefix min/max). Every reference is non-degenerate and
//! every sort/compaction asserts its input was NOT already in the answer shape (anti-false-pass guards).
//!
//! Wiring is identical to `tests/gpgpu_patterns.rs`: the same in-process [`InProcessCommandSink`] over the
//! [`CpuExecutor`] with the PTX compiler injected.
//!
//! Batteries:
//!   1. `merge_sort`       — iterative bottom-up multi-block merge sort of N u32 keys, ping-pong buffers,
//!      one kernel launch per doubling pass. Exact vs a stable-sorted reference.
//!   2. `dft_fixed_point`  — fixed-point DFT of an N=16 signal with an INTEGER twiddle table and power-of-two
//!      modular index `(k·n)&(N−1)`. Exact real/imag bins vs the same integer DFT on CPU.
//!   3. `bfs_frontier`     — one BFS level-expansion step (pull model) over a symmetric CSR graph, with a
//!      cross-block atomic frontier count. Exact new `dist`, frontier mask, and count.
//!   4. `spmv_csr`         — sparse matrix–vector product in CSR (ragged rows incl. an empty row). Exact.
//!   5. `stream_compaction`— predicate → multi-block inclusive prefix scan → scatter, compacting the
//!      elements that pass. Exact compacted prefix + exact count.
//!   6. `monte_carlo_pi`   — deterministic per-thread LCG (seeded by thread index) → integer count of
//!      samples inside the quarter circle, summed by atomics. Exact hit count vs the
//!      identical LCG replayed on CPU (bit-exact count, NOT a statistical π).
//!   7. `kmeans_step`      — one k-means iteration: assign each point to its nearest of K centroids by
//!      integer L2, then atomically accumulate per-cluster coordinate sums + counts.
//!      Exact assignment, sums, and counts.
//!   8. `running_minmax`   — inclusive Hillis-Steele running min AND running max scans in shared memory
//!      (a barrier per doubling step). Exact prefix-min and prefix-max.

use hl_cuda::adapter::ptx;
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::command::etag;
use hl_gpu::protocol::model::capability::{
    shader_payload, Capabilities, COLOR_FORMATS,
};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{
    BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION,
};

/// The encoder commands the CUDA lowering actually emits: a compute pass with a dispatch, plus the
/// on-device buffer copy `cuMemcpyDtoD` lowers to. A CUDA driver encodes no render pass and no texture
/// copy, so negotiating the full command set would claim a surface this driver never uses — and would be
/// refused by any executor that honestly advertises less than everything.
const CUDA_COMMANDS: &[u8] = &[
    etag::BEGIN_COMPUTE_PASS,
    etag::END_COMPUTE_PASS,
    etag::DISPATCH,
    etag::COPY_B2B,
];


// --------------------------------------------------------------------------------------------------
// shared harness — identical wiring to tests/gpgpu_patterns.rs.
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
        command_bits: Capabilities::command_bits(CUDA_COMMANDS),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
        ..FeatureRequest::default()
    };
    sink.negotiate(&req).expect("negotiate against CpuExecutor");
    sink
}

fn i32s_to_bytes(v: &[i32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn bytes_to_i32s(raw: &[u8]) -> Vec<i32> {
    raw.chunks_exact(4)
        .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}
fn u32s_to_bytes(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn bytes_to_u32s(raw: &[u8]) -> Vec<u32> {
    raw.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
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

fn alloc_zeroed_i32(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &mut CudaContext,
    n: usize,
) -> DevicePtr {
    let p = allocate::mem_alloc(ctx, sink, (n * 4) as u64).unwrap();
    transfer::memset(ctx, sink, p, &vec![0u8; n * 4]).unwrap();
    p
}

fn sc(v: i32) -> KernelArg {
    KernelArg::Scalar(v.to_le_bytes().to_vec())
}

// ==================================================================================================
// 1. merge_sort — iterative bottom-up multi-block merge sort of N u32 keys. Pass p merges adjacent
//    sorted runs of length `runlen = 1<<p` into runs of length `2·runlen`: each thread owns one merge
//    task (a pair of adjacent runs), does a sequential two-pointer merge into the OTHER buffer, and the
//    host ping-pongs the two device buffers between passes. Multi-block: at `runlen=1` there are N/2 merge
//    tasks spread across many blocks. Stable (`<=` keeps the left/earlier element first). Unsigned key
//    comparison (`setp.le.u32`) so keys above i32::MAX sort correctly.
// ==================================================================================================

#[path = "gpgpu_algorithms/bfs.rs"]
mod bfs;
#[path = "gpgpu_algorithms/compaction.rs"]
mod compaction;
#[path = "gpgpu_algorithms/dft.rs"]
mod dft;
#[path = "gpgpu_algorithms/kmeans.rs"]
mod kmeans;
#[path = "gpgpu_algorithms/merge.rs"]
mod merge;
#[path = "gpgpu_algorithms/minmax.rs"]
mod minmax;
#[path = "gpgpu_algorithms/monte_carlo.rs"]
mod monte_carlo;
#[path = "gpgpu_algorithms/spmv.rs"]
mod spmv;
