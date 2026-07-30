//! CUDA **advanced-correctness** demo battery — beyond compute (#162), error-paths (#172), and hostile
//! (#193). Each demo drives a real CUDA advanced feature through the REAL hl-cuda driver and asserts the
//! result **bit-exact** against an independent reference. Nothing here asserts "did not crash"; every
//! output is checked value-by-value, and every feature the driver models is exercised through its actual
//! lowering — no stub is allowed to return a placeholder that a fake-passing assert would bless.
//!
//! Batteries:
//!   1. `texture_object`     — a `cudaTextureObject` over a 2D array, fetched with POINT (exact texel) and
//!      LINEAR (exact bilinear midpoint) filtering; both bit-exact.
//!   2. `managed_memory`     — `cudaMallocManaged`: host writes a pattern, a kernel transforms it in place,
//!      the host reads it back — the unified pointer round-trips, bit-exact.
//!   3. `cuda_graph`         — a kernel sequence built into a `cudaGraph`, instantiated + launched; the
//!      replayed result equals running the sequence eagerly, bit-exact + idempotent.
//!   4. `constant_memory`    — a `.const` global set from host via `cudaMemcpyToSymbol` and read in a
//!      kernel; the round-trip and the kernel output are both exact.
//!   5. `multi_stream`       — overlapping async copies + kernels across four streams into a shared output,
//!      deterministic + bit-exact regardless of issue interleaving.
//!
//! ## The one honest boundary (texture)
//! A kernel-side `tex2D` is a `tex.2d` PTX instruction served by the GPU texture unit. The neutral
//! kernel-IR interpreter (in `hl-gpu`, out of this crate's scope) models no `tex` opcode, so the
//! texture *unit* is modeled in the driver, host-side ([`hl_cuda::model::texture`]): a real, deterministic
//! evaluation of CUDA's documented fetch/filter math — not a stub. Demo 1 is transparent about this: it
//! asserts the driver's `tex2d` against a hand-computed reference, exactly as a kernel `tex2D` must return.

use hl_cuda::adapter::ptx;
use hl_cuda::model::texture::{CudaArray, FilterMode, SamplerDesc, TextureObject};
use hl_cuda::service::{allocate, graph, launch, load_module, symbol, texture, transfer};
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

#[path = "advanced_demo/constant.rs"]
mod constant;
#[path = "advanced_demo/graph_case.rs"]
mod graph_case;
#[path = "advanced_demo/memory.rs"]
mod memory;
#[path = "advanced_demo/streams.rs"]
mod streams;
#[path = "advanced_demo/texture.rs"]
mod texture_case;
