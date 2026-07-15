//! The hand-written `cuda*` runtime entry points: marshal the CUDA Runtime API C ABI into the `hl_cuda`
//! lowering services through the process-global [`crate::state`] sink.
//!
//! Covers the memory + device + stream basics that map cleanly onto the services (alloc/free/memcpy/
//! memset/synchronize) plus the version/error/device queries a probe expects, AND the runtime's launch
//! path: `__cudaRegisterFatBinary`/`__cudaRegisterFunction`/`__cudaRegisterFatBinaryEnd` populate the
//! [`hl_cuda::service::register::Registry`] (fatbin handle → module, host-fn pointer → kernel) and
//! `cudaLaunchKernel` resolves a host-fn pointer to its device entry and lowers exactly like the
//! driver-API `cuLaunchKernel` (same `CreateShader{kernel}` + `ComputePipeline` + `BindGroup` + `Dispatch`).

use core::ffi::{c_char, c_void};

use hl_cuda::model::device::DevicePtr;
use hl_cuda::result::{
    cudart_from_gpu_error, CUDART_ERROR_INVALID_DEVICE, CUDART_ERROR_INVALID_RESOURCE_HANDLE,
    CUDART_ERROR_INVALID_VALUE, CUDART_ERROR_MEMORY_ALLOCATION, CUDART_ERROR_NOT_SUPPORTED,
    CUDART_SUCCESS,
};
use hl_cuda::service::register::{self, FatbinHandle};
use hl_cuda::service::{allocate, synchronize, transfer};

use crate::state::{with, CallCfg};
use crate::Dim3;

/// `cudaErrorNotReady` (600) — an async query whose work has not completed. The synchronous executor
/// only ever reports it for an event that was never recorded. Declared locally (the shared
/// `hl_cuda::result` runtime subset does not need it) to keep the shared crate untouched.
const CUDART_ERROR_NOT_READY: i32 = 600;

// nvcc's `__fatBinC_Wrapper_t`: `{ int magic; int version; const void* data; void* filename_or_fatbins; }`.
// `__cudaRegisterFatBinary` receives a pointer to this; `data` points at the fatbin CONTAINER (which
// begins with the 0xba55ed50 fatbin magic). Clean-room from the documented layout.
const FATBIN_WRAPPER_MAGIC: u32 = 0x4662_43b1;
const FATBIN_MAGIC: u32 = 0xba55_ed50;

/// CUDA 12.2 — reported by both `cudaDriverGetVersion` and `cudaRuntimeGetVersion`.
const CUDART_VERSION: i32 = 12020;

// cudaMemcpyKind values (stable ABI).
const MEMCPY_HOST_TO_HOST: i32 = 0;
const MEMCPY_HOST_TO_DEVICE: i32 = 1;
const MEMCPY_DEVICE_TO_HOST: i32 = 2;
const MEMCPY_DEVICE_TO_DEVICE: i32 = 3;

unsafe fn bytes<'a>(p: *const c_void, n: usize) -> &'a [u8] {
    if p.is_null() || n == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(p as *const u8, n)
    }
}

// ==================================================================================================
// memory
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaMalloc(dev_ptr: *mut *mut c_void, size: usize) -> i32 {
    if dev_ptr.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| match allocate::mem_alloc(&mut s.ctx, &mut s.sink, size as u64) {
        Ok(p) => {
            unsafe { *dev_ptr = p.0 as *mut c_void };
            CUDART_SUCCESS
        }
        Err(e) => s.fail(cudart_from_gpu_error(&e)),
    })
}

#[no_mangle]
pub extern "C" fn cudaFree(dev_ptr: *mut c_void) -> i32 {
    if dev_ptr.is_null() {
        return CUDART_SUCCESS; // cudaFree(NULL) is a valid no-op.
    }
    with(|s| match allocate::mem_free(&mut s.ctx, &mut s.sink, DevicePtr(dev_ptr as u64)) {
        Ok(()) => CUDART_SUCCESS,
        Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
    })
}

#[no_mangle]
pub extern "C" fn cudaMemcpy(dst: *mut c_void, src: *const c_void, count: usize, kind: i32) -> i32 {
    with(|s| memcpy_impl(s, dst, src, count, kind))
}

