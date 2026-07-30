//! Fatbinary registration, kernel launch, attributes, and nvcc call configuration.

use core::ffi::{c_char, c_void};

use hl_cuda::adapter::ptx;
use hl_cuda::result::{
    RuntimeStatus, CUDART_ERROR_INVALID_DEVICE_FUNCTION, CUDART_ERROR_INVALID_RESOURCE_HANDLE,
    CUDART_ERROR_INVALID_VALUE, CUDART_SUCCESS,
};
use hl_cuda::service::register::{self, FatbinHandle};

use crate::state::{CallCfg, ShimState};
use crate::Dim3;

// nvcc's wrapper magic and the CUDA fatbinary container magic.
const FATBIN_WRAPPER_MAGIC: u32 = 0x4662_43b1;
const FATBIN_MAGIC: u32 = 0xba55_ed50;

struct CInput;

impl CInput {
    /// Read a nul-terminated C string into an owned `String`.
    unsafe fn string(pointer: *const c_char) -> Option<String> {
        if pointer.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(pointer)
            .to_str()
            .ok()
            .map(str::to_string)
    }
}
/// nvcc's `__fatBinC_Wrapper_t` — the argument `__cudaRegisterFatBinary` actually receives. `data` points
/// at the fatbin container (which itself begins with [`FATBIN_MAGIC`]).
#[repr(C)]
struct FatBinWrapper {
    magic: i32,
    version: i32,
    data: *const c_void,
    filename_or_fatbins: *const c_void,
}

/// Follow `fat_cubin` (a `__fatBinC_Wrapper_t*`, or defensively a bare container) to the fatbin CONTAINER
/// bytes, sized by the container's own `header_size + fat_size`. `None` for a null/foreign/short image.
struct FatbinImage;

impl FatbinImage {
    unsafe fn container_bytes<'a>(fat_cubin: *const c_void) -> Option<&'a [u8]> {
        if fat_cubin.is_null() {
            return None;
        }
        let head = std::ptr::read_unaligned(fat_cubin as *const u32);
        let container: *const u8 = if head == FATBIN_WRAPPER_MAGIC {
            let w = &*(fat_cubin as *const FatBinWrapper);
            if w.data.is_null() {
                return None;
            }
            w.data as *const u8
        } else if head == FATBIN_MAGIC {
            fat_cubin as *const u8
        } else {
            return None;
        };
        if std::ptr::read_unaligned(container as *const u32) != FATBIN_MAGIC {
            return None;
        }
        let header_size = std::ptr::read_unaligned(container.add(6) as *const u16) as usize;
        let fat_size = std::ptr::read_unaligned(container.add(8) as *const u64) as usize;
        let total = header_size.checked_add(fat_size)?;
        Some(std::slice::from_raw_parts(container, total))
    }
}

/// Encode a [`FatbinHandle`] as the opaque `void**` nvcc round-trips back to us: a heap cell whose stored
/// `void*` value is the handle. [`decode_handle`] reads it back.
struct OpaqueFatbinHandle;

impl OpaqueFatbinHandle {
    fn encode(handle: FatbinHandle) -> *mut *mut c_void {
        Box::into_raw(Box::new(handle.0 as *mut c_void))
    }

    unsafe fn decode(handle: *mut *mut c_void) -> Option<FatbinHandle> {
        if handle.is_null() {
            return None;
        }
        Some(FatbinHandle(*handle as u64))
    }
}

/// `__cudaRegisterFatBinary(fatCubin)` — walk the wrapped fatbin to its PTX, load it as a module, and hand
/// nvcc an opaque handle bound to that module. Returns null on a bad image (nvcc tolerates a null handle).
#[no_mangle]
pub extern "C" fn __cudaRegisterFatBinary(fatCubin: *mut c_void) -> *mut *mut c_void {
    let Some(container) = (unsafe { FatbinImage::container_bytes(fatCubin) }) else {
        return core::ptr::null_mut();
    };
    ShimState::with(
        |s| match s.registry.register_fatbinary(&mut s.ctx, container) {
            Ok(handle) => OpaqueFatbinHandle::encode(handle),
            Err(e) => {
                s.fail(RuntimeStatus::from(&e).code());
                core::ptr::null_mut()
            }
        },
    )
}

