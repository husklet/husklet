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
    let val = match attrib {
        CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK => d.max_threads_per_block as i32,
        CU_DEVICE_ATTRIBUTE_WARP_SIZE => d.warp_size as i32,
        CU_DEVICE_ATTRIBUTE_CLOCK_RATE => d.clock_khz as i32,
        CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT => d.multiprocessor_count as i32,
        CU_DEVICE_ATTRIBUTE_INTEGRATED => 1, // unified memory on the host
        CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING => 1,
        CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR => d.compute_capability.0 as i32,
        CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR => d.compute_capability.1 as i32,
        _ => 0, // spec-faithful default for the long tail of attributes
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
            .map(|func| s.intern_function(func))
    });
    match handle {
        Some(h) => {
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

    // Use the shared PTX front-end purely to learn each parameter's width + pointer-ness, so we can
    // interpret the untyped `void** kernelParams` (CUDA's calling convention: each slot points at the
    // argument's value). Kernels outside the modeled subset are a traced no-op (long tail).
    let prog = match ptx::compile(&ptx_src, &entry, block) {
        Ok(p) => p,
        Err(_) => {
            crate::stub::hit(
                "cuLaunchKernel (PTX outside the modeled subset — traced, no IR emitted)",
            );
            return CUDA_SUCCESS;
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
        t
    });
    unsafe { *pstream = token as *mut c_void };
    CUDA_SUCCESS
}

#[no_mangle]
pub extern "C" fn cuStreamDestroy_v2(stream: *mut c_void) -> i32 {
    // Flush any work outstanding on the stream (a destroy implies completion), then drop the token.
    let _ = stream;
    state::with(|s| s.flush());
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
    // land the preceding work is here, so flush.
    state::with(|s| s.flush());
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
    let _ = event;
    CUDA_SUCCESS
}