#[no_mangle]
pub extern "C" fn cudaMemcpyAsync(
    dst: *mut c_void,
    src: *const c_void,
    count: usize,
    kind: i32,
    _stream: *mut c_void,
) -> i32 {
    // Synchronous executor: async copy is the same lowering (ordering is trivially satisfied).
    with(|s| memcpy_impl(s, dst, src, count, kind))
}

fn memcpy_impl(s: &mut crate::state::State, dst: *mut c_void, src: *const c_void, count: usize, kind: i32) -> i32 {
    match kind {
        MEMCPY_HOST_TO_DEVICE => {
            let host = unsafe { bytes(src, count) };
            match transfer::memcpy_htod(&mut s.ctx, &mut s.sink, DevicePtr(dst as u64), host) {
                Ok(()) => CUDART_SUCCESS,
                Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
            }
        }
        MEMCPY_DEVICE_TO_DEVICE => {
            match transfer::memcpy_dtod(&mut s.ctx, &mut s.sink, DevicePtr(dst as u64), DevicePtr(src as u64), count as u64) {
                Ok(()) => CUDART_SUCCESS,
                Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
            }
        }
        MEMCPY_DEVICE_TO_HOST => {
            // Read the device source back through the sink's device→host readback path and copy it into
            // the caller's host buffer.
            match transfer::read_dtoh(&s.ctx, &mut s.sink, DevicePtr(src as u64), count) {
                Ok(bytes) => {
                    if !dst.is_null() {
                        unsafe {
                            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len())
                        };
                    }
                    CUDART_SUCCESS
                }
                Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
            }
        }
        MEMCPY_HOST_TO_HOST => {
            if !dst.is_null() && !src.is_null() && count > 0 {
                unsafe { std::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, count) };
            }
            CUDART_SUCCESS
        }
        // cudaMemcpyDefault (UVA-inferred) is not modeled; report it truthfully rather than mis-copying.
        _ => s.fail(CUDART_ERROR_NOT_SUPPORTED),
    }
}

#[no_mangle]
pub extern "C" fn cudaMemset(dev_ptr: *mut c_void, value: i32, count: usize) -> i32 {
    with(|s| memset_impl(s, dev_ptr, value, count))
}

#[no_mangle]
pub extern "C" fn cudaMemsetAsync(dev_ptr: *mut c_void, value: i32, count: usize, _stream: *mut c_void) -> i32 {
    with(|s| memset_impl(s, dev_ptr, value, count))
}

fn memset_impl(s: &mut crate::state::State, dev_ptr: *mut c_void, value: i32, count: usize) -> i32 {
    let fill = vec![value as u8; count];
    match transfer::memcpy_htod(&mut s.ctx, &mut s.sink, DevicePtr(dev_ptr as u64), &fill) {
        Ok(()) => CUDART_SUCCESS,
        Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
    }
}

#[no_mangle]
pub extern "C" fn cudaMemGetInfo(free_b: *mut usize, total_b: *mut usize) -> i32 {
    if free_b.is_null() || total_b.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| {
        // free = advertised VRAM minus the sum of every live device allocation (never underflows).
        let (free, total) = s.mem_info();
        unsafe {
            *total_b = total;
            *free_b = free;
        }
    });
    CUDART_SUCCESS
}

// ==================================================================================================
// host (pinned / mapped) memory + managed (unified) memory
// ==================================================================================================

/// `cudaMallocManaged(devPtr, size, flags)` — a managed (unified) device allocation: the same
/// `CreateBuffer` IR as `cudaMalloc`, flagged managed in the model. The (attach-global/host) flags do not
/// change the modeled semantics.
#[no_mangle]
pub extern "C" fn cudaMallocManaged(dev_ptr: *mut *mut c_void, size: usize, _flags: u32) -> i32 {
    if dev_ptr.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| match allocate::mem_alloc_managed(&mut s.ctx, &mut s.sink, size as u64) {
        Ok(p) => {
            unsafe { *dev_ptr = p.0 as *mut c_void };
            CUDART_SUCCESS
        }
        Err(e) => s.fail(cudart_from_gpu_error(&e)),
    })
}