/// `__cudaRegisterFunction(handle, hostFun, deviceFun, deviceName, …)` — bind the host function pointer
/// `hostFun` to the device entry `deviceName` in the handle's module. `deviceFun` + the launch-bound
/// descriptors are nvcc bookkeeping the lowering does not need.
#[no_mangle]
pub extern "C" fn __cudaRegisterFunction(
    fatCubinHandle: *mut *mut c_void,
    hostFun: *const c_char,
    deviceFun: *mut c_char,
    deviceName: *const c_char,
    thread_limit: i32,
    tid: *mut c_void,
    bid: *mut c_void,
    bDim: *mut c_void,
    gDim: *mut c_void,
    wSize: *mut i32,
) {
    let _ = (deviceFun, thread_limit, tid, bid, bDim, gDim, wSize);
    let Some(handle) = (unsafe { OpaqueFatbinHandle::decode(fatCubinHandle) }) else {
        return;
    };
    let Some(name) = (unsafe { CInput::string(deviceName) }) else {
        return;
    };
    let host_fn = hostFun as usize;
    ShimState::with(|s| {
        if let Err(e) = s.registry.register_function(&s.ctx, handle, host_fn, &name) {
            s.fail(RuntimeStatus::from(&e).code());
        }
    });
}

/// `__cudaRegisterFatBinaryEnd(handle)` — the finalization marker after the last `__cudaRegisterFunction`.
#[no_mangle]
pub extern "C" fn __cudaRegisterFatBinaryEnd(fatCubinHandle: *mut *mut c_void) {
    if let Some(handle) = unsafe { OpaqueFatbinHandle::decode(fatCubinHandle) } {
        ShimState::with(|s| {
            s.registry.register_fatbinary_end(handle);
        });
    }
}

/// `cudaLaunchKernel(func, gridDim, blockDim, args, sharedMem, stream)` — resolve the host-fn pointer to
/// its registered device entry and lower exactly like the driver-API `cuLaunchKernel`, via the shared
/// [`register::launch_kernel`].
///
/// `sharedMem` requests DYNAMIC (`extern __shared__`) shared memory, which is not expressible in the
/// kernel IR — the PTX front-end rejects an `.extern .shared` declaration outright and
/// `cudaFuncAttributes::maxDynamicSharedSizeBytes` reports 0. A non-zero request is therefore
/// `cudaErrorInvalidValue`: running the kernel with none of the shared memory it asked for would return a
/// wrong result. `stream` is validated so a destroyed stream cannot carry a launch.
#[no_mangle]
pub extern "C" fn cudaLaunchKernel(
    func: *const c_void,
    gridDim: Dim3,
    blockDim: Dim3,
    args: *mut *mut c_void,
    sharedMem: usize,
    stream: *mut c_void,
) -> i32 {
    if sharedMem != 0 {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    let host_fn = func as usize;
    let grid = (gridDim.x, gridDim.y, gridDim.z);
    let block = (blockDim.x, blockDim.y, blockDim.z);
    ShimState::with(|s| {
        if s.stream(stream).is_none() {
            return s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE);
        }
        match unsafe {
            register::launch_kernel(
                &mut s.ctx,
                &mut s.sink,
                &s.registry,
                host_fn,
                grid,
                block,
                args as *const *const c_void,
            )
        } {
            Ok(()) => CUDART_SUCCESS,
            Err(e) => s.fail(RuntimeStatus::from(&e).code()),
        }
    })
}

// ==================================================================================================
// function attributes
// ==================================================================================================

