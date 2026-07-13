//! The hand-written `cu*` entry points — the ones the generated stubs deliberately skip
//! (`build.rs` `IMPLEMENTED`). Two groups:
//!
//!   * **Bring-up** (init / driver version / device presence / context lifecycle): real, sane values
//!     so a plain `dlopen` + probe succeeds and `torch.cuda`-style detection would accept the device.
//!   * **IR-wired** (memory alloc/copy, PTX module load, kernel launch): map the CUDA compute model
//!     onto the shared dd-gpu IR through [`dd_gpu::cuda::CudaContext`], accumulating [`dd_gpu::ir::Cmd`]s
//!     in the frame. `cuLaunchKernel` recovers the kernel's parameter layout with the shared
//!     [`dd_gpu::ptx`] front-end so it can pack `kernelParams` into the exact `CudaContext::launch` ABI.
//!
//! Where the host IR / a host compute backend can't yet serve an operation (device→host readback,
//! PTX outside the modeled subset), the entry point is a spec-faithful traced no-op with a clear TODO —
//! the same "shrinking long tail" discipline as dd-shim-gl. See `docs/rendering/SHIM_RUST_ARCHITECTURE.md`.

use core::ffi::{c_char, c_void};

use dd_gpu::cuda::{DevicePtr, KernelArg};
use dd_gpu::ptx;

use crate::result::*;
use crate::state;

// ---- small helpers -------------------------------------------------------------------------------

fn inited() -> bool {
    state::with(|s| s.inited)
}

/// Read a NUL-terminated C string (best-effort, lossy) from a raw pointer.
///
/// # Safety
/// `p` must be null or point to a valid NUL-terminated C string.
unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
}

/// Map a PTX front-end failure ([`dd_gpu::ptx::compile`] → [`dd_gpu::GpuError::Ptx`]) to the accurate
/// `CUresult`. A kernel that uses an instruction / space / type *outside dd's modeled subset* is
/// `CUDA_ERROR_NOT_SUPPORTED` (the executor genuinely cannot run it); a genuinely malformed / truncated
/// PTX image is `CUDA_ERROR_INVALID_PTX` (a JIT compilation failure), matching a real driver. The
/// front-end phrases every "outside the subset" rejection with the word "unsupported", so that is the
/// discriminator (ptx.rs is the read-only executor reference — we classify from its `Display` text).
fn ptx_error_code(e: &dd_gpu::GpuError) -> i32 {
    if e.to_string().contains("unsupported") {
        CUDA_ERROR_NOT_SUPPORTED
    } else {
        CUDA_ERROR_INVALID_PTX
    }
}

// ==================================================================================================
// bring-up: init + driver version + error strings
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuInit(Flags: u32) -> i32 {
    let _ = Flags;
    state::with(|s| s.inited = true);
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDriverGetVersion(v: *mut i32) -> i32 {
    if v.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *v = DRIVER_VERSION };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuGetErrorString(err: i32, pstr: *mut *const c_char) -> i32 {
    if pstr.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let msg: &'static [u8] = match err {
        CUDA_SUCCESS => b"no error\0",
        CUDA_ERROR_INVALID_VALUE => b"invalid argument\0",
        CUDA_ERROR_OUT_OF_MEMORY => b"out of memory\0",
        CUDA_ERROR_NOT_INITIALIZED => b"initialization error\0",
        CUDA_ERROR_INVALID_DEVICE => b"invalid device ordinal\0",
        CUDA_ERROR_INVALID_IMAGE => b"device kernel image is invalid\0",
        CUDA_ERROR_INVALID_CONTEXT => b"invalid device context\0",
        CUDA_ERROR_INVALID_PTX => b"a PTX JIT compilation failed\0",
        CUDA_ERROR_INVALID_HANDLE => b"invalid resource handle\0",
        CUDA_ERROR_NOT_FOUND => b"named symbol not found\0",
        _ => b"unknown error\0",
    };
    unsafe { *pstr = msg.as_ptr() as *const c_char };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuGetErrorName(err: i32, pstr: *mut *const c_char) -> i32 {
    if pstr.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let msg: &'static [u8] = match err {
        CUDA_SUCCESS => b"CUDA_SUCCESS\0",
        CUDA_ERROR_INVALID_VALUE => b"CUDA_ERROR_INVALID_VALUE\0",
        CUDA_ERROR_OUT_OF_MEMORY => b"CUDA_ERROR_OUT_OF_MEMORY\0",
        CUDA_ERROR_NOT_INITIALIZED => b"CUDA_ERROR_NOT_INITIALIZED\0",
        CUDA_ERROR_INVALID_DEVICE => b"CUDA_ERROR_INVALID_DEVICE\0",
        CUDA_ERROR_INVALID_IMAGE => b"CUDA_ERROR_INVALID_IMAGE\0",
        CUDA_ERROR_INVALID_CONTEXT => b"CUDA_ERROR_INVALID_CONTEXT\0",
        CUDA_ERROR_INVALID_PTX => b"CUDA_ERROR_INVALID_PTX\0",
        CUDA_ERROR_INVALID_HANDLE => b"CUDA_ERROR_INVALID_HANDLE\0",
        CUDA_ERROR_NOT_FOUND => b"CUDA_ERROR_NOT_FOUND\0",
        _ => b"CUDA_ERROR_UNKNOWN\0",
    };
    unsafe { *pstr = msg.as_ptr() as *const c_char };
    CUDA_SUCCESS
}

