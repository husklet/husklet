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

use core::ffi::c_void;

use hl_cuda::adapter::ptx;
use hl_cuda::service::register::{self, Registry};
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION};
use hl_gpu::protocol::model::capability::{command_bits, format_bits, shader_payload, ALL_COMMANDS, COLOR_FORMATS};

fn f32s_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Wrap PTX text in a minimal nvcc-style **fatbin container** (one uncompressed PTX entry), the exact
/// shape [`hl_cuda::adapter::fatbin`] walks. Container header (16B: magic + version + header_size +
/// fat_size) → one entry header (64B: kind=PTX, entry-header-size, payload-size, flags=0) → PTX payload.
fn make_fatbin(ptx: &str) -> Vec<u8> {
    let payload = ptx.as_bytes();
    let payload_len = payload.len() as u64;
    let fat_size = 64u64 + payload_len; // one 64-byte entry header + the payload

    let mut c = Vec::new();
    // container header (16 bytes)
    c.extend_from_slice(&0xba55_ed50u32.to_le_bytes()); // magic
    c.extend_from_slice(&1u16.to_le_bytes()); // version
    c.extend_from_slice(&16u16.to_le_bytes()); // header_size
    c.extend_from_slice(&fat_size.to_le_bytes()); // fat_size

    // entry header (64 bytes)
    let mut e = [0u8; 64];
    e[0..2].copy_from_slice(&1u16.to_le_bytes()); // kind = PTX
    e[4..8].copy_from_slice(&64u32.to_le_bytes()); // entry header_size
    e[8..16].copy_from_slice(&payload_len.to_le_bytes()); // payload_size
    // flags @40 stay 0 → uncompressed
    c.extend_from_slice(&e);

    // payload
    c.extend_from_slice(payload);
    c
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

/// End-to-end through the CUDA **Runtime API** launch path — the nvcc `__cudaRegister*` +
/// `cudaLaunchKernel` seam — against the same real lowering + `CpuExecutor`:
///
///   __cudaRegisterFatBinary(fatbin(VECADD_PTX)) → handle
///   __cudaRegisterFunction(handle, hostFn, "vecadd")     // bind host-fn pointer → device entry
///   __cudaRegisterFatBinaryEnd(handle)
///   cudaMalloc ×3 → cudaMemcpyHtoD ×2 →
///   cudaLaunchKernel(hostFn, grid, block, void** args)   // resolve hostFn → entry, marshal packed args
///        └▶ SAME `launch::launch` lowering the driver-API test drives ──▶ InProcessCommandSink
///   cudaMemcpyDtoH(out) → assert c == a + b == [11, 22, 33, 44].
///
/// Nothing is hand-fed on the kernel side: the fatbin is walked to its PTX, the host-fn pointer is the
/// only kernel identity `cudaLaunchKernel` receives, and the args arrive as a runtime-API `void**`.
#[test]
fn cuda_runtime_api_vecadd_registers_and_launches_end_to_end() {
    let a = [1.0f32, 2.0, 3.0, 4.0];
    let b = [10.0f32, 20.0, 30.0, 40.0];
    let n = a.len() as u32;

    // --- host side: the reference executor + in-process sink (identical wiring to the driver-API test) -
    let mut exec = CpuExecutor::new();
    exec.set_kernel_compiler(|desc: &KernelDescriptor| ptx::compile(&desc.ptx, &desc.entry, desc.block));
    let mut sink = InProcessCommandSink::new(exec);

    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: command_bits(ALL_COMMANDS),
        texture_formats: format_bits(COLOR_FORMATS),
    };
    let caps = sink.negotiate(&req).expect("negotiate against CpuExecutor");
    assert!(caps.supports_shader_payload(shader_payload::KERNEL));

    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    // --- nvcc's __cudaRegister* image-load sequence -------------------------------------------------
    let mut registry = Registry::new();
    let fatbin = make_fatbin(ptx::VECADD_PTX);
    let handle = registry.register_fatbinary(&mut ctx, &fatbin).unwrap();

    // A fake host-fn pointer: nvcc registers the address of the generated host stub; any stable token is
    // a valid stand-in. Take the address of a static so it is a genuine, distinct pointer value.
    static VECADD_HOST_STUB: u8 = 0;
    let host_fn = &VECADD_HOST_STUB as *const u8 as usize;

    registry.register_function(&ctx, handle, host_fn, "vecadd").unwrap();
    assert!(registry.register_fatbinary_end(handle), "finalize marks a known handle");
    assert!(registry.resolve(host_fn).is_some(), "host-fn pointer resolves to a device entry");
    assert!(registry.resolve(host_fn + 1).is_none(), "an unregistered pointer resolves to nothing");

    // --- cudaMalloc + cudaMemcpyHtoD ---------------------------------------------------------------
    let bytes = (n as u64) * 4;
    let da = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    let db = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, da, &f32s_to_bytes(&a)).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, db, &f32s_to_bytes(&b)).unwrap();

    // --- cudaLaunchKernel with packed `void** args` -------------------------------------------------
    // Each slot points at its argument value: three device pointers (u64) + the scalar count (int).
    let da_v = da.0;
    let db_v = db.0;
    let dc_v = dc.0;
    let n_v = n as i32;
    let params: Vec<*const c_void> = vec![
        &da_v as *const u64 as *const c_void,
        &db_v as *const u64 as *const c_void,
        &dc_v as *const u64 as *const c_void,
        &n_v as *const i32 as *const c_void,
    ];
    unsafe {
        register::launch_kernel(&mut ctx, &mut sink, &registry, host_fn, (1, 1, 1), (n, 1, 1), params.as_ptr())
            .unwrap();
    }

    // --- cudaMemcpyDtoH readback + assert -----------------------------------------------------------
    let (out_buf, off): (BufferId, u64) = transfer::memcpy_dtoh(&ctx, dc).unwrap();
    let raw = sink.read_buffer(out_buf, off, bytes as usize).unwrap();
    let got: Vec<f32> =
        raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();

    let want: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
    assert_eq!(got, want, "runtime-API vecadd end-to-end result");
    assert_eq!(got, vec![11.0, 22.0, 33.0, 44.0]);
    assert_eq!(sink.executor().dispatches, 1, "exactly one compute dispatch executed");
}
