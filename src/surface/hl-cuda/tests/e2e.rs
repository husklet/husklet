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

use hl_gpu::protocol::model::capability::{
    shader_payload, Capabilities, ALL_COMMANDS, COLOR_FORMATS,
};
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{
    BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION,
};

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
    exec.set_kernel_compiler(|desc: &KernelDescriptor| {
        ptx::compile(&desc.ptx, &desc.entry, desc.block)
    });
    let mut sink = InProcessCommandSink::new(exec);

    // Capability handshake against the executor, exactly as a socketed driver would negotiate first.
    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: Capabilities::command_bits(ALL_COMMANDS),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
    };
    let caps = sink.negotiate(&req).expect("negotiate against CpuExecutor");
    assert!(caps.supports_shader_payload(shader_payload::KERNEL));

    // --- guest side: the real CUDA driver services --------------------------------------------------
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    // cuModuleLoadData(PTX) + cuModuleGetFunction("vecadd").
    let module = ctx.load_module(ptx::VECADD_PTX.as_bytes()).unwrap();
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
    let (out_buf, off): (BufferId, u64) = ctx.device_location(dc).unwrap();
    let raw = sink.read_buffer(out_buf, off, bytes as usize).unwrap();
    let got: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();

    // The whole pipeline actually COMPUTED the elementwise sum.
    let want: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
    assert_eq!(got, want, "vecadd end-to-end result");
    assert_eq!(got, vec![11.0, 22.0, 33.0, 44.0]);

    // The dispatch really reached the executor (not a silently-skipped no-op).
    assert_eq!(
        sink.executor().dispatches,
        1,
        "exactly one compute dispatch executed"
    );

    // Freeing the output allocation lowers + runs cleanly too.
    allocate::mem_free(&mut ctx, &mut sink, dc).unwrap();
    assert!(
        ctx.resolve(DevicePtr(dc.0)).is_none(),
        "freed pointer no longer resolves"
    );
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

    registry
        .register_function(&ctx, handle, host_fn, "vecadd")
        .unwrap();
    assert!(
        registry.register_fatbinary_end(handle),
        "finalize marks a known handle"
    );
    assert!(
        registry.resolve(host_fn).is_some(),
        "host-fn pointer resolves to a device entry"
    );
    assert!(
        registry.resolve(host_fn + 1).is_none(),
        "an unregistered pointer resolves to nothing"
    );

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
        register::launch_kernel(
            &mut ctx,
            &mut sink,
            &registry,
            host_fn,
            (1, 1, 1),
            (n, 1, 1),
            params.as_ptr(),
        )
        .unwrap();
    }

    // --- cudaMemcpyDtoH readback + assert -----------------------------------------------------------
    let (out_buf, off): (BufferId, u64) = ctx.device_location(dc).unwrap();
    let raw = sink.read_buffer(out_buf, off, bytes as usize).unwrap();
    let got: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();

    let want: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
    assert_eq!(got, want, "runtime-API vecadd end-to-end result");
    assert_eq!(got, vec![11.0, 22.0, 33.0, 44.0]);
    assert_eq!(
        sink.executor().dispatches,
        1,
        "exactly one compute dispatch executed"
    );
}

// ===================================================================================================
// More real end-to-end paths: device memset, on-device copy, a multi-block grid, and an f32-scalar
// saxpy kernel — each COMPUTES then reads the bytes back and asserts the exact result (never "did not
// crash"). All share the same real lowering → InProcessCommandSink → CpuExecutor seam.
// ===================================================================================================

/// An in-process sink over the reference `CpuExecutor` with the PTX front-end injected and the capability
/// handshake performed — the exact host wiring a socketed driver negotiates before its first submit.
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

fn readback(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &CudaContext,
    p: DevicePtr,
    len: usize,
) -> Vec<u8> {
    let (buf, off): (BufferId, u64) = ctx.device_location(p).unwrap();
    sink.read_buffer(buf, off, len).unwrap()
}

#[test]
fn cuda_memset_d32_fills_device_memory_end_to_end() {
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let n = 8usize;
    let bytes = (n * 4) as u64;
    let p = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();

    // cuMemsetD32(p, 0xAABBCCDD, n) → the word repeated n times, lowered as a WriteBuffer and executed.
    let word: u32 = 0xAABB_CCDD;
    let pattern: Vec<u8> = (0..n).flat_map(|_| word.to_le_bytes()).collect();
    transfer::memset(&mut ctx, &mut sink, p, &pattern).unwrap();

    // Read it back off the executor: every 4-byte lane is the fill word (the fill really landed).
    let raw = readback(&mut sink, &ctx, p, bytes as usize);
    for chunk in raw.chunks_exact(4) {
        assert_eq!(u32::from_le_bytes(chunk.try_into().unwrap()), word);
    }
}