// ==================================================================================================
// bring-up: device presence (values from dd_gpu::cuda::CudaDeviceDesc)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuDeviceGetCount(c: *mut i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if c.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *c = 1 }; // one simulated device
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGet(d: *mut i32, ordinal: i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if d.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if ordinal != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    unsafe { *d = 0 };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetName(name: *mut c_char, len: i32, dev: i32) -> i32 {
    if name.is_null() || len <= 0 || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let dev_name = state::with(|s| s.ctx.device.name.clone());
    let bytes = dev_name.as_bytes();
    let cap = (len as usize) - 1; // reserve the NUL
    let n = bytes.len().min(cap);
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), name as *mut u8, n);
        *name.add(n) = 0;
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceTotalMem_v2(bytes: *mut usize, dev: i32) -> i32 {
    if bytes.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let total = state::with(|s| s.ctx.device.total_mem);
    unsafe { *bytes = total as usize };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetAttribute(pi: *mut i32, attrib: i32, dev: i32) -> i32 {
    if pi.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let d = state::with(|s| s.ctx.device.clone());
    // Values mirror the C oracle's cuDeviceGetAttribute switch exactly (full modeled attribute set).
    let val = match attrib {
        CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK => 1024,
        CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_X => 1024,
        CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Y => 1024,
        CU_DEVICE_ATTRIBUTE_MAX_BLOCK_DIM_Z => 64,
        CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_X => 2147483647,
        CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Y => 65535,
        CU_DEVICE_ATTRIBUTE_MAX_GRID_DIM_Z => 65535,
        CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK => 49152,
        CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_BLOCK_OPTIN => 101376,
        CU_DEVICE_ATTRIBUTE_MAX_SHARED_MEMORY_PER_MULTIPROCESSOR => 102400,
        CU_DEVICE_ATTRIBUTE_TOTAL_CONSTANT_MEMORY => 65536,
        CU_DEVICE_ATTRIBUTE_WARP_SIZE => d.warp_size as i32,
        CU_DEVICE_ATTRIBUTE_MAX_PITCH => 2147483647,
        CU_DEVICE_ATTRIBUTE_MAX_REGISTERS_PER_BLOCK => 65536,
        CU_DEVICE_ATTRIBUTE_CLOCK_RATE => d.clock_khz as i32,
        CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT => 512,
        CU_DEVICE_ATTRIBUTE_GPU_OVERLAP => 1,
        CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT => d.multiprocessor_count as i32,
        CU_DEVICE_ATTRIBUTE_KERNEL_EXEC_TIMEOUT => 0,
        CU_DEVICE_ATTRIBUTE_INTEGRATED => 1, // unified memory on the host
        CU_DEVICE_ATTRIBUTE_CAN_MAP_HOST_MEMORY => 1,
        CU_DEVICE_ATTRIBUTE_COMPUTE_MODE => 0, // DEFAULT
        CU_DEVICE_ATTRIBUTE_MAXIMUM_TEXTURE1D_WIDTH => 131072,
        CU_DEVICE_ATTRIBUTE_CONCURRENT_KERNELS => 1,
        CU_DEVICE_ATTRIBUTE_ECC_ENABLED => 0,
        CU_DEVICE_ATTRIBUTE_PCI_BUS_ID => 0,
        CU_DEVICE_ATTRIBUTE_PCI_DEVICE_ID => 0,
        CU_DEVICE_ATTRIBUTE_PCI_DOMAIN_ID => 0,
        CU_DEVICE_ATTRIBUTE_TCC_DRIVER => 0,
        CU_DEVICE_ATTRIBUTE_MEMORY_CLOCK_RATE => 6251000,
        CU_DEVICE_ATTRIBUTE_GLOBAL_MEMORY_BUS_WIDTH => 256,
        CU_DEVICE_ATTRIBUTE_L2_CACHE_SIZE => 4194304,
        CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_MULTIPROCESSOR => 2048,
        CU_DEVICE_ATTRIBUTE_ASYNC_ENGINE_COUNT => 2,
        CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING => 1,
        CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR => d.compute_capability.0 as i32,
        CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR => d.compute_capability.1 as i32,
        CU_DEVICE_ATTRIBUTE_MANAGED_MEMORY => 1,
        CU_DEVICE_ATTRIBUTE_MULTI_GPU_BOARD => 0,
        CU_DEVICE_ATTRIBUTE_CONCURRENT_MANAGED_ACCESS => 1,
        CU_DEVICE_ATTRIBUTE_COMPUTE_PREEMPTION_SUPPORTED => 1,
        CU_DEVICE_ATTRIBUTE_PAGEABLE_MEMORY_ACCESS => 1,
        CU_DEVICE_ATTRIBUTE_DIRECT_MANAGED_MEM_ACCESS_FROM_HOST => 1,
        CU_DEVICE_ATTRIBUTE_MEMORY_POOLS_SUPPORTED => 0, // pools are unsupported
        _ => 0, // spec-faithful default for the unmodeled attribute tail
    };
    unsafe { *pi = val };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceComputeCapability(major: *mut i32, minor: *mut i32, dev: i32) -> i32 {
    if major.is_null() || minor.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let (maj, min) = state::with(|s| s.ctx.device.compute_capability);
    unsafe {
        *major = maj as i32;
        *minor = min as i32;
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetUuid(uuid: *mut c_void, dev: i32) -> i32 {
    if uuid.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let bytes = state::with(|s| s.ctx.device.uuid);
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), uuid as *mut u8, 16) };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetUuid_v2(uuid: *mut c_void, dev: i32) -> i32 {
    cuDeviceGetUuid(uuid, dev)
}

// ==================================================================================================
// bring-up: context lifecycle
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuCtxCreate_v2(pctx: *mut *mut c_void, flags: u32, dev: i32) -> i32 {
    let _ = flags;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if pctx.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let token = state::with(|s| {
        let t = s.next_ctx;
        s.next_ctx += 1;
        s.current_ctx = t;
        s.set_ctx_flags(t, flags);
        t
    });
    unsafe { *pctx = token as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxDestroy_v2(ctx: *mut c_void) -> i32 {
    let token = ctx as usize;
    if token == 0 {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    state::with(|s| {
        if s.current_ctx == token {
            s.current_ctx = 0;
        }
    });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSetCurrent(ctx: *mut c_void) -> i32 {
    state::with(|s| s.current_ctx = ctx as usize);
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetCurrent(pctx: *mut *mut c_void) -> i32 {
    if pctx.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let cur = state::with(|s| s.current_ctx);
    unsafe { *pctx = cur as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetDevice(dev: *mut i32) -> i32 {
    if dev.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if state::with(|s| s.current_ctx) == 0 {
        return CUDA_ERROR_INVALID_CONTEXT;
    }
    unsafe { *dev = 0 };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSynchronize() -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    state::with(|s| s.flush()); // the executor is synchronous; flushing is the sync point
    CUDA_SUCCESS
}

// ==================================================================================================
// IR-wired: memory (CUDA alloc/copy -> dd-gpu IR through CudaContext)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuMemAlloc_v2(dptr: *mut u64, bytesize: usize) -> i32 {
    if dptr.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if bytesize == 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let p = state::with(|s| {
        let (p, cmd) = s.ctx.mem_alloc(bytesize as u64); // -> Cmd::CreateBuffer
        s.frame.push(cmd);
        s.register_alloc(p.0, bytesize as u64, state::ALLOC_DEVICE);
        p.0
    });
    unsafe { *dptr = p };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemFree_v2(dptr: u64) -> i32 {
    state::with(|s| {
        if let Some(cmd) = s.ctx.mem_free(DevicePtr(dptr)) {
            // -> Cmd::DestroyBuffer
            s.frame.push(cmd);
        }
        s.unregister_alloc(dptr);
    });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemcpyHtoD_v2(dst: u64, src: *const c_void, n: usize) -> i32 {
    if src.is_null() && n > 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let bytes = unsafe { core::slice::from_raw_parts(src as *const u8, n) };
    let ok = state::with(|s| match s.ctx.memcpy_htod(DevicePtr(dst), bytes) {
        Some(cmd) => {
            // -> Cmd::WriteBuffer
            s.frame.push(cmd);
            true
        }
        None => false, // dangling device pointer
    });
    if ok {
        CUDA_SUCCESS
    } else {
        CUDA_ERROR_INVALID_VALUE
    }
}

#[no_mangle]
pub extern "C" fn cuMemcpyDtoH_v2(dst: *mut c_void, src: u64, n: usize) -> i32 {
    if dst.is_null() && n > 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if n == 0 {
        return CUDA_SUCCESS;
    }
    // Real device->host readback: flush pending work so any launched kernel has executed on the
    // embedded backend, then copy the resulting device-buffer bytes into `dst`. This is the readback
    // the scaffold deferred — the shim is now functional end-to-end (see state::read_device).
    let out = unsafe { core::slice::from_raw_parts_mut(dst as *mut u8, n) };
    let ok = state::with(|s| s.read_device(DevicePtr(src), out));
    if ok {
        CUDA_SUCCESS
    } else {
        CUDA_ERROR_INVALID_VALUE
    }
}

// ==================================================================================================
// IR-wired: module (PTX) + kernel launch (compute pipeline + dispatch)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuModuleLoadData(module: *mut *mut c_void, image: *const c_void) -> i32 {
    if module.is_null() || image.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // `image` is the PTX text (NUL-terminated) for a JIT load; parse entry points now, translate the
    // kernel body host-side later (that is the PTX->AIR/MSL research-grade step — see CUDA_ON_METAL.md).
    let ptx_src = match unsafe { cstr(image as *const c_char) } {
        Some(s) => s,
        None => return CUDA_ERROR_INVALID_IMAGE,
    };
    let id = state::with(|s| s.ctx.module_load(&ptx_src));
    crate::stub::note(format!("cuModuleLoadData(module={id})"));
    unsafe { *module = id as usize as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuModuleGetFunction(
    f: *mut *mut c_void,
    m: *mut c_void,
    name: *const c_char,
) -> i32 {
    if f.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let name = match unsafe { cstr(name) } {
        Some(s) => s,
        None => return CUDA_ERROR_INVALID_VALUE,
    };
    let mid = m as usize as u32;
    let handle = state::with(|s| {
        s.ctx
            .module_get_function(mid, &name)
            .map(|func| s.intern_function(func, &name))
    });
    match handle {
        Some(h) => {
            crate::stub::note(format!("cuModuleGetFunction(`{name}`)"));
            unsafe { *f = h };
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_NOT_FOUND,
    }
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn cuLaunchKernel(
    f: *mut c_void,
    gx: u32,
    gy: u32,
    gz: u32,
    bx: u32,
    by: u32,
    bz: u32,
    sharedMemBytes: u32,
    stream: *mut c_void,
    kernelParams: *mut *mut c_void,
    extra: *mut *mut c_void,
) -> i32 {
    let _ = (sharedMemBytes, stream, extra);

    let func = match state::with(|s| s.function(f)) {
        Some(x) => x,
        None => return CUDA_ERROR_INVALID_HANDLE,
    };
    let block = [bx.max(1), by.max(1), bz.max(1)];

    // Recover the module PTX + entry name so we can (a) forward the kernel descriptor and (b) learn the
    // parameter layout for packing `kernelParams`.
    let Some((ptx_src, entry)) = state::with(|s| {
        s.ctx.module(func.module).and_then(|md| {
            md.entries
                .get(func.entry as usize)
                .map(|e| (md.source.clone(), e.clone()))
        })
    }) else {
        return CUDA_ERROR_INVALID_HANDLE;
    };

    crate::stub::note(format!("cuLaunchKernel(entry=`{entry}`, grid=({gx},{gy},{gz}), block=({bx},{by},{bz}))"));

    // Use the shared PTX front-end purely to learn each parameter's width + pointer-ness, so we can
    // interpret the untyped `void** kernelParams` (CUDA's calling convention: each slot points at the
    // argument's value). A kernel outside the modeled PTX subset (warp intrinsics, f64, textures, inline
    // asm, …) CANNOT be executed, so — instead of the old false `CUDA_SUCCESS` no-op that left the output
    // buffer unwritten — return the accurate CUDA error. `cuLaunchKernel` has no output parameters to
    // initialize; the caller's device buffers are simply left untouched, exactly as a real launch failure.
    let prog = match ptx::compile(&ptx_src, &entry, block) {
        Ok(p) => p,
        Err(e) => {
            let cur = state::with(|s| s.current_ctx);
            let code = ptx_error_code(&e);
            crate::stub::unsupported(
                "cuLaunchKernel",
                &format!("entry `{entry}` (ctx={cur}): {e}"),
            );
            return code;
        }
    };

    let mut args: Vec<KernelArg> = Vec::with_capacity(prog.params.len());
    if !kernelParams.is_null() {
        for (i, p) in prog.params.iter().enumerate() {
            let slot = unsafe { *kernelParams.add(i) };
            if slot.is_null() {
                break;
            }
            if p.is_ptr {
                let dptr = unsafe { *(slot as *const u64) };
                args.push(KernelArg::Ptr(DevicePtr(dptr)));
            } else {
                let bytes =
                    unsafe { core::slice::from_raw_parts(slot as *const u8, p.width as usize) };
                args.push(KernelArg::Scalar(bytes.to_vec()));
            }
        }
    }

    let grid = (gx.max(1), gy.max(1), gz.max(1));
    let bl = (block[0], block[1], block[2]);
    state::with(|s| {
        // -> CreateShader(kernel descriptor) + CreateComputePipeline + param buffer + CreateBindGroup
        //    + Submit(BeginComputePass/SetPipeline/SetBindGroup/Dispatch/EndComputePass) + cleanup.
        for cmd in s.ctx.launch(func, grid, bl, &args) {
            s.frame.push(cmd);
        }
    });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamSynchronize(s: *mut c_void) -> i32 {
    let _ = s;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    state::with(|st| st.flush());
    CUDA_SUCCESS
}

// ==================================================================================================
// IR-wired: stream + event lifecycle (synchronization tokens; the executor is synchronous)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuStreamCreate(pstream: *mut *mut c_void, flags: u32) -> i32 {
    let _ = flags;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if pstream.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // The executor completes each submit synchronously and ordering is preserved by the single
    // accumulated frame, so a stream is a non-null scheduling token (no per-stream queue needed).
    let token = state::with(|s| {
        let t = s.next_stream;
        s.next_stream += 1;
        s.register_stream(t, flags, 0);
        t
    });
    unsafe { *pstream = token as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamDestroy_v2(stream: *mut c_void) -> i32 {
    // Flush any work outstanding on the stream (a destroy implies completion), then drop the token.
    state::with(|s| {
        s.flush();
        s.unregister_stream(stream as usize);
    });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuEventCreate(pevent: *mut *mut c_void, flags: u32) -> i32 {
    let _ = flags;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if pevent.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let token = state::with(|s| {
        let t = s.next_event;
        s.next_event += 1;
        s.register_event(t);
        t
    });
    unsafe { *pevent = token as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuEventRecord(event: *mut c_void, stream: *mut c_void) -> i32 {
    let _ = stream;
    if event.is_null() {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    // Recording an event marks a point in the stream; with a synchronous executor the correct place to
    // land the preceding work is here, so flush — then timestamp so cuEventElapsedTime is truthful.
    state::with(|s| {
        s.flush();
        s.record_event(event as usize);
    });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuEventSynchronize(event: *mut c_void) -> i32 {
    if event.is_null() {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    state::with(|s| s.flush());
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuEventDestroy_v2(event: *mut c_void) -> i32 {
    state::with(|s| s.unregister_event(event as usize));
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuEventRecordWithFlags(event: *mut c_void, stream: *mut c_void, flags: u32) -> i32 {
    let _ = flags;
    cuEventRecord(event, stream)
}

#[no_mangle]
pub extern "C" fn cuEventQuery(event: *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if event.is_null() {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    // Synchronous executor: a recorded event is already complete; an unrecorded one is not ready.
    if state::with(|s| s.event_recorded(event as usize)) {
        CUDA_SUCCESS
    } else {
        CUDA_ERROR_NOT_READY
    }
}

#[no_mangle]
pub extern "C" fn cuEventElapsedTime(ms: *mut f32, start: *mut c_void, end: *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if ms.is_null() || start.is_null() || end.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    match state::with(|s| s.event_elapsed_ms(start as usize, end as usize)) {
        Some(v) => {
            unsafe { *ms = v };
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_NOT_READY, // one or both events unrecorded
    }
}

// ==================================================================================================
// context management: push/pop stack, api version, flags, limits, cache config, peer access
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuCtxCreate_v3(
    pctx: *mut *mut c_void,
    paramsArray: *mut c_void,
    numParams: i32,
    flags: u32,
    dev: i32,
) -> i32 {
    let _ = (paramsArray, numParams);
    cuCtxCreate_v2(pctx, flags, dev)
}

#[no_mangle]
pub extern "C" fn cuCtxPushCurrent_v2(ctx: *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if ctx.is_null() {
        return CUDA_ERROR_INVALID_HANDLE;
    }
    state::with(|s| {
        let prev = s.current_ctx;
        s.ctx_stack.push(prev);
        s.current_ctx = ctx as usize;
    });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxPopCurrent_v2(pctx: *mut *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    let cur = state::with(|s| {
        let cur = s.current_ctx;
        s.current_ctx = s.ctx_stack.pop().unwrap_or(0);
        cur
    });
    if !pctx.is_null() {
        unsafe { *pctx = cur as *mut c_void };
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetApiVersion(ctx: *mut c_void, version: *mut u32) -> i32 {
    let _ = ctx;
    if version.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *version = CTX_API_VERSION };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetFlags(flags: *mut u32) -> i32 {
    if flags.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let f = state::with(|s| s.current_ctx_flags());
    unsafe { *flags = f };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSetFlags(flags: u32) -> i32 {
    state::with(|s| s.set_current_ctx_flags(flags));
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetId(ctx: *mut c_void, id: *mut u64) -> i32 {
    if id.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let token = if ctx.is_null() {
        state::with(|s| s.current_ctx)
    } else {
        ctx as usize
    };
    unsafe { *id = token as u64 };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetLimit(v: *mut usize, limit: i32) -> i32 {
    if v.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if limit < 0 || limit >= CU_LIMIT_MAX {
        return CUDA_ERROR_UNSUPPORTED_LIMIT;
    }
    let val = state::with(|s| s.limits[limit as usize]);
    unsafe { *v = val };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSetLimit(limit: i32, value: usize) -> i32 {
    if limit < 0 || limit >= CU_LIMIT_MAX {
        return CUDA_ERROR_UNSUPPORTED_LIMIT;
    }
    state::with(|s| s.limits[limit as usize] = value);
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetCacheConfig(c: *mut i32) -> i32 {
    if c.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let v = state::with(|s| s.cache_config);
    unsafe { *c = v };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSetCacheConfig(c: i32) -> i32 {
    state::with(|s| s.cache_config = c);
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetSharedMemConfig(c: *mut i32) -> i32 {
    if c.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let v = state::with(|s| s.shared_config);
    unsafe { *c = v };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxSetSharedMemConfig(c: i32) -> i32 {
    state::with(|s| s.shared_config = c);
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxGetStreamPriorityRange(least: *mut i32, greatest: *mut i32) -> i32 {
    // The executor is synchronous: a single priority band (matches the C oracle).
    if !least.is_null() {
        unsafe { *least = 0 };
    }
    if !greatest.is_null() {
        unsafe { *greatest = 0 };
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxResetPersistingL2Cache() -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuCtxEnablePeerAccess(peer: *mut c_void, flags: u32) -> i32 {
    let _ = (peer, flags);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_ERROR_PEER_ACCESS_UNSUPPORTED // single simulated device: never any peers
}

#[no_mangle]
pub extern "C" fn cuCtxDisablePeerAccess(peer: *mut c_void) -> i32 {
    let _ = peer;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_ERROR_PEER_ACCESS_NOT_ENABLED
}

// ==================================================================================================
// device: peer, PCI bus id, LUID, legacy properties
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuDeviceCanAccessPeer(canAccessPeer: *mut i32, a: i32, b: i32) -> i32 {
    let _ = (a, b);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if canAccessPeer.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *canAccessPeer = 0 }; // single simulated device: no peers
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetPCIBusId(s: *mut c_char, len: i32, dev: i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if s.is_null() || len <= 0 || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    write_cstr(s, len as usize, "0000:00:00.0");
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetByPCIBusId(dev: *mut i32, s: *const c_char) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if dev.is_null() || s.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *dev = 0 }; // single simulated device
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDeviceGetLuid(luid: *mut c_char, mask: *mut u32, dev: i32) -> i32 {
    let _ = (luid, mask);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    CUDA_ERROR_NOT_SUPPORTED // LUID is Windows/TCC-only; not modeled
}

/// The `CUdevprop` struct filled by `cuDeviceGetProperties` (layout matches `dd-gpu/cuda/cuda_min.h`).
#[repr(C)]
struct CuDevprop {
    max_threads_per_block: i32,
    max_threads_dim: [i32; 3],
    max_grid_size: [i32; 3],
    shared_mem_per_block: i32,
    total_constant_memory: i32,
    simd_width: i32,
    mem_pitch: i32,
    regs_per_block: i32,
    clock_rate: i32,
    texture_align: i32,
}

#[no_mangle]
pub extern "C" fn cuDeviceGetProperties(prop: *mut c_void, dev: i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if prop.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // Values mirror the C oracle's cuDeviceGetProperties exactly.
    let p = CuDevprop {
        max_threads_per_block: 1024,
        max_threads_dim: [1024, 1024, 64],
        max_grid_size: [2147483647, 65535, 65535],
        shared_mem_per_block: 49152,
        total_constant_memory: 65536,
        simd_width: 32,
        mem_pitch: 2147483647,
        regs_per_block: 65536,
        clock_rate: 1500000,
        texture_align: 512,
    };
    unsafe { *(prop as *mut CuDevprop) = p };
    CUDA_SUCCESS
}

// ==================================================================================================
// primary context (device 0) — retain/release/reset ref-counting
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxRetain(pctx: *mut *mut c_void, dev: i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if pctx.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let token = state::with(|s| {
        if s.primary_ctx == 0 {
            let t = s.next_ctx;
            s.next_ctx += 1;
            s.primary_ctx = t;
        }
        s.primary_refcount += 1;
        s.primary_ctx
    });
    unsafe { *pctx = token as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxRelease_v2(dev: i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    state::with(|s| {
        if s.primary_refcount > 0 {
            s.primary_refcount -= 1;
        }
        if s.primary_refcount == 0 && s.primary_ctx != 0 {
            if s.current_ctx == s.primary_ctx {
                s.current_ctx = 0;
            }
            s.primary_ctx = 0;
        }
    });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxReset_v2(dev: i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    state::with(|s| {
        s.primary_refcount = 0;
        if s.primary_ctx != 0 {
            if s.current_ctx == s.primary_ctx {
                s.current_ctx = 0;
            }
            s.primary_ctx = 0;
        }
    });
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxGetState(dev: i32, flags: *mut u32, active: *mut i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    let (f, a) = state::with(|s| {
        (
            if s.primary_ctx != 0 { s.primary_flags } else { 0 },
            (s.primary_refcount > 0) as i32,
        )
    });
    if !flags.is_null() {
        unsafe { *flags = f };
    }
    if !active.is_null() {
        unsafe { *active = a };
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuDevicePrimaryCtxSetFlags_v2(dev: i32, flags: u32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    state::with(|s| {
        if s.primary_ctx != 0 {
            s.primary_flags = flags;
        }
    });
    CUDA_SUCCESS
}

// ==================================================================================================
// memory: managed / pitch / host alloc + register, info, address range
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuMemAllocManaged(dptr: *mut u64, bytesize: usize, flags: u32) -> i32 {
    let _ = flags;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if dptr.is_null() || bytesize == 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let p = state::with(|s| {
        let (p, cmd) = s.ctx.mem_alloc(bytesize as u64);
        s.frame.push(cmd);
        s.register_alloc(p.0, bytesize as u64, state::ALLOC_MANAGED);
        p.0
    });
    unsafe { *dptr = p };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemAllocPitch_v2(
    dptr: *mut u64,
    pPitch: *mut usize,
    widthBytes: usize,
    height: usize,
    elemSz: u32,
) -> i32 {
    let _ = elemSz;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if dptr.is_null() || pPitch.is_null() || widthBytes == 0 || height == 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let pitch = (widthBytes + 511) & !511usize; // 512-byte aligned rows, like a real allocator
    let p = state::with(|s| {
        let (p, cmd) = s.ctx.mem_alloc((pitch * height) as u64);
        s.frame.push(cmd);
        s.register_alloc(p.0, (pitch * height) as u64, state::ALLOC_DEVICE);
        p.0
    });
    unsafe {
        *dptr = p;
        *pPitch = pitch;
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemAllocHost_v2(pp: *mut *mut c_void, size: usize) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if pp.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let p = state::with(|s| s.host_alloc(size, state::ALLOC_HOST));
    unsafe { *pp = p };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemHostAlloc(pp: *mut *mut c_void, size: usize, flags: u32) -> i32 {
    let _ = flags;
    cuMemAllocHost_v2(pp, size)
}

#[no_mangle]
pub extern "C" fn cuMemFreeHost(p: *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if !p.is_null() {
        state::with(|s| s.host_free(p));
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemHostGetDevicePointer_v2(
    pdptr: *mut u64,
    p: *mut c_void,
    flags: u32,
) -> i32 {
    let _ = flags;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if pdptr.is_null() || p.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *pdptr = p as usize as u64 }; // unified: host and device addresses coincide
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemHostGetFlags(pflags: *mut u32, p: *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if pflags.is_null() || p.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *pflags = 0 };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemHostRegister_v2(p: *mut c_void, size: usize, flags: u32) -> i32 {
    let _ = flags;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if p.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    state::with(|s| {
        if s.alloc_is_base(p as u64) {
            CUDA_ERROR_HOST_MEMORY_ALREADY_REGISTERED
        } else {
            s.register_alloc(p as u64, size as u64, state::ALLOC_REGISTERED);
            CUDA_SUCCESS
        }
    })
}

#[no_mangle]
pub extern "C" fn cuMemHostUnregister(p: *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if p.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    state::with(|s| {
        if s.alloc_is_base(p as u64) {
            s.unregister_alloc(p as u64);
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_HOST_MEMORY_NOT_REGISTERED
        }
    })
}

#[no_mangle]
pub extern "C" fn cuMemGetInfo_v2(freeB: *mut usize, totalB: *mut usize) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    let (free, total) = state::with(|s| {
        let total = s.ctx.device.total_mem;
        let used = s.bytes_outstanding.min(total);
        ((total - used) as usize, total as usize)
    });
    if !freeB.is_null() {
        unsafe { *freeB = free };
    }
    if !totalB.is_null() {
        unsafe { *totalB = total };
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuMemGetAddressRange_v2(pbase: *mut u64, psize: *mut usize, dptr: u64) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    match state::with(|s| s.find_alloc(dptr)) {
        Some(a) => {
            if !pbase.is_null() {
                unsafe { *pbase = a.base };
            }
            if !psize.is_null() {
                unsafe { *psize = a.size as usize };
            }
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_VALUE,
    }
}

#[no_mangle]
pub extern "C" fn cuMemAllocAsync(dptr: *mut u64, bytesize: usize, s: *mut c_void) -> i32 {
    let _ = s;
    cuMemAlloc_v2(dptr, bytesize)
}

#[no_mangle]
pub extern "C" fn cuMemFreeAsync(dptr: u64, s: *mut c_void) -> i32 {
    let _ = s;
    cuMemFree_v2(dptr)
}

#[no_mangle]
pub extern "C" fn cuMemPrefetchAsync(p: u64, n: usize, dst: i32, s: *mut c_void) -> i32 {
    let _ = (p, n, dst, s);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS // unified memory: prefetch is a valid no-op
}

#[no_mangle]
pub extern "C" fn cuMemAdvise(p: u64, n: usize, advice: i32, dev: i32) -> i32 {
    let _ = (p, n, advice, dev);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS // unified memory: advise is a valid no-op
}

// ==================================================================================================
// memory: copies (DtoD / generic / peer / async) + memset
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuMemcpyDtoD_v2(dst: u64, src: u64, n: usize) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if n == 0 {
        return CUDA_SUCCESS;
    }
    if dst == 0 || src == 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    if state::with(|s| s.copy_dtod(DevicePtr(dst), DevicePtr(src), n)) {
        CUDA_SUCCESS
    } else {
        CUDA_ERROR_INVALID_VALUE
    }
}

#[no_mangle]
pub extern "C" fn cuMemcpy(dst: u64, src: u64, n: usize) -> i32 {
    // Unified-memory model: a generic copy is device→device.
    cuMemcpyDtoD_v2(dst, src, n)
}

#[no_mangle]
pub extern "C" fn cuMemcpyDtoDAsync_v2(dst: u64, src: u64, n: usize, s: *mut c_void) -> i32 {
    let _ = s;
    cuMemcpyDtoD_v2(dst, src, n)
}

#[no_mangle]
pub extern "C" fn cuMemcpyAsync(dst: u64, src: u64, n: usize, s: *mut c_void) -> i32 {
    let _ = s;
    cuMemcpy(dst, src, n)
}

#[no_mangle]
pub extern "C" fn cuMemcpyHtoDAsync_v2(dst: u64, src: *const c_void, n: usize, s: *mut c_void) -> i32 {
    let _ = s;
    cuMemcpyHtoD_v2(dst, src, n)
}

#[no_mangle]
pub extern "C" fn cuMemcpyDtoHAsync_v2(dst: *mut c_void, src: u64, n: usize, s: *mut c_void) -> i32 {
    let _ = s;
    cuMemcpyDtoH_v2(dst, src, n)
}

#[no_mangle]
pub extern "C" fn cuMemcpyPeer(
    dst: u64,
    dctx: *mut c_void,
    src: u64,
    sctx: *mut c_void,
    n: usize,
) -> i32 {
    let _ = (dctx, sctx);
    cuMemcpyDtoD_v2(dst, src, n)
}

#[no_mangle]
pub extern "C" fn cuMemcpyPeerAsync(
    dst: u64,
    dctx: *mut c_void,
    src: u64,
    sctx: *mut c_void,
    n: usize,
    s: *mut c_void,
) -> i32 {
    let _ = (dctx, sctx, s);
    cuMemcpyDtoD_v2(dst, src, n)
}

#[no_mangle]
pub extern "C" fn cuMemsetD8_v2(dst: u64, uc: u8, N: usize) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if N == 0 {
        return CUDA_SUCCESS;
    }
    if dst == 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let pattern = vec![uc; N];
    memset_common(dst, &pattern)
}

#[no_mangle]
pub extern "C" fn cuMemsetD16_v2(dst: u64, us: u16, N: usize) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if N == 0 {
        return CUDA_SUCCESS;
    }
    if dst == 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let mut pattern = Vec::with_capacity(N * 2);
    let b = us.to_le_bytes();
    for _ in 0..N {
        pattern.extend_from_slice(&b);
    }
    memset_common(dst, &pattern)
}

#[no_mangle]
pub extern "C" fn cuMemsetD32_v2(dst: u64, ui: u32, N: usize) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if N == 0 {
        return CUDA_SUCCESS;
    }
    if dst == 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let mut pattern = Vec::with_capacity(N * 4);
    let b = ui.to_le_bytes();
    for _ in 0..N {
        pattern.extend_from_slice(&b);
    }
    memset_common(dst, &pattern)
}

/// Shared body for the memset family: push the expanded fill into the device buffer.
fn memset_common(dst: u64, pattern: &[u8]) -> i32 {
    if state::with(|s| s.memset(DevicePtr(dst), pattern)) {
        CUDA_SUCCESS
    } else {
        CUDA_ERROR_INVALID_VALUE
    }
}

#[no_mangle]
pub extern "C" fn cuMemsetD8Async(dst: u64, uc: u8, N: usize, s: *mut c_void) -> i32 {
    let _ = s;
    cuMemsetD8_v2(dst, uc, N)
}

#[no_mangle]
pub extern "C" fn cuMemsetD16Async(dst: u64, us: u16, N: usize, s: *mut c_void) -> i32 {
    let _ = s;
    cuMemsetD16_v2(dst, us, N)
}

#[no_mangle]
pub extern "C" fn cuMemsetD32Async(dst: u64, ui: u32, N: usize, s: *mut c_void) -> i32 {
    let _ = s;
    cuMemsetD32_v2(dst, ui, N)
}

// ==================================================================================================
// module: load variants, unload, global/texref/surfref, loading mode
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuModuleLoad(module: *mut *mut c_void, fname: *const c_char) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if module.is_null() || fname.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let path = match unsafe { cstr(fname) } {
        Some(s) => s,
        None => return CUDA_ERROR_INVALID_VALUE,
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return CUDA_ERROR_FILE_NOT_FOUND,
    };
    // Our module model is PTX text; NUL-terminate and load through the shared translator.
    let src = String::from_utf8_lossy(&bytes).into_owned();
    let id = state::with(|s| s.ctx.module_load(&src));
    unsafe { *module = id as usize as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuModuleLoadDataEx(
    module: *mut *mut c_void,
    image: *const c_void,
    n: u32,
    o: *mut i32,
    ov: *mut *mut c_void,
) -> i32 {
    let _ = (n, o, ov);
    cuModuleLoadData(module, image)
}

#[no_mangle]
pub extern "C" fn cuModuleLoadFatBinary(module: *mut *mut c_void, image: *const c_void) -> i32 {
    // dd's Rust module model parses PTX text; a real fatbin container is not unpacked here (the C
    // oracle's fatbin.h extraction has no Rust port yet), so treat the image as PTX text.
    cuModuleLoadData(module, image)
}

#[no_mangle]
pub extern "C" fn cuModuleUnload(m: *mut c_void) -> i32 {
    let _ = m; // CudaContext keeps modules for the process lifetime; unload is a valid no-op
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuModuleGetGlobal_v2(
    dptr: *mut u64,
    bytes: *mut usize,
    m: *mut c_void,
    name: *const c_char,
) -> i32 {
    let _ = (dptr, bytes);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if m.is_null() || name.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // dd parses only kernel entries out of PTX, not `.global` variables → symbol absent.
    CUDA_ERROR_NOT_FOUND
}

#[no_mangle]
pub extern "C" fn cuModuleGetLoadingMode(mode: *mut i32) -> i32 {
    if mode.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *mode = CU_MODULE_EAGER_LOADING };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuModuleGetTexRef(t: *mut *mut c_void, m: *mut c_void, name: *const c_char) -> i32 {
    let _ = (t, m, name);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_ERROR_NOT_FOUND // no texture references in dd's PTX model
}

#[no_mangle]
pub extern "C" fn cuModuleGetSurfRef(s: *mut *mut c_void, m: *mut c_void, name: *const c_char) -> i32 {
    let _ = (s, m, name);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_ERROR_NOT_FOUND // no surface references in dd's PTX model
}

// ==================================================================================================
// function attributes / module / name
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuFuncGetAttribute(pi: *mut i32, attrib: i32, f: *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if pi.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let Some(dyn_shared) = state::with(|s| s.func_dyn_shared(f)) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    let cc = state::with(|s| s.ctx.device.compute_capability);
    let ptx_ver = cc.0 as i32 * 10 + cc.1 as i32;
    let val = match attrib {
        CU_FUNC_ATTRIBUTE_MAX_THREADS_PER_BLOCK => 1024,
        CU_FUNC_ATTRIBUTE_SHARED_SIZE_BYTES => 0,
        CU_FUNC_ATTRIBUTE_CONST_SIZE_BYTES => 0,
        CU_FUNC_ATTRIBUTE_LOCAL_SIZE_BYTES => 0,
        CU_FUNC_ATTRIBUTE_NUM_REGS => 32,
        CU_FUNC_ATTRIBUTE_PTX_VERSION => ptx_ver,
        CU_FUNC_ATTRIBUTE_BINARY_VERSION => ptx_ver,
        CU_FUNC_ATTRIBUTE_CACHE_MODE_CA => 0,
        CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES => dyn_shared,
        CU_FUNC_ATTRIBUTE_PREFERRED_SHARED_MEMORY_CARVEOUT => -1,
        _ => 0,
    };
    unsafe { *pi = val };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuFuncSetAttribute(f: *mut c_void, attrib: i32, value: i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    let ok = state::with(|s| {
        if attrib == CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES {
            s.set_func_dyn_shared(f, value)
        } else {
            s.func_dyn_shared(f).is_some() // validate the handle
        }
    });
    if ok {
        CUDA_SUCCESS
    } else {
        CUDA_ERROR_INVALID_VALUE
    }
}

#[no_mangle]
pub extern "C" fn cuFuncSetCacheConfig(f: *mut c_void, config: i32) -> i32 {
    let _ = (f, config);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuFuncSetSharedMemConfig(f: *mut c_void, config: i32) -> i32 {
    let _ = (f, config);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuFuncGetModule(m: *mut *mut c_void, f: *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if m.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    match state::with(|s| s.function(f)) {
        Some(func) => {
            unsafe { *m = func.module as usize as *mut c_void };
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_VALUE,
    }
}

#[no_mangle]
pub extern "C" fn cuFuncGetName(name: *mut *const c_char, f: *mut c_void) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if name.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    match state::with(|s| s.func_name_ptr(f)) {
        Some(p) => {
            unsafe { *name = p };
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_VALUE,
    }
}

// ==================================================================================================
// occupancy (computed from the modeled SM limits — sane, not fabricated)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuOccupancyMaxActiveBlocksPerMultiprocessor(
    numBlocks: *mut i32,
    f: *mut c_void,
    blockSize: i32,
    dynSmem: usize,
) -> i32 {
    let _ = (f, dynSmem);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if numBlocks.is_null() || blockSize <= 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let by_threads = 2048 / blockSize; // MAX_THREADS_PER_MULTIPROCESSOR / blockSize
    let n = by_threads.min(32).max(1); // cap at 32 resident blocks/SM
    unsafe { *numBlocks = n };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuOccupancyMaxActiveBlocksPerMultiprocessorWithFlags(
    numBlocks: *mut i32,
    f: *mut c_void,
    blockSize: i32,
    dynSmem: usize,
    flags: u32,
) -> i32 {
    let _ = flags;
    cuOccupancyMaxActiveBlocksPerMultiprocessor(numBlocks, f, blockSize, dynSmem)
}

#[no_mangle]
pub extern "C" fn cuOccupancyMaxPotentialBlockSize(
    minGridSize: *mut i32,
    blockSize: *mut i32,
    f: *mut c_void,
    b2d: *mut c_void,
    dynSmem: usize,
    blockSizeLimit: i32,
) -> i32 {
    let _ = (f, b2d, dynSmem);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    let bs = if blockSizeLimit > 0 && blockSizeLimit < 256 { blockSizeLimit } else { 256 };
    if !blockSize.is_null() {
        unsafe { *blockSize = bs };
    }
    if !minGridSize.is_null() {
        unsafe { *minGridSize = 32 }; // MULTIPROCESSOR_COUNT
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuOccupancyMaxPotentialBlockSizeWithFlags(
    minGridSize: *mut i32,
    blockSize: *mut i32,
    f: *mut c_void,
    b2d: *mut c_void,
    dynSmem: usize,
    blockSizeLimit: i32,
    flags: u32,
) -> i32 {
    let _ = flags;
    cuOccupancyMaxPotentialBlockSize(minGridSize, blockSize, f, b2d, dynSmem, blockSizeLimit)
}

#[no_mangle]
pub extern "C" fn cuOccupancyAvailableDynamicSMemPerBlock(
    dynSmem: *mut usize,
    f: *mut c_void,
    numBlocks: i32,
    blockSize: i32,
) -> i32 {
    let _ = (f, numBlocks, blockSize);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if dynSmem.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *dynSmem = 49152 };
    CUDA_SUCCESS
}

// ==================================================================================================
// launch: cooperative, host func, launch-ex
// ==================================================================================================

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn cuLaunchCooperativeKernel(
    f: *mut c_void,
    gx: u32,
    gy: u32,
    gz: u32,
    bx: u32,
    by: u32,
    bz: u32,
    sharedMemBytes: u32,
    stream: *mut c_void,
    kernelParams: *mut *mut c_void,
) -> i32 {
    // A cooperative launch is an ordinary grid launch in dd's synchronous single-device model.
    cuLaunchKernel(
        f, gx, gy, gz, bx, by, bz, sharedMemBytes, stream, kernelParams, core::ptr::null_mut(),
    )
}

#[no_mangle]
pub extern "C" fn cuLaunchKernelEx(
    cfg: *const c_void,
    f: *mut c_void,
    kernelParams: *mut *mut c_void,
    extra: *mut *mut c_void,
) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if cfg.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // CUlaunchConfig layout (cuda_min.h): gridDim{X,Y,Z}, blockDim{X,Y,Z}, sharedMemBytes are the
    // first seven u32s. Read them to drive the ordinary launch path.
    let d = cfg as *const u32;
    let (gx, gy, gz, bx, by, bz, smem) = unsafe {
        (
            *d.add(0),
            *d.add(1),
            *d.add(2),
            *d.add(3),
            *d.add(4),
            *d.add(5),
            *d.add(6),
        )
    };
    cuLaunchKernel(f, gx, gy, gz, bx, by, bz, smem, core::ptr::null_mut(), kernelParams, extra)
}

#[no_mangle]
pub extern "C" fn cuLaunchHostFunc(stream: *mut c_void, fn_: *mut c_void, userData: *mut c_void) -> i32 {
    let _ = stream;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if fn_.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // Flush pending work, then run the host callback inline (synchronous executor). Call it OUTSIDE
    // the state lock — a callback may re-enter the driver API.
    state::with(|s| s.flush());
    // SAFETY: `fn_` is a `CUhostFn = void(*)(void*)` passed by the caller.
    let hostfn: extern "C" fn(*mut c_void) = unsafe { core::mem::transmute(fn_) };
    hostfn(userData);
    CUDA_SUCCESS
}

// ==================================================================================================
// pointer attributes (report what dd's model actually knows)
// ==================================================================================================

/// Fill one pointer attribute into `data` (mirrors the C oracle's `pointer_attr`).
///
/// # Safety
/// `data` must point at a buffer large enough for `attr`'s value type.
unsafe fn pointer_attr(attr: i32, data: *mut c_void, ptr: u64) -> i32 {
    if data.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let (found, meta) = state::with(|s| {
        let m = s.find_alloc(ptr);
        (m.is_some(), m)
    });
    let cur_ctx = state::with(|s| s.current_ctx);
    match attr {
        CU_POINTER_ATTRIBUTE_CONTEXT => *(data as *mut usize) = cur_ctx,
        CU_POINTER_ATTRIBUTE_MEMORY_TYPE => {
            let is_host = meta.map(|m| m.kind == state::ALLOC_HOST).unwrap_or(false);
            *(data as *mut u32) = if is_host { CU_MEMORYTYPE_HOST } else { CU_MEMORYTYPE_DEVICE };
        }
        CU_POINTER_ATTRIBUTE_DEVICE_POINTER => *(data as *mut u64) = ptr,
        CU_POINTER_ATTRIBUTE_HOST_POINTER => *(data as *mut *mut c_void) = ptr as usize as *mut c_void,
        CU_POINTER_ATTRIBUTE_IS_MANAGED => {
            *(data as *mut u32) = meta.map(|m| (m.kind == state::ALLOC_MANAGED) as u32).unwrap_or(0)
        }
        CU_POINTER_ATTRIBUTE_DEVICE_ORDINAL => *(data as *mut i32) = 0,
        CU_POINTER_ATTRIBUTE_BUFFER_ID => *(data as *mut u64) = meta.map(|m| m.base).unwrap_or(0),
        CU_POINTER_ATTRIBUTE_SYNC_MEMOPS => *(data as *mut i32) = 1,
        CU_POINTER_ATTRIBUTE_MAPPED => *(data as *mut i32) = found as i32,
        CU_POINTER_ATTRIBUTE_RANGE_START_ADDR => *(data as *mut u64) = meta.map(|m| m.base).unwrap_or(0),
        CU_POINTER_ATTRIBUTE_RANGE_SIZE => *(data as *mut usize) = meta.map(|m| m.size as usize).unwrap_or(0),
        _ => return CUDA_ERROR_NOT_SUPPORTED,
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuPointerGetAttribute(data: *mut c_void, attr: i32, ptr: u64) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if data.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { pointer_attr(attr, data, ptr) }
}

#[no_mangle]
pub extern "C" fn cuPointerGetAttributes(
    n: u32,
    attrs: *mut i32,
    data: *mut *mut c_void,
    ptr: u64,
) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if attrs.is_null() || data.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    for i in 0..n as usize {
        let attr = unsafe { *attrs.add(i) };
        let slot = unsafe { *data.add(i) };
        let r = unsafe { pointer_attr(attr, slot, ptr) };
        if r != CUDA_SUCCESS && r != CUDA_ERROR_NOT_SUPPORTED {
            return r;
        }
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuPointerSetAttribute(value: *const c_void, attr: i32, ptr: u64) -> i32 {
    let _ = (value, attr, ptr);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS
}

// ==================================================================================================
// streams: create-with-priority, query, wait-event, callback, getters, attach, capture
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuStreamCreateWithPriority(
    s: *mut *mut c_void,
    flags: u32,
    priority: i32,
) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if s.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let token = state::with(|st| {
        let t = st.next_stream;
        st.next_stream += 1;
        st.register_stream(t, flags, priority);
        t
    });
    unsafe { *s = token as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamQuery(s: *mut c_void) -> i32 {
    let _ = s;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS // synchronous executor: always ready
}

#[no_mangle]
pub extern "C" fn cuStreamWaitEvent(s: *mut c_void, e: *mut c_void, flags: u32) -> i32 {
    let _ = (s, e, flags);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS // synchronous executor: the awaited work has already completed
}

#[no_mangle]
pub extern "C" fn cuStreamAddCallback(
    s: *mut c_void,
    cb: *mut c_void,
    userData: *mut c_void,
    flags: u32,
) -> i32 {
    let _ = flags;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if cb.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    // Flush pending work, then fire the callback with success — OUTSIDE the state lock (it may
    // re-enter the driver API).
    state::with(|st| st.flush());
    // SAFETY: `cb` is a `CUstreamCallback = void(*)(CUstream, CUresult, void*)`.
    let callback: extern "C" fn(*mut c_void, i32, *mut c_void) =
        unsafe { core::mem::transmute(cb) };
    callback(s, CUDA_SUCCESS, userData);
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamGetFlags(s: *mut c_void, flags: *mut u32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if flags.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let v = state::with(|st| st.stream_flags(s as usize));
    unsafe { *flags = v };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamGetPriority(s: *mut c_void, priority: *mut i32) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if priority.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let v = state::with(|st| st.stream_priority(s as usize));
    unsafe { *priority = v };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamGetCtx(s: *mut c_void, pctx: *mut *mut c_void) -> i32 {
    let _ = s;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if pctx.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let cur = state::with(|st| st.current_ctx);
    unsafe { *pctx = cur as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamGetId(s: *mut c_void, id: *mut u64) -> i32 {
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if id.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *id = s as u64 };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamAttachMemAsync(
    s: *mut c_void,
    dptr: u64,
    length: usize,
    flags: u32,
) -> i32 {
    let _ = (s, dptr, length, flags);
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamIsCapturing(s: *mut c_void, status: *mut i32) -> i32 {
    let _ = s;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    if !status.is_null() {
        unsafe { *status = 0 }; // CU_STREAM_CAPTURE_STATUS_NONE
    }
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuThreadExchangeStreamCaptureMode(mode: *mut i32) -> i32 {
    let _ = mode;
    if !inited() {
        return CUDA_ERROR_NOT_INITIALIZED;
    }
    CUDA_SUCCESS // capture is unsupported; leave the mode unchanged
}

// ==================================================================================================
// profiler control (valid no-ops)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cuProfilerInitialize(cfg: *const c_char, out: *const c_char, fmt: i32) -> i32 {
    let _ = (cfg, out, fmt);
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuProfilerStart() -> i32 {
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuProfilerStop() -> i32 {
    CUDA_SUCCESS
}

// ==================================================================================================
// entry-point dispatch: cuGetProcAddress (+_v2) — resolve a cu* symbol to its function pointer.
// ==================================================================================================

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Map a base (unversioned) `cu*` name to the newest versioned symbol the app should bind, matching
/// the C oracle's alias table. Names not listed resolve as-is.
fn newest_symbol(name: &str) -> &str {
    match name {
        "cuDeviceTotalMem" => "cuDeviceTotalMem_v2",
        "cuCtxCreate" => "cuCtxCreate_v2",
        "cuCtxDestroy" => "cuCtxDestroy_v2",
        "cuCtxPushCurrent" => "cuCtxPushCurrent_v2",
        "cuCtxPopCurrent" => "cuCtxPopCurrent_v2",
        "cuDevicePrimaryCtxRelease" => "cuDevicePrimaryCtxRelease_v2",
        "cuDevicePrimaryCtxReset" => "cuDevicePrimaryCtxReset_v2",
        "cuDevicePrimaryCtxSetFlags" => "cuDevicePrimaryCtxSetFlags_v2",
        "cuModuleGetGlobal" => "cuModuleGetGlobal_v2",
        "cuMemGetInfo" => "cuMemGetInfo_v2",
        "cuMemAlloc" => "cuMemAlloc_v2",
        "cuMemAllocPitch" => "cuMemAllocPitch_v2",
        "cuMemFree" => "cuMemFree_v2",
        "cuMemGetAddressRange" => "cuMemGetAddressRange_v2",
        "cuMemAllocHost" => "cuMemAllocHost_v2",
        "cuMemHostGetDevicePointer" => "cuMemHostGetDevicePointer_v2",
        "cuMemHostRegister" => "cuMemHostRegister_v2",
        "cuMemcpyHtoD" => "cuMemcpyHtoD_v2",
        "cuMemcpyDtoH" => "cuMemcpyDtoH_v2",
        "cuMemcpyDtoD" => "cuMemcpyDtoD_v2",
        "cuMemcpyHtoDAsync" => "cuMemcpyHtoDAsync_v2",
        "cuMemcpyDtoHAsync" => "cuMemcpyDtoHAsync_v2",
        "cuMemcpyDtoDAsync" => "cuMemcpyDtoDAsync_v2",
        "cuMemsetD8" => "cuMemsetD8_v2",
        "cuMemsetD16" => "cuMemsetD16_v2",
        "cuMemsetD32" => "cuMemsetD32_v2",
        "cuStreamDestroy" => "cuStreamDestroy_v2",
        "cuEventDestroy" => "cuEventDestroy_v2",
        other => other, // already a real exported symbol (versioned or unversioned)
    }
}

#[no_mangle]
pub extern "C" fn cuGetProcAddress(
    symbol: *const c_char,
    pfn: *mut *mut c_void,
    cudaVersion: i32,
    flags: u64,
) -> i32 {
    let _ = (cudaVersion, flags);
    if symbol.is_null() || pfn.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let name = match unsafe { cstr(symbol) } {
        Some(s) => s,
        None => return CUDA_ERROR_INVALID_VALUE,
    };
    let resolved = newest_symbol(&name);
    // Resolve against this object's own exported cu* surface. When deployed as libcuda.so.1 every
    // entry point is a dynamic symbol, so RTLD_DEFAULT finds it.
    let cname = match std::ffi::CString::new(resolved) {
        Ok(c) => c,
        Err(_) => return CUDA_ERROR_INVALID_VALUE,
    };
    let p = unsafe { dlsym(core::ptr::null_mut(), cname.as_ptr()) }; // RTLD_DEFAULT
    if p.is_null() {
        unsafe { *pfn = core::ptr::null_mut() };
        return CUDA_ERROR_NOT_FOUND;
    }
    unsafe { *pfn = p };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuGetProcAddress_v2(
    symbol: *const c_char,
    pfn: *mut *mut c_void,
    cudaVersion: i32,
    flags: u64,
    status: *mut i32,
) -> i32 {
    let r = cuGetProcAddress(symbol, pfn, cudaVersion, flags);
    if !status.is_null() {
        unsafe {
            *status = if r == CUDA_SUCCESS {
                CU_GET_PROC_ADDRESS_SUCCESS
            } else {
                CU_GET_PROC_ADDRESS_SYMBOL_NOT_FOUND
            }
        };
    }
    r
}

/// Write `s` (truncated to `cap-1` bytes) as a NUL-terminated C string into `dst`.
///
/// # Safety
/// `dst` must be writable for `cap` bytes.
fn write_cstr(dst: *mut c_char, cap: usize, s: &str) {
    if cap == 0 {
        return;
    }
    let bytes = s.as_bytes();
    let n = bytes.len().min(cap - 1);
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, n);
        *dst.add(n) = 0;
    }
}