/// `cudaFuncAttributes` — a faithful `#[repr(C)]` of the CUDA 12.x layout; the trailing reserved tail is
/// zeroed. The exact field order/type is the ABI contract a caller reads back.
#[repr(C)]
struct CudaFuncAttributes {
    shared_size_bytes: usize,
    const_size_bytes: usize,
    local_size_bytes: usize,
    max_threads_per_block: i32,
    num_regs: i32,
    ptx_version: i32,
    binary_version: i32,
    cache_mode_ca: i32,
    max_dynamic_shared_size_bytes: i32,
    preferred_shmem_carveout: i32,
    cluster_dim_must_be_set: i32,
    required_cluster_width: i32,
    required_cluster_height: i32,
    required_cluster_depth: i32,
    cluster_scheduling_policy_preference: i32,
    non_portable_cluster_size_allowed: i32,
    reserved: [i32; 16],
}

/// Byte offsets into the filled `cudaFuncAttributes` that the tests read back, derived from the
/// `#[repr(C)]` layout itself so an assertion can never drift from the struct it inspects.
#[cfg(test)]
pub(crate) struct FuncAttrOffset;

#[cfg(test)]
impl FuncAttrOffset {
    pub(crate) const MAX_DYNAMIC_SHARED_SIZE_BYTES: usize =
        core::mem::offset_of!(CudaFuncAttributes, max_dynamic_shared_size_bytes);
}

/// `cudaFuncGetAttributes(attr, func)` — the launch-relevant attributes of a device function. `func` is
/// nvcc's host stub pointer; it must resolve through the runtime-API [`register::Registry`] to a real
/// device entry, and the register + static-shared figures are then recovered from the module PTX by the
/// SAME front-end the driver-API `cuFuncGetAttribute` uses.
///
/// A host pointer that was never registered is `cudaErrorInvalidDeviceFunction`, as in real CUDA. It used
/// to fall back to plausible constants (32 registers, 0 shared) and report `cudaSuccess`, so a caller
/// sizing a launch from the attributes of a function that does not exist got numbers with nothing behind
/// them. A null `attr` is `cudaErrorInvalidValue`.
#[no_mangle]
pub extern "C" fn cudaFuncGetAttributes(attr: *mut c_void, func: *const c_void) -> i32 {
    if attr.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    let resolved = ShimState::with(|s| {
        // Resolve the host stub → device Function → real (reg_count, static-shared bytes) via the PTX
        // front-end. A kernel outside the modeled subset still has a device entry, so it reports (0, 0)
        // rather than being mistaken for an unregistered function.
        let function = s.registry.resolve(func as usize)?;
        let (regs, shared) = s
            .ctx
            .entry_source(function)
            .and_then(|(src, entry)| ptx::compile(&src, &entry, [1, 1, 1]).ok())
            .map(|p| (p.reg_count as i32, p.shared_bytes as usize))
            .unwrap_or((0, 0));
        Some((s.ctx.device.compute_capability, regs, shared))
    });
    let Some((cc, num_regs, shared_size_bytes)) = resolved else {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_DEVICE_FUNCTION));
    };
    let ptx_ver = cc.0 as i32 * 10 + cc.1 as i32;
    let a = CudaFuncAttributes {
        shared_size_bytes,
        const_size_bytes: 0,
        local_size_bytes: 0,
        max_threads_per_block: 1024,
        num_regs,
        ptx_version: ptx_ver,
        binary_version: ptx_ver,
        cache_mode_ca: 0,
        // Dynamic shared memory is not expressible in the kernel IR, so the opt-in maximum is 0.
        max_dynamic_shared_size_bytes: 0,
        preferred_shmem_carveout: -1,
        cluster_dim_must_be_set: 0,
        required_cluster_width: 0,
        required_cluster_height: 0,
        required_cluster_depth: 0,
        cluster_scheduling_policy_preference: 0,
        non_portable_cluster_size_allowed: 0,
        reserved: [0; 16],
    };
    unsafe { *(attr as *mut CudaFuncAttributes) = a };
    CUDART_SUCCESS
}

// ==================================================================================================
// nvcc glue tail: `__cudaRegisterVar`, `__cudaUnregisterFatBinary`, `<<<>>>` call-config stack
// ==================================================================================================