#[test]
fn cuda_dtod_copy_moves_bytes_on_device_end_to_end() {
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let data: Vec<u8> = (0..64u8).collect();

    let a = allocate::mem_alloc(&mut ctx, &mut sink, 64).unwrap();
    let b = allocate::mem_alloc(&mut ctx, &mut sink, 64).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, a, &data).unwrap();
    // On-device copy a → b, then read b back: it must equal the source bytes exactly.
    transfer::memcpy_dtod(&mut ctx, &mut sink, b, a, 64).unwrap();
    assert_eq!(
        readback(&mut sink, &ctx, b, 64),
        data,
        "DtoD copy produced the source bytes"
    );
}

#[test]
fn cuda_vecadd_over_a_multi_block_grid_computes_all_elements() {
    // grid = 2 blocks, block = 2 threads → the kernel's global index ctaid*ntid+tid covers 4 elements
    // across two workgroups. Proves the grid dims propagate as the dispatch's workgroup count.
    let a = [1.0f32, 2.0, 3.0, 4.0];
    let b = [10.0f32, 20.0, 30.0, 40.0];
    let n = 4u32;

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(ptx::VECADD_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "vecadd").unwrap();

    let bytes = (n as u64) * 4;
    let da = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    let db = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, da, &f32s_to_bytes(&a)).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, db, &f32s_to_bytes(&b)).unwrap();

    let args = vec![
        KernelArg::Ptr(da),
        KernelArg::Ptr(db),
        KernelArg::Ptr(dc),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, (2, 1, 1), (2, 1, 1), &args).unwrap();

    let raw = readback(&mut sink, &ctx, dc, bytes as usize);
    let got: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    assert_eq!(
        got,
        vec![11.0, 22.0, 33.0, 44.0],
        "all 4 elements across 2 blocks"
    );
    assert_eq!(sink.executor().dispatches, 1);
}

/// saxpy: `y[i] = a*x[i] + y[i]` with the scalar count + f32 alpha placed BEFORE the two pointers — a
/// natural-aligned layout (`u32@0, f32@4, u64@8, u64@16`) and an `fma.rn.f32`.
const SAXPY_PTX: &str = r#"
    .visible .entry saxpy(
        .param .u32 saxpy_param_0,
        .param .f32 saxpy_param_1,
        .param .u64 saxpy_param_2,
        .param .u64 saxpy_param_3
    )
    {
        .reg .pred %p<2>;
        .reg .f32 %f<5>;
        .reg .b32 %r<6>;
        .reg .b64 %rd<9>;

        ld.param.u32  %r2, [saxpy_param_0];
        ld.param.f32  %f1, [saxpy_param_1];
        ld.param.u64  %rd1, [saxpy_param_2];
        ld.param.u64  %rd2, [saxpy_param_3];
        mov.u32       %r3, %ntid.x;
        mov.u32       %r4, %ctaid.x;
        mov.u32       %r5, %tid.x;
        mad.lo.s32    %r1, %r4, %r3, %r5;
        setp.ge.s32   %p1, %r1, %r2;
        @%p1 bra      DONE;
        cvta.to.global.u64 %rd3, %rd1;
        cvta.to.global.u64 %rd4, %rd2;
        mul.wide.s32  %rd5, %r1, 4;
        add.s64       %rd6, %rd3, %rd5;
        add.s64       %rd7, %rd4, %rd5;
        ld.global.f32 %f2, [%rd6];
        ld.global.f32 %f3, [%rd7];
        fma.rn.f32    %f4, %f1, %f2, %f3;
        st.global.f32 [%rd7], %f4;
    DONE:
        ret;
    }
"#;

#[test]
fn cuda_saxpy_with_f32_scalar_computes_end_to_end() {
    let x = [1.0f32, 2.0, 3.0, 4.0];
    let y = [10.0f32, 20.0, 30.0, 40.0];
    let alpha = 2.5f32;
    let n = 4u32;

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SAXPY_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "saxpy").unwrap();

    let bytes = (n as u64) * 4;
    let dx = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    let dy = allocate::mem_alloc(&mut ctx, &mut sink, bytes).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, dx, &f32s_to_bytes(&x)).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, dy, &f32s_to_bytes(&y)).unwrap();

    // args in declared order: n (int), alpha (f32), x, y.
    let args = vec![
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
        KernelArg::Scalar(alpha.to_le_bytes().to_vec()),
        KernelArg::Ptr(dx),
        KernelArg::Ptr(dy),
    ];
    launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (n, 1, 1), &args).unwrap();

    let raw = readback(&mut sink, &ctx, dy, bytes as usize);
    let got: Vec<f32> = raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let want: Vec<f32> = x.iter().zip(&y).map(|(xi, yi)| alpha * xi + yi).collect();
    assert_eq!(got, want, "saxpy y = a*x + y");
    assert_eq!(got, vec![12.5, 25.0, 37.5, 50.0]);
}
