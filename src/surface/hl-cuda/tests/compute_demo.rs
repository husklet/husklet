//! CUDA **compute-correctness** demo battery: real CUDA kernels driven through the REAL hl-cuda driver
//! lowering → an in-process [`InProcessCommandSink`] over the reference [`CpuExecutor`] → the device
//! buffer read back → asserted **EXACTLY, element by element** against an independent CPU reference.
//!
//! Every test here follows the exact seam `tests/e2e.rs` established (module-load / mem-alloc /
//! memcpy-HtoD / launch / memcpy-DtoH), but each demo COMPUTES a non-trivial result and checks every
//! output element — never "did not crash". If the software path had a lowering / param-marshalling /
//! grid-mapping / shared-memory bug, these assertions would catch it.
//!
//! Batteries:
//!   1. `saxpy`        — `y[i] = a*x[i] + y[i]`, multi-block grid, every element vs an `fma` reference.
//!   2. `reduction`    — sum AND max of an N-element array across a MULTI-BLOCK grid via global atomics
//!                       (`red.global.add` / `red.global.max`), asserting the exact cross-block total.
//!   3. `matmul`       — MxK · KxN → MxN over a 2D block/grid, vs a CPU triple-loop (`fma`), per element.
//!   4. `elementwise`  — `mul` / `add` (f32) and `min` (s32, branch-selected) over two arrays, exact.
//!   5. `strided/2D`   — copy a sub-rectangle out of a wider row-major source, exact resulting layout.
//!   6. `shared+sync`  — block-scoped tree reduction in `.shared` memory with `bar.sync`, one partial per
//!                       block over a multi-block grid; each partial AND the host-summed total exact.

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
// shared harness — identical wiring to tests/e2e.rs: the reference CpuExecutor with the PTX front-end
// injected + the capability handshake a socketed driver would negotiate before its first submit.
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

#[path = "compute_demo/copy.rs"]
mod copy;
#[path = "compute_demo/elementwise.rs"]
mod elementwise;
#[path = "compute_demo/matmul.rs"]
mod matmul;
#[path = "compute_demo/reduction.rs"]
mod reduction;
#[path = "compute_demo/saxpy.rs"]
mod saxpy;
#[path = "compute_demo/shared_memory.rs"]
mod shared_memory;