/// `cudaMallocHost(ptr, size)` — a page-locked host allocation. Hands back the base of a real host buffer
/// the model owns (usable directly as a `cudaMemcpy` host source/destination).
#[no_mangle]
pub extern "C" fn cudaMallocHost(ptr: *mut *mut c_void, size: usize) -> i32 {
    if ptr.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    match with(|s| allocate::host_alloc(&mut s.ctx, size)) {
        Some(base) => {
            unsafe { *ptr = base as *mut c_void };
            CUDART_SUCCESS
        }
        None => with(|s| s.fail(CUDART_ERROR_MEMORY_ALLOCATION)),
    }
}

/// `cudaHostAlloc(ptr, size, flags)` — the flagged pinned-allocation form; the modeled semantics do not
/// depend on the (portable / mapped / write-combined) flags, so it shares `cudaMallocHost`'s body.
#[no_mangle]
pub extern "C" fn cudaHostAlloc(ptr: *mut *mut c_void, size: usize, _flags: u32) -> i32 {
    cudaMallocHost(ptr, size)
}

/// `cudaFreeHost(ptr)` — free a pinned allocation. `cudaFreeHost(NULL)` is a valid no-op; a bogus /
/// already-freed pointer is `cudaErrorInvalidValue`.
#[no_mangle]
pub extern "C" fn cudaFreeHost(ptr: *mut c_void) -> i32 {
    if ptr.is_null() {
        return CUDART_SUCCESS;
    }
    with(|s| match allocate::host_free(&mut s.ctx, ptr as u64) {
        Ok(()) => CUDART_SUCCESS,
        Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
    })
}

/// `cudaHostGetDevicePointer(pDevice, pHost, flags)` — the device pointer that maps a host allocation
/// `pHost` (lazily creating its backing device buffer). A pointer that is not a live host allocation is
/// `cudaErrorInvalidValue`.
#[no_mangle]
pub extern "C" fn cudaHostGetDevicePointer(
    p_device: *mut *mut c_void,
    p_host: *mut c_void,
    _flags: u32,
) -> i32 {
    if p_device.is_null() || p_host.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| match allocate::host_get_device_pointer(&mut s.ctx, &mut s.sink, p_host as u64) {
        Ok(ptr) => {
            unsafe { *p_device = ptr.0 as *mut c_void };
            CUDART_SUCCESS
        }
        Err(_) => s.fail(CUDART_ERROR_INVALID_VALUE),
    })
}

// ==================================================================================================
// synchronization
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaDeviceSynchronize() -> i32 {
    with(|s| match synchronize::ctx_synchronize(&mut s.ctx, &mut s.sink) {
        Ok(()) => CUDART_SUCCESS,
        Err(e) => s.fail(cudart_from_gpu_error(&e)),
    })
}

#[no_mangle]
pub extern "C" fn cudaThreadSynchronize() -> i32 {
    cudaDeviceSynchronize()
}

#[no_mangle]
pub extern "C" fn cudaStreamCreate(p_stream: *mut *mut c_void) -> i32 {
    if p_stream.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    let h = with(|s| {
        let st = s.ctx.streams.create();
        s.intern_stream(st)
    });
    unsafe { *p_stream = h };
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaStreamCreateWithFlags(p_stream: *mut *mut c_void, _flags: u32) -> i32 {
    cudaStreamCreate(p_stream)
}

#[no_mangle]
pub extern "C" fn cudaStreamDestroy(stream: *mut c_void) -> i32 {
    with(|s| match s.stream(stream) {
        Some(st) => {
            s.ctx.streams.destroy(st);
            CUDART_SUCCESS
        }
        None => s.fail(CUDART_ERROR_INVALID_VALUE),
    })
}

#[no_mangle]
pub extern "C" fn cudaStreamSynchronize(stream: *mut c_void) -> i32 {
    with(|s| {
        let Some(st) = s.stream(stream) else {
            return s.fail(CUDART_ERROR_INVALID_VALUE);
        };
        match synchronize::stream_synchronize(&mut s.ctx, &mut s.sink, st) {
            Ok(()) => CUDART_SUCCESS,
            Err(e) => s.fail(cudart_from_gpu_error(&e)),
        }
    })
}

/// `cudaStreamQuery(stream)` — is the stream idle? The synchronous executor completes every submit before
/// the call returns, so a valid stream is always ready (`cudaSuccess`). An unknown handle is
/// `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaStreamQuery(stream: *mut c_void) -> i32 {
    with(|s| match s.stream(stream) {
        Some(_) => CUDART_SUCCESS,
        None => s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE),
    })
}

