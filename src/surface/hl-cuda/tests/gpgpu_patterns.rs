//! CUDA **real-GPGPU-pattern** demo battery — the algorithm patterns real CUDA applications actually use
//! (tiled matmul, prefix-scan, atomic histogram, stencil convolution, segmented reduction, coalesced
//! transpose, bitonic sort), each driven through the REAL hl-cuda PTX front-end → the reference
//! [`CpuExecutor`] kernel-IR interpreter → device readback → asserted **bit-exact, element by element**
//! against an independent CPU reference computed in the test.
//!
//! Every kernel here exercises the genuine pattern — `.shared` workgroup memory, `bar.sync` barriers,
//! `atom`/`red` atomics under cross-block contention, multi-block grids with real global indexing and
//! remainder handling — and every input is integer (or exactly-representable), so the assertions are
//! bit-exact with no float tolerance. If a barrier failed to synchronize, if a block's shared memory
//! leaked into another block, if an atomic dropped an update, or if the grid indexing were wrong, these
//! assertions would catch it.
//!
//! Wiring is identical to `tests/compute_demo.rs` / `tests/advanced_demo.rs`: the same in-process
//! [`InProcessCommandSink`] over the [`CpuExecutor`] with the PTX compiler injected.
//!
//! Batteries:
//!   1. `tiled_matmul`     — shared-memory blocked (TILE=16) 64×64 integer matmul, C == A·B exact.
//!   2. `prefix_scan`      — multi-block Hillis-Steele inclusive scan (shared mem + double barrier per
//!      step) + host block-offset combine, exact inclusive AND exclusive scan.
//!   3. `histogram`        — atomic histogram into K bins, BOTH a global-atomic and a shared-privatized
//!      (shared atomics + merge) variant, exact bin counts under contention.
//!   4. `convolution`      — 1D box stencil with a shared-memory halo tile, 2D 3×3 box blur, and a 3×3
//!      Sobel |Gx|+|Gy|, each exact vs a CPU convolution.
//!   5. `reduce_segmented` — per-segment sum AND signed max via atomics into per-segment bins, exact.
//!   6. `transpose`        — shared-memory coalesced tiled matrix transpose (non-square, remainder), exact.
//!   7. `bitonic_sort`     — in-shared-memory bitonic sorting network (power-of-2 N) with a barrier per
//!      compare-exchange substep, exact vs a sorted reference.

use hl_cuda::adapter::ptx;
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::capability::{
    shader_payload, Capabilities, ALL_COMMANDS, COLOR_FORMATS,
};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{
    BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION,
};

// --------------------------------------------------------------------------------------------------
// shared harness — identical wiring to tests/compute_demo.rs.
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

fn readback(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &CudaContext,
    p: DevicePtr,
    len: usize,
) -> Vec<u8> {
    let (buf, off): (BufferId, u64) = ctx.device_location(p).unwrap();
    sink.read_buffer(buf, off, len).unwrap()
}

/// Allocate a device buffer and upload `bytes` to it (cuMemAlloc + cuMemcpyHtoD).
fn upload(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &mut CudaContext,
    bytes: &[u8],
) -> DevicePtr {
    let p = allocate::mem_alloc(ctx, sink, bytes.len() as u64).unwrap();
    transfer::memcpy_htod(ctx, sink, p, bytes).unwrap();
    p
}

/// Allocate a device buffer of `n` i32 slots, zero-initialised.
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

#[path = "gpgpu_patterns/convolution.rs"]
mod convolution;
#[path = "gpgpu_patterns/histogram.rs"]
mod histogram;
#[path = "gpgpu_patterns/matmul.rs"]
mod matmul;
#[path = "gpgpu_patterns/reduction.rs"]
mod reduction;
#[path = "gpgpu_patterns/scan.rs"]
mod scan;
#[path = "gpgpu_patterns/sort.rs"]
mod sort;
#[path = "gpgpu_patterns/special_register.rs"]
mod special_register;
#[path = "gpgpu_patterns/transpose.rs"]
mod transpose;