/// `__cudaRegisterVar(handle, hostVar, deviceAddress, deviceName, ext, size, constant, global)` — nvcc
/// binds a `__device__`/`__constant__` global. It returns `void`, so it cannot report a failure, and hl's
/// PTX model parses only kernel entries: there is no module-scope storage to bind.
///
/// That makes the no-op honest only because the KERNEL that would use such a global is refused: the PTX
/// front-end rejects a module-scope symbol in a `.global` address operand
/// (`ld.global.u32 %r1, [gCounter];`) rather than interning the name as a fresh zero register, so
/// `cudaLaunchKernel` returns `cudaErrorInvalidPtx` instead of silently reading zeros. Without that
/// rejection this no-op would be a wrong-result bug rather than a missing feature.
#[no_mangle]
pub extern "C" fn __cudaRegisterVar(
    fatCubinHandle: *mut *mut c_void,
    hostVar: *mut c_char,
    deviceAddress: *mut c_char,
    deviceName: *const c_char,
    ext: i32,
    size: usize,
    constant: i32,
    global: i32,
) {
    let _ = (
        fatCubinHandle,
        hostVar,
        deviceAddress,
        deviceName,
        ext,
        size,
        constant,
        global,
    );
}

/// `__cudaUnregisterFatBinary(handle)` — drop the fatbin handle's module binding. The loaded module stays
/// resident in the context (a stale launch may still reference it), so this only forgets the handle.
#[no_mangle]
pub extern "C" fn __cudaUnregisterFatBinary(fatCubinHandle: *mut *mut c_void) {
    if let Some(handle) = unsafe { OpaqueFatbinHandle::decode(fatCubinHandle) } {
        ShimState::with(|s| s.registry.unregister_fatbinary(handle));
    }
}

/// `__cudaPushCallConfiguration(gridDim, blockDim, sharedMem, stream)` — nvcc emits this for each `<<<>>>`
/// launch to stash the configuration; it returns `0` when the config was accepted (the host stub then
/// proceeds to the device stub, which pops it back). A stack overflow returns nonzero so the stub skips
/// the launch.
#[no_mangle]
pub extern "C" fn __cudaPushCallConfiguration(
    gridDim: Dim3,
    blockDim: Dim3,
    sharedMem: usize,
    stream: *mut c_void,
) -> u32 {
    let cfg = CallCfg {
        grid: [gridDim.x, gridDim.y, gridDim.z],
        block: [blockDim.x, blockDim.y, blockDim.z],
        shmem: sharedMem,
        stream: stream as usize,
    };
    if ShimState::with(|s| s.push_call_config(cfg)) {
        0
    } else {
        1
    }
}

/// `__cudaPopCallConfiguration(gridDim, blockDim, sharedMem, stream)` — the matching pop inside nvcc's
/// generated device stub; writes the most-recently-pushed config back into the caller's out-params. An
/// empty stack is `cudaErrorInvalidConfiguration` (9).
#[no_mangle]
pub extern "C" fn __cudaPopCallConfiguration(
    gridDim: *mut Dim3,
    blockDim: *mut Dim3,
    sharedMem: *mut usize,
    stream: *mut c_void,
) -> i32 {
    const CUDART_ERROR_INVALID_CONFIGURATION: i32 = 9;
    let Some(c) = ShimState::with(|s| s.pop_call_config()) else {
        return CUDART_ERROR_INVALID_CONFIGURATION;
    };
    unsafe {
        if !gridDim.is_null() {
            *gridDim = Dim3 {
                x: c.grid[0],
                y: c.grid[1],
                z: c.grid[2],
            };
        }
        if !blockDim.is_null() {
            *blockDim = Dim3 {
                x: c.block[0],
                y: c.block[1],
                z: c.block[2],
            };
        }
        if !sharedMem.is_null() {
            *sharedMem = c.shmem;
        }
        if !stream.is_null() {
            // `stream` points at the caller's `cudaStream_t` slot.
            *(stream as *mut *mut c_void) = c.stream as *mut c_void;
        }
    }
    CUDART_SUCCESS
}