/// `cudaStreamWaitEvent(stream, event, flags)` — make `stream` wait on `event`. With the synchronous
/// executor the awaited work has already completed, so this validates both handles and records nothing.
/// An unknown stream or event handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaStreamWaitEvent(stream: *mut c_void, event: *mut c_void, _flags: u32) -> i32 {
    with(|s| {
        if s.stream(stream).is_none() || !s.event_is_valid(event) {
            return s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE);
        }
        CUDART_SUCCESS
    })
}

// ==================================================================================================
// events — record/query/elapsed via the monotonic clock (the synchronous-executor timing surface)
// ==================================================================================================

/// `cudaEventCreate(event)` — mint an (unrecorded) event handle.
#[no_mangle]
pub extern "C" fn cudaEventCreate(event: *mut *mut c_void) -> i32 {
    if event.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    let h = with(|s| s.create_event());
    unsafe { *event = h };
    CUDART_SUCCESS
}

/// `cudaEventCreateWithFlags(event, flags)` — the flagged form. The (blocking-sync / disable-timing /
/// interprocess) flags do not change the modeled monotonic-clock timing, so it shares the body.
#[no_mangle]
pub extern "C" fn cudaEventCreateWithFlags(event: *mut *mut c_void, _flags: u32) -> i32 {
    cudaEventCreate(event)
}

/// `cudaEventRecord(event, stream)` — timestamp the event. Every prior submit has already completed under
/// the synchronous executor, so the record time is the correct completion instant for
/// `cudaEventElapsedTime`. A bad event handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaEventRecord(event: *mut c_void, _stream: *mut c_void) -> i32 {
    with(|s| {
        if s.record_event(event) {
            CUDART_SUCCESS
        } else {
            s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE)
        }
    })
}

/// `cudaEventSynchronize(event)` — block until the event completes. Recorded work is already complete
/// (synchronous executor), so a valid, recorded event returns immediately; an unrecorded one is
/// `cudaErrorNotReady`; a bad handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaEventSynchronize(event: *mut c_void) -> i32 {
    with(|s| {
        if !s.event_is_valid(event) {
            return s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE);
        }
        if s.event_recorded(event) {
            CUDART_SUCCESS
        } else {
            s.fail(CUDART_ERROR_NOT_READY)
        }
    })
}

/// `cudaEventQuery(event)` — has the event completed? A recorded event is complete (`cudaSuccess`); a
/// valid-but-unrecorded one is `cudaErrorNotReady`; a bad handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaEventQuery(event: *mut c_void) -> i32 {
    with(|s| {
        if !s.event_is_valid(event) {
            return s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE);
        }
        if s.event_recorded(event) {
            CUDART_SUCCESS
        } else {
            s.fail(CUDART_ERROR_NOT_READY)
        }
    })
}

/// `cudaEventElapsedTime(ms, start, end)` — milliseconds between two recorded events, from the monotonic
/// clock. A null out-pointer is `cudaErrorInvalidValue`; either event invalid/unrecorded is
/// `cudaErrorNotReady`.
#[no_mangle]
pub extern "C" fn cudaEventElapsedTime(ms: *mut f32, start: *mut c_void, end: *mut c_void) -> i32 {
    if ms.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| match s.event_elapsed_ms(start, end) {
        Some(v) => {
            unsafe { *ms = v };
            CUDART_SUCCESS
        }
        None => s.fail(CUDART_ERROR_NOT_READY),
    })
}

/// `cudaEventDestroy(event)` — retire an event handle. A bad handle is `cudaErrorInvalidResourceHandle`.
#[no_mangle]
pub extern "C" fn cudaEventDestroy(event: *mut c_void) -> i32 {
    with(|s| {
        if s.destroy_event(event) {
            CUDART_SUCCESS
        } else {
            s.fail(CUDART_ERROR_INVALID_RESOURCE_HANDLE)
        }
    })
}

