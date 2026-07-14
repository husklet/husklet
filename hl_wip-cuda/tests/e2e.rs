//! End-to-end: a CUDA `vecadd` driven through the REAL hl-cuda lowering services, an in-process
//! [`InProcessCommandSink`] over the reference [`CpuExecutor`], and the whole host runtime pipeline —
//! then the COMPUTED output is read back and asserted elementwise.
//!
//! Unlike `tests/lowering.rs` (which asserts the emitted `Cmd` stream against a `RecordingSink`) and
//! `hl-gpu`'s conformance suite (which hand-builds the `KernelProgram` and injects it via
//! `define_kernel`), this test exercises the FULL seam with nothing hand-fed on the execution side:
//!
//!   cuModuleLoadData(VECADD_PTX) → cuModuleGetFunction → cuMemAlloc ×3 → cuMemcpyHtoD ×2 →
//!   cuLaunchKernel  ──lowers to──▶  protocol Cmds  ──submit──▶  InProcessCommandSink
//!        └▶ runtime validate → account → dispatch → CpuExecutor::execute
//!              └▶ CreateShader decodes the KERNEL payload → the injected PTX front-end compiles it
//!                 → the kernel interpreter runs it → writes the output buffer
//!   cuMemcpyDtoH(out) → read the output buffer back off the sink → assert c[i] == a[i] + b[i].
//!
//! The only injected wiring is the kernel FRONT-END (`hl_cuda::adapter::ptx::compile`): the PTX parser
//! is a driver concern the neutral `hl-gpu` crate never links, so the composition root hands it to the
//! executor. Everything else is the real code path.

use hl_cuda::adapter::ptx;
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION};
use hl_gpu::protocol::model::capability::{command_bits, format_bits, shader_payload, ALL_COMMANDS, COLOR_FORMATS};

fn f32s_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

#[test]
fn cuda_vecadd_runs_end_to_end_and_reads_back_the_elementwise_sum() {
    let a = [1.0f32, 2.0, 3.0, 4.0];
    let b = [10.0f32, 20.0, 30.0, 40.0];
    let n = a.len() as u32;

    // --- host side: the reference executor + the in-process sink -------------------------------------
    // Inject the PTX front-end so a driver-produced `CreateShader { PtxKernel, .. }` (whose payload is a
    // `KernelDescriptor` carrying the PTX source) compiles for real — no `define_kernel`.
    let mut exec = CpuExecutor::new();
    exec.set_kernel_compiler(|desc: &KernelDescriptor| ptx::compile(&desc.ptx, &desc.entry, desc.block));
    let mut sink = InProcessCommandSink::new(exec);

    // Capability handshake against the executor, exactly as a socketed driver would negotiate first.
    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: command_bits(ALL_COMMANDS),
        texture_formats: format_bits(COLOR_FORMATS),
    };
    let caps = sink.negotiate(&req).expect("negotiate against CpuExecutor");
    assert!(caps.supports_shader_payload(shader_payload::KERNEL));

    // --- guest side: the real CUDA driver services --------------------------------------------------
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    // cuModuleLoadData(PTX) + cuModuleGetFunction("vecadd").
    let module = load_module::module_load_data(&mut ctx, ptx::VECADD_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "vecadd").unwrap();

    // cuMemAlloc for the two inputs and the output (4 f32 = 16 bytes each).
    let bytes = (n as u64) * 4;
    let da = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    let db = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();

    // cuMemcpyHtoD the two input vectors.
    transfer::memcpy_htod(&mut ctx, &mut sink, da, &f32s_to_bytes(&a)).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, db, &f32s_to_bytes(&b)).unwrap();

    // cuLaunchKernel: grid = 1 block, block = n threads → the compiled kernel bakes block=[n,1,1] and one
    // workgroup of n threads computes c[0..n].
    let args = vec![
        KernelArg::Ptr(da),
        KernelArg::Ptr(db),
        KernelArg::Ptr(dc),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (n, 1, 1), &args).unwrap();

    // --- readback: cuMemcpyDtoH resolves the output pointer; read it back off the sink ---------------
    let (out_buf, off): (BufferId, u64) = transfer::memcpy_dtoh(&ctx, dc).unwrap();
    let raw = sink.read_buffer(out_buf, off, bytes as usize).unwrap();
    let got: Vec<f32> =
        raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();

    // The whole pipeline actually COMPUTED the elementwise sum.
    let want: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
    assert_eq!(got, want, "vecadd end-to-end result");
    assert_eq!(got, vec![11.0, 22.0, 33.0, 44.0]);

    // The dispatch really reached the executor (not a silently-skipped no-op).
    assert_eq!(sink.executor().dispatches, 1, "exactly one compute dispatch executed");

    // Freeing the output allocation lowers + runs cleanly too.
    allocate::mem_free(&mut ctx, &mut sink, dc).unwrap();
    assert!(ctx.resolve(DevicePtr(dc.0)).is_none(), "freed pointer no longer resolves");
}
