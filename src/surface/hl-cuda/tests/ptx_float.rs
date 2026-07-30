//! Floating-point comparison, conversion and memory-fence lowering in the PTX front end.
//!
//! Every case here runs through the REAL front end and (where it computes) the reference [`CpuExecutor`],
//! reading its operands from device memory so nothing folds at compile time. The values are chosen so a
//! wrong lowering produces a DIFFERENT NUMBER, not merely a different instruction:
//!   * comparison — negative operands, where an integer compare of f32 bit patterns inverts, plus NaN,
//!     where the ordered and unordered PTX families disagree;
//!   * conversion — `0x80000000`, which a signed kind reads as negative, and `2.5`/`3.5`, which round to
//!     2 and 4 under ties-to-even so neither truncation nor round-half-up reproduces both;
//!   * fence — the lowered `Inst::Fence` scope, which a `Nop` lowering discards entirely.
//!
//! Each rejection asserts its valid neighbour still compiles, so no fix becomes a blanket refusal.

use hl_cuda::adapter::ptx;
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::capability::{shader_payload, Capabilities, COLOR_FORMATS};
use hl_gpu::protocol::model::command::etag;
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::kernel::{mem_scope, Inst, KernelDescriptor};
use hl_gpu::{
    CommandSink, CpuExecutor, FeatureRequest, GpuError, InProcessCommandSink, WIRE_VERSION,
};

/// The encoder commands the CUDA lowering emits: a compute pass with a dispatch, plus the on-device buffer
/// copy `cuMemcpyDtoD` lowers to.
const CUDA_COMMANDS: &[u8] = &[
    etag::BEGIN_COMPUTE_PASS,
    etag::END_COMPUTE_PASS,
    etag::DISPATCH,
    etag::COPY_B2B,
];

fn harness() -> InProcessCommandSink<CpuExecutor> {
    let mut exec = CpuExecutor::new();
    exec.set_kernel_compiler(|desc: &KernelDescriptor| {
        ptx::compile(&desc.ptx, &desc.entry, desc.block)
    });
    let mut sink = InProcessCommandSink::new(exec);
    let request = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: Capabilities::command_bits(CUDA_COMMANDS),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
        ..FeatureRequest::default()
    };
    sink.negotiate(&request)
        .expect("negotiate against CpuExecutor");
    sink
}

/// Compile `body` as the single entry `k` and report the front end's verdict.
fn compile(body: &str) -> Result<(), GpuError> {
    let src = format!(".visible .entry k(.param .u64 k_param_0) {{\n{body}\n}}");
    ptx::compile(&src, "k", [1, 1, 1]).map(|_| ())
}

fn assert_rejected(body: &str) {
    match compile(body) {
        Err(GpuError::Kernel(_)) => {}
        other => panic!("`{body}` must be a typed kernel error, got {other:?}"),
    }
}

/// Run a two-input elementwise kernel: upload `a` and `b`, launch one thread per element, read the output
/// back as raw little-endian words.
fn run(source: &str, entry: &str, a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(source.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, entry).unwrap();

    let n = a.len();
    let bytes = (n * 4) as u64;
    let upload =
        |ctx: &mut CudaContext, sink: &mut InProcessCommandSink<CpuExecutor>, v: &[u32]| {
            let raw: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
            let p: DevicePtr = allocate::mem_alloc(ctx, sink, bytes).unwrap();
            transfer::memcpy_htod(ctx, sink, p, &raw).unwrap();
            p
        };
    let pa = upload(&mut ctx, &mut sink, a);
    let pb = upload(&mut ctx, &mut sink, b);
    let out = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    transfer::memset(&mut ctx, &mut sink, out, &vec![0xAA; n * 4]).unwrap();

    let args = [
        KernelArg::Ptr(pa),
        KernelArg::Ptr(pb),
        KernelArg::Ptr(out),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, 1),
        (n as u32, 1, 1),
        &args,
    )
    .unwrap();
    transfer::read_dtoh(&ctx, &mut sink, out, n * 4)
        .unwrap()
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

#[path = "ptx_float/compare.rs"]
mod compare;
#[path = "ptx_float/convert.rs"]
mod convert;
#[path = "ptx_float/fence.rs"]
mod fence;
