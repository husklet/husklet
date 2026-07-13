//! The hand-written CUDA Runtime API entry points — the whole surface, at parity with dd's C oracle
//! `hl-gpu/cuda/cudart_shim.c`. The stateful compute calls lower the CUDA model into the shared `dd-gpu`
//! IR through [`hl_gpu::cuda::CudaContext`] and EXECUTE it on the embedded
//! [`SoftwareBackend`](hl_gpu::software::SoftwareBackend) (CPU PTX interpreter), so a runtime vecadd
//! runs end-to-end and `cudaMemcpy(...DeviceToHost)` reads back numerically-correct results with NO GPU
//! — the same numbers the oracle produces. This mirrors dd-shim-cuda's driver bodies (the runtime API
//! maps to the same IR — reuse, don't redefine); cudart adds the `cudaError_t` surface, the last-error
//! cell, the nvcc registration glue, and the fatbin → PTX walk on top.

use core::ffi::{c_char, c_void};

use hl_gpu::cuda::{DevicePtr, KernelArg};
use hl_gpu::ptx;

use crate::result::*;
use crate::state::{self, CallCfg};

/// `dim3` — the C ABI passes it by value as three unsigned ints.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Dim3 {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

/// Read a NUL-terminated C string (best-effort, lossy). `None` for null.
///
/// # Safety
/// `p` must be null or a valid NUL-terminated C string.
unsafe fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
}