// ==================================================================================================
// device + version + error queries
// ==================================================================================================

#[no_mangle]
pub extern "C" fn cudaGetDeviceCount(count: *mut i32) -> i32 {
    if count.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    unsafe { *count = 1 };
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaGetDevice(device: *mut i32) -> i32 {
    if device.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| unsafe { *device = s.device });
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaSetDevice(device: i32) -> i32 {
    if device != 0 {
        return with(|s| s.fail(CUDART_ERROR_INVALID_DEVICE));
    }
    with(|s| s.device = 0);
    CUDART_SUCCESS
}

/// `cudaDeviceProp` — a faithful clean-room reconstruction of the CUDA 12.x layout (`driver_types.h`). The
/// exact `#[repr(C)]` field order/type/size is the load-bearing ABI contract; fields the model does not
/// populate stay zeroed, which is the spec-faithful "feature absent" answer.
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

/// `cudaGetDeviceProperties_v2(prop, device)` — fill the `cudaDeviceProp` from the device descriptor +
/// the modeled Ampere-class attributes (the same values `cudaDeviceGetAttribute`/`cuDeviceGetAttribute`
/// answer). A null `prop` or non-zero `device` is `cudaErrorInvalidDevice`.
#[no_mangle]
pub extern "C" fn cudaGetDeviceProperties_v2(prop: *mut c_void, device: i32) -> i32 {
    if prop.is_null() || device != 0 {
        return with(|s| s.fail(CUDART_ERROR_INVALID_DEVICE));
    }
    let p = unsafe { &mut *(prop as *mut CudaDeviceProp) };
    unsafe {
        core::ptr::write_bytes(p as *mut CudaDeviceProp as *mut u8, 0, core::mem::size_of::<CudaDeviceProp>())
    };
    with(|s| {
        let d = &s.ctx.device;
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
    });
    // Fixed modeled attributes of the simulated unified-memory device (mirror the attribute switch).
    p.max_threads_per_block = 1024;
    p.max_threads_dim = [1024, 1024, 64];
    p.max_grid_size = [2147483647, 65535, 65535];
    p.regs_per_block = 65536;
    p.regs_per_multiprocessor = 65536;
    p.shared_mem_per_block = 49152;
    p.shared_mem_per_block_optin = 101376;
    p.total_const_mem = 65536;
    p.shared_mem_per_multiprocessor = 102400;
    p.mem_pitch = 2147483647;
    p.integrated = 1; // unified memory on the host
    p.can_map_host_memory = 1;
    p.device_overlap = 1;
    p.compute_mode = 0; // DEFAULT
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
    p.compute_preemption_supported = 1;
    p.pageable_memory_access = 1;
    p.direct_managed_mem_access_from_host = 1;
    p.texture_alignment = 512;
    p.texture_pitch_alignment = 32;
    p.max_texture_1d = 131072;
    p.cooperative_launch = 1;
    CUDART_SUCCESS
}

/// `cudaGetDeviceProperties(prop, device)` — the legacy alias; identical fill to the `_v2` form.
#[no_mangle]
pub extern "C" fn cudaGetDeviceProperties(prop: *mut c_void, device: i32) -> i32 {
    cudaGetDeviceProperties_v2(prop, device)
}

/// `cudaDeviceGetPCIBusId(pciBusId, len, device)` — write the device's PCI bus id string. A null buffer,
/// non-positive `len`, or non-zero `device` is `cudaErrorInvalidValue`.
#[no_mangle]
pub extern "C" fn cudaDeviceGetPCIBusId(pci_bus_id: *mut c_char, len: i32, device: i32) -> i32 {
    if pci_bus_id.is_null() || len <= 0 || device != 0 {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    with(|s| {
        let id = s.ctx.device.pci_bus_id.as_bytes();
        let cap = (len as usize) - 1;
        let n = id.len().min(cap);
        unsafe {
            core::ptr::copy_nonoverlapping(id.as_ptr(), pci_bus_id as *mut u8, n);
            *pci_bus_id.add(n) = 0;
        }
    });
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaDriverGetVersion(version: *mut i32) -> i32 {
    if version.is_null() {
        return CUDART_ERROR_INVALID_VALUE;
    }
    unsafe { *version = CUDART_VERSION };
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaRuntimeGetVersion(version: *mut i32) -> i32 {
    if version.is_null() {
        return CUDART_ERROR_INVALID_VALUE;
    }
    unsafe { *version = CUDART_VERSION };
    CUDART_SUCCESS
}

fn error_text(code: i32, name: bool) -> &'static [u8] {
    match (code, name) {
        (0, false) => b"no error\0",
        (0, true) => b"cudaSuccess\0",
        (1, false) => b"invalid argument\0",
        (1, true) => b"cudaErrorInvalidValue\0",
        (2, false) => b"out of memory\0",
        (2, true) => b"cudaErrorMemoryAllocation\0",
        (3, false) => b"initialization error\0",
        (3, true) => b"cudaErrorInitializationError\0",
        (101, false) => b"invalid device ordinal\0",
        (101, true) => b"cudaErrorInvalidDevice\0",
        (200, false) => b"device kernel image is invalid\0",
        (200, true) => b"cudaErrorInvalidKernelImage\0",
        (218, false) => b"a PTX JIT compilation failed\0",
        (218, true) => b"cudaErrorInvalidPtx\0",
        (400, false) => b"invalid resource handle\0",
        (400, true) => b"cudaErrorInvalidResourceHandle\0",
        (500, false) => b"named symbol not found\0",
        (500, true) => b"cudaErrorSymbolNotFound\0",
        (600, false) => b"device not ready\0",
        (600, true) => b"cudaErrorNotReady\0",
        (801, false) => b"operation not supported\0",
        (801, true) => b"cudaErrorNotSupported\0",
        (_, true) => b"cudaErrorUnknown\0",
        (_, false) => b"unknown error\0",
    }
}

#[no_mangle]
pub extern "C" fn cudaGetErrorString(error: i32) -> *const c_char {
    error_text(error, false).as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn cudaGetErrorName(error: i32) -> *const c_char {
    error_text(error, true).as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn cudaGetLastError() -> i32 {
    with(|s| std::mem::replace(&mut s.last_error, CUDART_SUCCESS))
}

#[no_mangle]
pub extern "C" fn cudaPeekAtLastError() -> i32 {
    with(|s| s.last_error)
}

#[no_mangle]
pub extern "C" fn cudaDeviceReset() -> i32 {
    with(|s| {
        s.last_error = CUDART_SUCCESS;
        s.device = 0;
    });
    CUDART_SUCCESS
}

// ==================================================================================================
// runtime-API launch path: `__cudaRegister*` fatbin/function registry + `cudaLaunchKernel`
// ==================================================================================================

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

/// Read a nul-terminated C string into an owned `String` (`None` if null or not UTF-8).
unsafe fn cstr_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    std::ffi::CStr::from_ptr(p).to_str().ok().map(str::to_string)
}

/// Encode a [`FatbinHandle`] as the opaque `void**` nvcc round-trips back to us: a heap cell whose stored
/// `void*` value is the handle. [`decode_handle`] reads it back.
fn encode_handle(h: FatbinHandle) -> *mut *mut c_void {
    Box::into_raw(Box::new(h.0 as *mut c_void))
}
unsafe fn decode_handle(h: *mut *mut c_void) -> Option<FatbinHandle> {
    if h.is_null() {
        return None;
    }
    Some(FatbinHandle(*h as u64))
}

/// `__cudaRegisterFatBinary(fatCubin)` — walk the wrapped fatbin to its PTX, load it as a module, and hand
/// nvcc an opaque handle bound to that module. Returns null on a bad image (nvcc tolerates a null handle).
#[no_mangle]
pub extern "C" fn __cudaRegisterFatBinary(fatCubin: *mut c_void) -> *mut *mut c_void {
    let Some(container) = (unsafe { container_bytes(fatCubin) }) else {
        return core::ptr::null_mut();
    };
    with(|s| match s.registry.register_fatbinary(&mut s.ctx, container) {
        Ok(handle) => encode_handle(handle),
        Err(e) => {
            s.fail(cudart_from_gpu_error(&e));
            core::ptr::null_mut()
        }
    })
}

/// `__cudaRegisterFunction(handle, hostFun, deviceFun, deviceName, …)` — bind the host function pointer
/// `hostFun` to the device entry `deviceName` in the handle's module. `deviceFun` + the launch-bound
/// descriptors are nvcc bookkeeping the lowering does not need.
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
    let Some(handle) = (unsafe { decode_handle(fatCubinHandle) }) else {
        return;
    };
    let Some(name) = (unsafe { cstr_string(deviceName) }) else {
        return;
    };
    let host_fn = hostFun as usize;
    with(|s| {
        if let Err(e) = s.registry.register_function(&s.ctx, handle, host_fn, &name) {
            s.fail(cudart_from_gpu_error(&e));
        }
    });
}

/// `__cudaRegisterFatBinaryEnd(handle)` — the finalization marker after the last `__cudaRegisterFunction`.
#[no_mangle]
pub extern "C" fn __cudaRegisterFatBinaryEnd(fatCubinHandle: *mut *mut c_void) {
    if let Some(handle) = unsafe { decode_handle(fatCubinHandle) } {
        with(|s| {
            s.registry.register_fatbinary_end(handle);
        });
    }
}

/// `cudaLaunchKernel(func, gridDim, blockDim, args, sharedMem, stream)` — resolve the host-fn pointer to
/// its registered device entry and lower exactly like the driver-API `cuLaunchKernel`, via the shared
/// [`register::launch_kernel`].
#[no_mangle]
pub extern "C" fn cudaLaunchKernel(
    func: *const c_void,
    gridDim: Dim3,
    blockDim: Dim3,
    args: *mut *mut c_void,
    _sharedMem: usize,
    _stream: *mut c_void,
) -> i32 {
    let host_fn = func as usize;
    let grid = (gridDim.x, gridDim.y, gridDim.z);
    let block = (blockDim.x, blockDim.y, blockDim.z);
    with(|s| {
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
            Err(e) => s.fail(cudart_from_gpu_error(&e)),
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

/// `cudaFuncGetAttributes(attr, func)` — the launch-relevant attributes of a device function. hl's PTX
/// model does not track per-kernel register/shared pressure, so these are the honest GPU-free modeled
/// defaults (the same values the driver's `cuFuncGetAttribute` answers for the simulated device). `func`
/// is nvcc's host stub pointer; a null `attr` is `cudaErrorInvalidValue`.
#[no_mangle]
pub extern "C" fn cudaFuncGetAttributes(attr: *mut c_void, func: *const c_void) -> i32 {
    let _ = func;
    if attr.is_null() {
        return with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    let cc = with(|s| s.ctx.device.compute_capability);
    let ptx_ver = cc.0 as i32 * 10 + cc.1 as i32;
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
    CUDART_SUCCESS
}

// ==================================================================================================
// nvcc glue tail: `__cudaRegisterVar`, `__cudaUnregisterFatBinary`, `<<<>>>` call-config stack
// ==================================================================================================

/// `__cudaRegisterVar(handle, hostVar, deviceAddress, deviceName, ext, size, constant, global)` — nvcc
/// binds a `__device__`/`__constant__` global. hl's PTX model parses only kernel entries (not `.global`
/// variables), so there is nothing to bind; this is an honest no-op (a later `cudaGetSymbolAddress` on
/// such a symbol — not part of this surface — would report it absent).
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
    let _ = (fatCubinHandle, hostVar, deviceAddress, deviceName, ext, size, constant, global);
}

/// `__cudaUnregisterFatBinary(handle)` — drop the fatbin handle's module binding. The loaded module stays
/// resident in the context (a stale launch may still reference it), so this only forgets the handle.
#[no_mangle]
pub extern "C" fn __cudaUnregisterFatBinary(fatCubinHandle: *mut *mut c_void) {
    if let Some(handle) = unsafe { decode_handle(fatCubinHandle) } {
        with(|s| s.registry.unregister_fatbinary(handle));
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
    if with(|s| s.push_call_config(cfg)) {
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
    let Some(c) = with(|s| s.pop_call_config()) else {
        return CUDART_ERROR_INVALID_CONFIGURATION;
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
            // `stream` points at the caller's `cudaStream_t` slot.
            *(stream as *mut *mut c_void) = c.stream as *mut c_void;
        }
    }
    CUDART_SUCCESS
}
