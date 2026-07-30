//! CUDA **real-library-shape** demo battery — the exact kernel shapes production ML/HPC libraries
//! (cuBLAS strided-batched GEMM, cuDNN conv/pool/softmax/normalization, embedding/im2col front-ends)
//! actually dispatch, each driven through the REAL hl-cuda PTX front-end → the reference
//! [`CpuExecutor`] kernel-IR interpreter → device readback → asserted **bit-exact, element by element**
//! against an independent CPU reference computed in the test.
//!
//! Every kernel exercises the genuine structure of its library counterpart — batch strides, NCHW /
//! im2col indexing with valid-padding remainders, `.shared` tiles + `bar.sync`, cross-block `red.*`
//! atomics (max / min / add), multi-block grids — and every value is integer / fixed-point, so the
//! assertions are bit-exact with NO float tolerance. A mis-strided batch, a dropped channel in the
//! accumulation, an off-by-one in the pooling window, a barrier that failed to fence the row reductions,
//! or an atomic that lost an update would each fail these assertions.
//!
//! Wiring is identical to `tests/gpgpu_patterns.rs` / `tests/compute_demo.rs`: the same in-process
//! [`InProcessCommandSink`] over the [`CpuExecutor`] with the PTX compiler injected.
//!
//! Batteries:
//!   1. `batched_strided_gemm` — B independent M×K·K×N GEMMs at a batch stride (the cuBLAS
//!      `cublasGemmStridedBatched` shape), shared-memory TILE=16 blocked. Exact per batch.
//!   2. `conv2d_nchw`          — a cuDNN conv layer (N=1, C_in=3, C_out=4, 3×3, NCHW, valid pad), exact
//!      multi-channel accumulation vs a CPU conv.
//!   3. `pool2x2`              — max-pool AND avg-pool, 2×2 stride-2 (the cuDNN pooling shape), exact.
//!   4. `softmax_rowwise`      — numerically-stable per-row softmax in fixed point: subtract the row max,
//!      base-2 fixed-point exponential (`(1<<Q) >> (max−x)`), row-sum denominator — the two exact stages
//!      of a stable softmax (the final normalize-divide is the sole float step, omitted). Exact per row.
//!   5. `layernorm_stats`      — per-row LayerNorm statistics in fixed point: row mean (N a power of two →
//!      exact arithmetic-shift), centered residual `x−mean`, and variance `Σ(x−mean)² / N`. Exact.
//!   6. `relu_gelu`            — ReLU and a fixed-point GELU-style cubic, elementwise over a multi-block
//!      grid, each exact vs the identical integer polynomial on CPU.
//!   7. `gemv_argmax`          — matrix–vector `y = A·x` then a large cross-block reduction: the max of y
//!      (`red.global.max`) and its arg-max index (`red.global.min` over the indices that hit the max).
//!      Exact y, exact max, exact lowest-index arg-max.
//!   8. `im2col` + `embedding` — the im2col lowering front-end (valid-pad patch gather) and an embedding
//!      table gather, each a pure exact index remap vs CPU.

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
        command_bits: Capabilities::command_bits(ALL_COMMANDS),
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

#[path = "library_shapes/activation.rs"]
mod activation;
#[path = "library_shapes/convolution.rs"]
mod convolution;
#[path = "library_shapes/gemm.rs"]
mod gemm;
#[path = "library_shapes/gemv.rs"]
mod gemv;
#[path = "library_shapes/im2col.rs"]
mod im2col;
#[path = "library_shapes/layernorm.rs"]
mod layernorm;
#[path = "library_shapes/pool.rs"]
mod pool;
#[path = "library_shapes/softmax.rs"]
mod softmax;