// ==================================================================================================
// Tier 0 — device management
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaGetDeviceCount(count: *mut i32) -> i32 {
    if count.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    state::ensure_init();
    unsafe { *count = 1 }; // one simulated device
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaSetDevice(device: i32) -> i32 {
    state::ensure_init();
    if device != 0 {
        return state::rec(CUDA_ERROR_INVALID_DEVICE_RT);
    }
    state::set_current_device(device);
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaGetDevice(device: *mut i32) -> i32 {
    if device.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    state::ensure_init();
    unsafe { *device = state::current_device() };
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaDeviceSynchronize() -> i32 {
    state::ensure_init();
    state::with(|s| s.flush()); // the executor is synchronous; flushing is the sync point
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaThreadSynchronize() -> i32 {
    cudaDeviceSynchronize() // legacy alias
}

#[no_mangle]
pub extern "C" fn cudaDeviceReset() -> i32 {
    state::ensure_init();
    state::with(|s| s.flush());
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaDeviceGetPCIBusId(pciBusId: *mut c_char, len: i32, device: i32) -> i32 {
    state::ensure_init();
    if pciBusId.is_null() || len <= 0 || device != 0 {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    let id = state::with(|s| s.ctx.device.pci_bus_id.clone());
    let bytes = id.as_bytes();
    let cap = (len as usize) - 1;
    let n = bytes.len().min(cap);
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), pciBusId as *mut u8, n);
        *pciBusId.add(n) = 0;
    }
    state::rec(CUDA_SUCCESS_RT)
}

/// `cudaDeviceProp` — a faithful clean-room reconstruction of the CUDA 12.x layout (matches
/// `hl-gpu/cuda/cudart_min.h`). A `#[repr(C)]` with the exact field order/types is required for ABI
/// correctness; fields we do not populate stay zeroed.
#[repr(C)]
struct CudaDeviceProp {
    name: [c_char; 256],
    uuid: [c_char; 16],
    luid: [c_char; 8],
    luid_device_node_mask: u32,
    total_global_mem: usize,
    shared_mem_per_block: usize,
    regs_per_block: i32,
    warp_size: i32,
    mem_pitch: usize,
    max_threads_per_block: i32,
    max_threads_dim: [i32; 3],
    max_grid_size: [i32; 3],
    clock_rate: i32,
    total_const_mem: usize,
    major: i32,
    minor: i32,
    texture_alignment: usize,
    texture_pitch_alignment: usize,
    device_overlap: i32,
    multi_processor_count: i32,
    kernel_exec_timeout_enabled: i32,
    integrated: i32,
    can_map_host_memory: i32,
    compute_mode: i32,
    max_texture_1d: i32,
    max_texture_1d_mipmap: i32,
    max_texture_1d_linear: i32,
    max_texture_2d: [i32; 2],
    max_texture_2d_mipmap: [i32; 2],
    max_texture_2d_linear: [i32; 3],
    max_texture_2d_gather: [i32; 2],
    max_texture_3d: [i32; 3],
    max_texture_3d_alt: [i32; 3],
    max_texture_cubemap: i32,
    max_texture_1d_layered: [i32; 2],
    max_texture_2d_layered: [i32; 3],
    max_texture_cubemap_layered: [i32; 2],
    max_surface_1d: i32,
    max_surface_2d: [i32; 2],
    max_surface_3d: [i32; 3],
    max_surface_1d_layered: [i32; 2],
    max_surface_2d_layered: [i32; 3],
    max_surface_cubemap: i32,
    max_surface_cubemap_layered: [i32; 2],
    surface_alignment: usize,
    concurrent_kernels: i32,
    ecc_enabled: i32,
    pci_bus_id: i32,
    pci_device_id: i32,
    pci_domain_id: i32,
    tcc_driver: i32,
    async_engine_count: i32,
    unified_addressing: i32,
    memory_clock_rate: i32,
    memory_bus_width: i32,
    l2_cache_size: i32,
    persisting_l2_cache_max_size: i32,
    max_threads_per_multiprocessor: i32,
    stream_priorities_supported: i32,
    global_l1_cache_supported: i32,
    local_l1_cache_supported: i32,
    shared_mem_per_multiprocessor: usize,
    regs_per_multiprocessor: i32,
    managed_memory: i32,
    is_multi_gpu_board: i32,
    multi_gpu_board_group_id: i32,
    host_native_atomic_supported: i32,
    single_to_double_precision_perf_ratio: i32,
    pageable_memory_access: i32,
    concurrent_managed_access: i32,
    compute_preemption_supported: i32,
    can_use_host_pointer_for_registered_mem: i32,
    cooperative_launch: i32,
    cooperative_multi_device_launch: i32,
    shared_mem_per_block_optin: usize,
    pageable_memory_access_uses_host_page_tables: i32,
    direct_managed_mem_access_from_host: i32,
    max_blocks_per_multiprocessor: i32,
    access_policy_max_window_size: i32,
    reserved_shared_mem_per_block: usize,
    host_register_supported: i32,
    sparse_cuda_array_supported: i32,
    host_register_read_only_supported: i32,
    timeline_semaphore_interop_supported: i32,
    memory_pools_supported: i32,
    gpu_direct_rdma_supported: i32,
    gpu_direct_rdma_flush_writes_options: u32,
    gpu_direct_rdma_writes_ordering: i32,
    memory_pool_supported_handle_types: u32,
    deferred_mapping_cuda_array_supported: i32,
    ipc_event_supported: i32,
    cluster_launch: i32,
    unified_function_pointers: i32,
    reserved2: [i32; 2],
    reserved1: [i32; 1],
    reserved: [i32; 60],
}

#[no_mangle]
pub extern "C" fn cudaGetDeviceProperties_v2(prop: *mut c_void, device: i32) -> i32 {
    state::ensure_init();
    if prop.is_null() || device != 0 {
        return state::rec(CUDA_ERROR_INVALID_DEVICE_RT);
    }
    let p = unsafe { &mut *(prop as *mut CudaDeviceProp) };
    // Zero the whole struct (the C oracle memsets first).
    unsafe {
        core::ptr::write_bytes(p as *mut CudaDeviceProp as *mut u8, 0, core::mem::size_of::<CudaDeviceProp>())
    };
    // Values mirror the C oracle's fill_props (which reads them from the driver's device model +
    // attribute switch); here we read the same `CudaDeviceDesc` + constants directly.
    let d = state::with(|s| s.ctx.device.clone());
    let nb = d.name.as_bytes();
    let n = nb.len().min(255);
    unsafe { core::ptr::copy_nonoverlapping(nb.as_ptr(), p.name.as_mut_ptr() as *mut u8, n) };
    unsafe { core::ptr::copy_nonoverlapping(d.uuid.as_ptr(), p.uuid.as_mut_ptr() as *mut u8, 16) };
    p.total_global_mem = d.total_mem as usize;
    p.major = d.compute_capability.0 as i32;
    p.minor = d.compute_capability.1 as i32;
    p.warp_size = d.warp_size as i32;
    p.clock_rate = d.clock_khz as i32;
    p.multi_processor_count = d.multiprocessor_count as i32;
    p.max_threads_per_block = 1024;
    p.max_threads_dim = [1024, 1024, 64];
    p.max_grid_size = [2147483647, 65535, 65535];
    p.regs_per_block = 65536;
    p.shared_mem_per_block = 49152;
    p.total_const_mem = 65536;
    p.shared_mem_per_multiprocessor = 102400;
    p.integrated = 1; // unified memory on the host
    p.can_map_host_memory = 1;
    p.compute_mode = 0;
    p.concurrent_kernels = 1;
    p.ecc_enabled = 0;
    p.memory_clock_rate = 6251000;
    p.memory_bus_width = 256;
    p.l2_cache_size = 4194304;
    p.max_threads_per_multiprocessor = 2048;
    p.async_engine_count = 2;
    p.unified_addressing = 1;
    p.managed_memory = 1;
    p.concurrent_managed_access = 1;
    p.pageable_memory_access = 1;
    p.texture_alignment = 512;
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaGetDeviceProperties(prop: *mut c_void, device: i32) -> i32 {
    cudaGetDeviceProperties_v2(prop, device)
}

// ==================================================================================================
// Tier 0 — error reporting + versions
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaGetLastError() -> i32 {
    state::take_last_error()
}

#[no_mangle]
pub extern "C" fn cudaPeekAtLastError() -> i32 {
    state::peek_last_error()
}

#[no_mangle]
pub extern "C" fn cudaGetErrorName(e: i32) -> *const c_char {
    let s: &'static [u8] = match e {
        CUDA_SUCCESS_RT => b"cudaSuccess\0",
        CUDA_ERROR_INVALID_VALUE_RT => b"cudaErrorInvalidValue\0",
        CUDA_ERROR_MEMORY_ALLOCATION => b"cudaErrorMemoryAllocation\0",
        CUDA_ERROR_INITIALIZATION => b"cudaErrorInitializationError\0",
        CUDA_ERROR_CUDART_UNLOADING => b"cudaErrorCudartUnloading\0",
        CUDA_ERROR_INVALID_CONFIGURATION => b"cudaErrorInvalidConfiguration\0",
        CUDA_ERROR_INVALID_MEMCPY_DIRECTION => b"cudaErrorInvalidMemcpyDirection\0",
        CUDA_ERROR_INVALID_DEVICE_FUNCTION => b"cudaErrorInvalidDeviceFunction\0",
        CUDA_ERROR_NO_DEVICE_RT => b"cudaErrorNoDevice\0",
        CUDA_ERROR_INVALID_DEVICE_RT => b"cudaErrorInvalidDevice\0",
        CUDA_ERROR_INVALID_KERNEL_IMAGE => b"cudaErrorInvalidKernelImage\0",
        CUDA_ERROR_DEVICE_UNINITIALIZED => b"cudaErrorDeviceUninitialized\0",
        CUDA_ERROR_NO_KERNEL_IMAGE_FOR_DEVICE => b"cudaErrorNoKernelImageForDevice\0",
        CUDA_ERROR_INVALID_PTX_RT => b"cudaErrorInvalidPtx\0",
        CUDA_ERROR_INVALID_RESOURCE_HANDLE => b"cudaErrorInvalidResourceHandle\0",
        CUDA_ERROR_SYMBOL_NOT_FOUND => b"cudaErrorSymbolNotFound\0",
        CUDA_ERROR_NOT_READY_RT => b"cudaErrorNotReady\0",
        CUDA_ERROR_ILLEGAL_ADDRESS_RT => b"cudaErrorIllegalAddress\0",
        CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES_RT => b"cudaErrorLaunchOutOfResources\0",
        CUDA_ERROR_LAUNCH_FAILURE => b"cudaErrorLaunchFailure\0",
        CUDA_ERROR_NOT_SUPPORTED_RT => b"cudaErrorNotSupported\0",
        CUDA_ERROR_UNKNOWN_RT => b"cudaErrorUnknown\0",
        _ => b"cudaErrorUnknown\0",
    };
    s.as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn cudaGetErrorString(e: i32) -> *const c_char {
    let s: &'static [u8] = match e {
        CUDA_SUCCESS_RT => b"no error\0",
        CUDA_ERROR_INVALID_VALUE_RT => b"invalid argument\0",
        CUDA_ERROR_MEMORY_ALLOCATION => b"out of memory\0",
        CUDA_ERROR_INITIALIZATION => b"initialization error\0",
        CUDA_ERROR_CUDART_UNLOADING => b"driver shutting down\0",
        CUDA_ERROR_INVALID_CONFIGURATION => b"invalid configuration argument\0",
        CUDA_ERROR_INVALID_MEMCPY_DIRECTION => b"invalid copy direction for memcpy\0",
        CUDA_ERROR_INVALID_DEVICE_FUNCTION => b"invalid device function\0",
        CUDA_ERROR_NO_DEVICE_RT => b"no CUDA-capable device is detected\0",
        CUDA_ERROR_INVALID_DEVICE_RT => b"invalid device ordinal\0",
        CUDA_ERROR_INVALID_KERNEL_IMAGE => b"device kernel image is invalid\0",
        CUDA_ERROR_DEVICE_UNINITIALIZED => b"invalid device context\0",
        CUDA_ERROR_NO_KERNEL_IMAGE_FOR_DEVICE => {
            b"no kernel image is available for execution on the device\0"
        }
        CUDA_ERROR_INVALID_PTX_RT => b"a PTX JIT compilation failed\0",
        CUDA_ERROR_INVALID_RESOURCE_HANDLE => b"invalid resource handle\0",
        CUDA_ERROR_SYMBOL_NOT_FOUND => b"named symbol not found\0",
        CUDA_ERROR_NOT_READY_RT => b"device not ready\0",
        CUDA_ERROR_ILLEGAL_ADDRESS_RT => b"an illegal memory access was encountered\0",
        CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES_RT => b"too many resources requested for launch\0",
        CUDA_ERROR_LAUNCH_FAILURE => b"unspecified launch failure\0",
        CUDA_ERROR_NOT_SUPPORTED_RT => b"operation not supported\0",
        CUDA_ERROR_UNKNOWN_RT => b"unknown error\0",
        _ => b"unknown error\0",
    };
    s.as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn cudaDriverGetVersion(v: *mut i32) -> i32 {
    if v.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    unsafe { *v = DRIVER_VERSION };
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaRuntimeGetVersion(v: *mut i32) -> i32 {
    if v.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    unsafe { *v = RUNTIME_VERSION };
    state::rec(CUDA_SUCCESS_RT)
}

// ==================================================================================================
// Tier 0 — memory (a device pointer is a `u64`; on unified memory it fits a `void*`)
// ==================================================================================================

/// Shared allocation body (`cudaMalloc` / `cudaMallocManaged`): mem_alloc → CreateBuffer IR, register.
fn alloc(devPtr: *mut *mut c_void, size: usize, kind: u8) -> i32 {
    if devPtr.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    state::ensure_init();
    if size == 0 {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    let p = state::with(|s| {
        let (p, cmd) = s.ctx.mem_alloc(size as u64);
        s.frame.push(cmd);
        s.register_alloc(p.0, size as u64, kind);
        p.0
    });
    unsafe { *devPtr = p as usize as *mut c_void };
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaMalloc(devPtr: *mut *mut c_void, size: usize) -> i32 {
    alloc(devPtr, size, state::ALLOC_DEVICE)
}

#[no_mangle]
pub extern "C" fn cudaMallocManaged(devPtr: *mut *mut c_void, size: usize, flags: u32) -> i32 {
    let _ = flags;
    alloc(devPtr, size, state::ALLOC_MANAGED)
}

#[no_mangle]
pub extern "C" fn cudaFree(devPtr: *mut c_void) -> i32 {
    state::ensure_init();
    if devPtr.is_null() {
        return state::rec(CUDA_SUCCESS_RT); // cudaFree(NULL) is a no-op success
    }
    state::with(|s| {
        if let Some(cmd) = s.ctx.mem_free(DevicePtr(devPtr as usize as u64)) {
            s.frame.push(cmd);
        }
        s.unregister_alloc(devPtr as usize as u64);
    });
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaMallocHost(ptr: *mut *mut c_void, size: usize) -> i32 {
    if ptr.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    state::ensure_init();
    let p = state::with(|s| s.host_alloc(size, state::ALLOC_HOST));
    unsafe { *ptr = p };
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaHostAlloc(ptr: *mut *mut c_void, size: usize, flags: u32) -> i32 {
    let _ = flags;
    cudaMallocHost(ptr, size)
}

#[no_mangle]
pub extern "C" fn cudaFreeHost(ptr: *mut c_void) -> i32 {
    state::ensure_init();
    if !ptr.is_null() {
        state::with(|s| s.host_free(ptr));
    }
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaHostGetDevicePointer(
    pDevice: *mut *mut c_void,
    pHost: *mut c_void,
    flags: u32,
) -> i32 {
    let _ = flags;
    state::ensure_init();
    if pDevice.is_null() || pHost.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    unsafe { *pDevice = pHost }; // unified: host and device addresses coincide
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaMemcpy(dst: *mut c_void, src: *const c_void, count: usize, kind: i32) -> i32 {
    state::ensure_init();
    let e = match kind {
        CUDA_MEMCPY_HOST_TO_DEVICE => {
            if src.is_null() && count > 0 {
                CUDA_ERROR_INVALID_VALUE_RT
            } else {
                let bytes = unsafe { core::slice::from_raw_parts(src as *const u8, count) };
                let ok = state::with(|s| match s.ctx.memcpy_htod(DevicePtr(dst as usize as u64), bytes) {
                    Some(cmd) => {
                        s.frame.push(cmd);
                        true
                    }
                    None => false,
                });
                if ok {
                    CUDA_SUCCESS_RT
                } else {
                    CUDA_ERROR_INVALID_VALUE_RT
                }
            }
        }
        CUDA_MEMCPY_DEVICE_TO_HOST => {
            if dst.is_null() && count > 0 {
                CUDA_ERROR_INVALID_VALUE_RT
            } else if count == 0 {
                CUDA_SUCCESS_RT
            } else {
                let out = unsafe { core::slice::from_raw_parts_mut(dst as *mut u8, count) };
                let ok = state::with(|s| s.read_device(DevicePtr(src as usize as u64), out));
                if ok {
                    CUDA_SUCCESS_RT
                } else {
                    CUDA_ERROR_INVALID_VALUE_RT
                }
            }
        }
        CUDA_MEMCPY_DEVICE_TO_DEVICE | CUDA_MEMCPY_DEFAULT => {
            if count == 0 {
                CUDA_SUCCESS_RT
            } else {
                let ok = state::with(|s| {
                    s.copy_dtod(DevicePtr(dst as usize as u64), DevicePtr(src as usize as u64), count)
                });
                if ok {
                    CUDA_SUCCESS_RT
                } else {
                    CUDA_ERROR_INVALID_VALUE_RT
                }
            }
        }
        CUDA_MEMCPY_HOST_TO_HOST => {
            if count != 0 && (dst.is_null() || src.is_null()) {
                CUDA_ERROR_INVALID_VALUE_RT
            } else {
                if count != 0 {
                    unsafe { core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, count) };
                }
                CUDA_SUCCESS_RT
            }
        }
        _ => CUDA_ERROR_INVALID_MEMCPY_DIRECTION,
    };
    state::rec(e)
}

#[no_mangle]
pub extern "C" fn cudaMemcpyAsync(
    dst: *mut c_void,
    src: *const c_void,
    count: usize,
    kind: i32,
    stream: *mut c_void,
) -> i32 {
    let _ = stream; // synchronous executor: async == sync
    cudaMemcpy(dst, src, count, kind)
}

#[no_mangle]
pub extern "C" fn cudaMemset(devPtr: *mut c_void, value: i32, count: usize) -> i32 {
    state::ensure_init();
    if count == 0 {
        return state::rec(CUDA_SUCCESS_RT);
    }
    if devPtr.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    let pattern = vec![value as u8; count];
    let ok = state::with(|s| s.memset(DevicePtr(devPtr as usize as u64), &pattern));
    state::rec(if ok { CUDA_SUCCESS_RT } else { CUDA_ERROR_INVALID_VALUE_RT })
}

#[no_mangle]
pub extern "C" fn cudaMemsetAsync(devPtr: *mut c_void, value: i32, count: usize, stream: *mut c_void) -> i32 {
    let _ = stream;
    cudaMemset(devPtr, value, count)
}

#[no_mangle]
pub extern "C" fn cudaMemGetInfo(freeB: *mut usize, totalB: *mut usize) -> i32 {
    state::ensure_init();
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
    state::rec(CUDA_SUCCESS_RT)
}

// ==================================================================================================
// Tier 0 — streams (a runtime cudaStream_t is a non-null scheduling token)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaStreamCreate(pStream: *mut *mut c_void) -> i32 {
    cudaStreamCreateWithFlags(pStream, 0)
}

#[no_mangle]
pub extern "C" fn cudaStreamCreateWithFlags(pStream: *mut *mut c_void, flags: u32) -> i32 {
    let _ = flags;
    if pStream.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    state::ensure_init();
    // The executor completes each submit synchronously and ordering is preserved by the single
    // accumulated frame, so a stream is a non-null scheduling token (no per-stream queue needed).
    let token = state::with(|s| {
        let t = s.next_stream;
        s.next_stream += 1;
        t
    });
    unsafe { *pStream = token as *mut c_void };
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaStreamDestroy(stream: *mut c_void) -> i32 {
    let _ = stream;
    state::with(|s| s.flush()); // a destroy implies completion
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaStreamSynchronize(stream: *mut c_void) -> i32 {
    let _ = stream;
    state::ensure_init();
    state::with(|s| s.flush());
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaStreamWaitEvent(stream: *mut c_void, event: *mut c_void, flags: u32) -> i32 {
    let _ = (stream, event, flags);
    state::ensure_init();
    state::rec(CUDA_SUCCESS_RT) // synchronous executor: the awaited work has already completed
}

#[no_mangle]
pub extern "C" fn cudaStreamQuery(stream: *mut c_void) -> i32 {
    let _ = stream;
    state::ensure_init();
    state::rec(CUDA_SUCCESS_RT) // synchronous executor: always ready
}

// ==================================================================================================
// Tier 0 — events (record/query/elapsed via the monotonic clock, like the oracle)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaEventCreate(event: *mut *mut c_void) -> i32 {
    cudaEventCreateWithFlags(event, 0)
}

#[no_mangle]
pub extern "C" fn cudaEventCreateWithFlags(event: *mut *mut c_void, flags: u32) -> i32 {
    let _ = flags;
    if event.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    state::ensure_init();
    let token = state::with(|s| {
        let t = s.next_event;
        s.next_event += 1;
        s.register_event(t);
        t
    });
    unsafe { *event = token as *mut c_void };
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaEventDestroy(event: *mut c_void) -> i32 {
    state::with(|s| s.unregister_event(event as usize));
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaEventRecord(event: *mut c_void, stream: *mut c_void) -> i32 {
    let _ = stream;
    if event.is_null() {
        return state::rec(CUDA_ERROR_INVALID_RESOURCE_HANDLE);
    }
    // With a synchronous executor the correct place to land preceding work is here: flush, then
    // timestamp so cudaEventElapsedTime is truthful.
    state::with(|s| {
        s.flush();
        s.record_event(event as usize);
    });
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaEventSynchronize(event: *mut c_void) -> i32 {
    if event.is_null() {
        return state::rec(CUDA_ERROR_INVALID_RESOURCE_HANDLE);
    }
    state::with(|s| s.flush());
    state::rec(CUDA_SUCCESS_RT)
}

#[no_mangle]
pub extern "C" fn cudaEventQuery(event: *mut c_void) -> i32 {
    state::ensure_init();
    if event.is_null() {
        return state::rec(CUDA_ERROR_INVALID_RESOURCE_HANDLE);
    }
    // Synchronous executor: a recorded event is complete; an unrecorded one is not ready.
    if state::with(|s| s.event_recorded(event as usize)) {
        state::rec(CUDA_SUCCESS_RT)
    } else {
        state::rec(CUDA_ERROR_NOT_READY_RT)
    }
}

#[no_mangle]
pub extern "C" fn cudaEventElapsedTime(ms: *mut f32, start: *mut c_void, end: *mut c_void) -> i32 {
    if ms.is_null() || start.is_null() || end.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    match state::with(|s| s.event_elapsed_ms(start as usize, end as usize)) {
        Some(v) => {
            unsafe { *ms = v };
            state::rec(CUDA_SUCCESS_RT)
        }
        None => state::rec(CUDA_ERROR_NOT_READY_RT), // one or both events unrecorded
    }
}

// ==================================================================================================
// Tier 1 — kernel launch + function attributes
// ==================================================================================================

/// `cudaFuncAttributes` — a minimal faithful `#[repr(C)]` prefix; the trailing reserved tail is zeroed.
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

#[no_mangle]
pub extern "C" fn cudaFuncGetAttributes(attr: *mut c_void, func: *const c_void) -> i32 {
    let _ = func;
    if attr.is_null() {
        return state::rec(CUDA_ERROR_INVALID_VALUE_RT);
    }
    state::ensure_init();
    let cc = state::with(|s| s.ctx.device.compute_capability);
    let ptx_ver = cc.0 as i32 * 10 + cc.1 as i32;
    // Modeled defaults matching the driver's cuFuncGetAttribute answers — dd's PTX model does not track
    // per-kernel register/shared pressure, so these are honest GPU-free values.
    let a = CudaFuncAttributes {
        shared_size_bytes: 0,
        const_size_bytes: 0,
        local_size_bytes: 0,
        max_threads_per_block: 1024,
        num_regs: 32,
        ptx_version: ptx_ver,
        binary_version: ptx_ver,
        cache_mode_ca: 0,
        max_dynamic_shared_size_bytes: 49152,
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
    state::rec(CUDA_SUCCESS_RT)
}

/// Load the module for a registered fatbin on first use: walk the fatbin → extract PTX → `module_load`.
/// Returns the driver module id (or 0) + the CUresult-style load result.
fn ensure_module_loaded(fat: usize) -> (u32, i32) {
    let (loaded, module, load_res, fatcubin) = state::with_registry(|reg| {
        let fb = &reg.fatbins[fat - 1];
        (fb.loaded, fb.module, fb.load_res, fb.fatcubin)
    });
    if loaded {
        return (module, load_res);
    }
    let (module, res) = match unsafe { crate::fatbin::extract_ptx(fatcubin as *const c_void) } {
        Some(ptx) => {
            let src = String::from_utf8_lossy(&ptx).into_owned();
            let id = state::with(|s| s.ctx.module_load(&src));
            (id, CUDA_SUCCESS_RT)
        }
        // No usable PTX (SASS-only / compressed / malformed) -> InvalidKernelImage, like the driver.
        None => (0, CUDA_ERROR_INVALID_KERNEL_IMAGE),
    };
    state::with_registry(|reg| {
        let fb = &mut reg.fatbins[fat - 1];
        fb.module = module;
        fb.load_res = res;
        fb.loaded = true;
    });
    (module, res)
}

#[no_mangle]
pub extern "C" fn cudaLaunchKernel(
    func: *const c_void,
    gridDim: Dim3,
    blockDim: Dim3,
    args: *mut *mut c_void,
    sharedMem: usize,
    stream: *mut c_void,
) -> i32 {
    let _ = (sharedMem, stream);
    state::ensure_init();
    let host_fun = func as usize;

    // Resolve host-stub -> registered fatbin + device kernel name.
    let Some((fat, name)) = state::with_registry(|reg| {
        reg.funcs
            .iter()
            .find(|fr| fr.host_fun == host_fun)
            .filter(|fr| fr.fat != 0 && fr.fat <= reg.fatbins.len() && reg.fatbins[fr.fat - 1].live)
            .map(|fr| (fr.fat, fr.name.clone()))
    }) else {
        return state::rec(CUDA_ERROR_INVALID_DEVICE_FUNCTION);
    };

    crate::stub::note(format!(
        "cudaLaunchKernel(kernel=`{name}`, grid=({},{},{}), block=({},{},{}))",
        gridDim.x, gridDim.y, gridDim.z, blockDim.x, blockDim.y, blockDim.z
    ));

    // Lazily load the module (walk fatbin -> PTX -> module_load).
    let (module, load_res) = ensure_module_loaded(fat);
    if load_res != CUDA_SUCCESS_RT {
        return state::rec(load_res);
    }

    // Resolve + intern the function, then launch — all in the compute state.
    let block = [blockDim.x.max(1), blockDim.y.max(1), blockDim.z.max(1)];
    let grid = (gridDim.x.max(1), gridDim.y.max(1), gridDim.z.max(1));
    let r = state::with(|s| {
        let Some(func) = s.ctx.module_get_function(module, &name) else {
            return CUDA_ERROR_SYMBOL_NOT_FOUND;
        };
        // Recover the module PTX + entry name to learn the parameter layout for packing `args`.
        let Some((ptx_src, entry)) = s.ctx.module(func.module).and_then(|md| {
            md.entries.get(func.entry as usize).map(|e| (md.source.clone(), e.clone()))
        }) else {
            return CUDA_ERROR_INVALID_RESOURCE_HANDLE;
        };
        let prog = match ptx::compile(&ptx_src, &entry, block) {
            Ok(p) => p,
            // A kernel outside the modeled PTX subset CANNOT be executed. Instead of the old false
            // `cudaSuccess` no-op (which left the output buffer unwritten and moved the failure
            // elsewhere), return the accurate runtime error: `cudaErrorNotSupported` for an unsupported
            // instruction/feature, `cudaErrorInvalidPtx` for malformed PTX (the executor's `Display`
            // text phrases every "outside the subset" rejection with "unsupported" — ptx.rs is the
            // read-only reference, so we classify from its message).
            Err(e) => {
                let code = if e.to_string().contains("unsupported") {
                    CUDA_ERROR_NOT_SUPPORTED_RT
                } else {
                    CUDA_ERROR_INVALID_PTX_RT
                };
                crate::stub::unsupported("cudaLaunchKernel", &format!("kernel `{name}`: {e}"));
                return code;
            }
        };
        // CUDA calling convention: each `args` slot points at the argument's value.
        let mut kargs: Vec<KernelArg> = Vec::with_capacity(prog.params.len());
        if !args.is_null() {
            for (i, p) in prog.params.iter().enumerate() {
                let slot = unsafe { *args.add(i) };
                if slot.is_null() {
                    break;
                }
                if p.is_ptr {
                    let dptr = unsafe { *(slot as *const u64) };
                    kargs.push(KernelArg::Ptr(DevicePtr(dptr)));
                } else {
                    let bytes = unsafe { core::slice::from_raw_parts(slot as *const u8, p.width as usize) };
                    kargs.push(KernelArg::Scalar(bytes.to_vec()));
                }
            }
        }
        let bl = (block[0], block[1], block[2]);
        for cmd in s.ctx.launch(func, grid, bl, &kargs) {
            s.frame.push(cmd);
        }
        CUDA_SUCCESS_RT
    });
    state::rec(r)
}

// ==================================================================================================
// Tier 1 — nvcc compiler registration glue + <<<>>> call-config stack
// ==================================================================================================

#[no_mangle]
pub extern "C" fn __cudaRegisterFatBinary(fatCubin: *mut c_void) -> *mut *mut c_void {
    state::ensure_init();
    let handle = state::with_registry(|reg| {
        reg.fatbins.push(state::FatBin {
            fatcubin: fatCubin as usize,
            module: 0,
            loaded: false,
            load_res: 0,
            live: true,
        });
        reg.fatbins.len() // 1-based handle
    });
    handle as *mut *mut c_void // opaque handle (the 1-based fatbin index)
}

#[no_mangle]
pub extern "C" fn __cudaRegisterFatBinaryEnd(fatCubinHandle: *mut *mut c_void) {
    let _ = fatCubinHandle; // finalize marker
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
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
    let name = unsafe { cstr(deviceName) }.unwrap_or_default();
    state::with_registry(|reg| {
        reg.funcs.push(state::FuncReg {
            host_fun: hostFun as usize,
            fat: fatCubinHandle as usize, // the 1-based fatbin index handle
            name,
        });
    });
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
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
    // dd's PTX model parses only kernel entries, not .global variables -> nothing to bind.
    let _ = (fatCubinHandle, hostVar, deviceAddress, deviceName, ext, size, constant, global);
}

#[no_mangle]
pub extern "C" fn __cudaUnregisterFatBinary(fatCubinHandle: *mut *mut c_void) {
    let handle = fatCubinHandle as usize;
    state::with_registry(|reg| {
        if handle == 0 || handle > reg.fatbins.len() {
            return;
        }
        reg.fatbins[handle - 1].live = false;
        for fr in reg.funcs.iter_mut() {
            if fr.fat == handle {
                fr.fat = 0;
            }
        }
    });
    // The module stays resident in the CudaContext for the process lifetime (unload is a valid no-op).
}

#[no_mangle]
pub extern "C" fn __cudaPushCallConfiguration(
    gridDim: Dim3,
    blockDim: Dim3,
    sharedMem: usize,
    stream: *mut c_void,
) -> u32 {
    let ok = state::push_call_config(CallCfg {
        grid: [gridDim.x, gridDim.y, gridDim.z],
        block: [blockDim.x, blockDim.y, blockDim.z],
        shmem: sharedMem,
        stream,
    });
    if ok {
        0
    } else {
        1 // nonzero -> the host stub skips the launch (stack overflow)
    }
}

#[no_mangle]
pub extern "C" fn __cudaPopCallConfiguration(
    gridDim: *mut Dim3,
    blockDim: *mut Dim3,
    sharedMem: *mut usize,
    stream: *mut c_void,
) -> i32 {
    let Some(c) = state::pop_call_config() else {
        return CUDA_ERROR_INVALID_CONFIGURATION;
    };
    unsafe {
        if !gridDim.is_null() {
            *gridDim = Dim3 { x: c.grid[0], y: c.grid[1], z: c.grid[2] };
        }
        if !blockDim.is_null() {
            *blockDim = Dim3 { x: c.block[0], y: c.block[1], z: c.block[2] };
        }
        if !sharedMem.is_null() {
            *sharedMem = c.shmem;
        }
        if !stream.is_null() {
            *(stream as *mut *mut c_void) = c.stream; // `stream` points at a cudaStream_t slot
        }
    }
    CUDA_SUCCESS_RT
}
