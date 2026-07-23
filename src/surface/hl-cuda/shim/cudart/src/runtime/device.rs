//! Device selection, properties, versions, and runtime error state.

use core::ffi::{c_char, c_void};

use hl_cuda::result::{CUDART_ERROR_INVALID_DEVICE, CUDART_ERROR_INVALID_VALUE, CUDART_SUCCESS};

use crate::state::ShimState;

/// CUDA 12.2, reported by the driver and runtime version queries.
const CUDART_VERSION: i32 = 12020;
#[no_mangle]
pub extern "C" fn cudaGetDeviceCount(count: *mut i32) -> i32 {
    if count.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    unsafe { *count = 1 };
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaGetDevice(device: *mut i32) -> i32 {
    if device.is_null() {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    ShimState::with(|s| unsafe { *device = s.device });
    CUDART_SUCCESS
}

#[no_mangle]
pub extern "C" fn cudaSetDevice(device: i32) -> i32 {
    if device != 0 {
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_DEVICE));
    }
    ShimState::with(|s| s.device = 0);
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
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_DEVICE));
    }
    let p = unsafe { &mut *(prop as *mut CudaDeviceProp) };
    unsafe {
        core::ptr::write_bytes(
            p as *mut CudaDeviceProp as *mut u8,
            0,
            core::mem::size_of::<CudaDeviceProp>(),
        )
    };
    ShimState::with(|s| {
        let d = &s.ctx.device;
        let nb = d.name.as_bytes();
        let n = nb.len().min(255);
        unsafe { core::ptr::copy_nonoverlapping(nb.as_ptr(), p.name.as_mut_ptr() as *mut u8, n) };
        unsafe {
            core::ptr::copy_nonoverlapping(d.uuid.as_ptr(), p.uuid.as_mut_ptr() as *mut u8, 16)
        };
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
        return ShimState::with(|s| s.fail(CUDART_ERROR_INVALID_VALUE));
    }
    ShimState::with(|s| {
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

struct CudaError;

impl CudaError {
    fn text(code: i32, name: bool) -> &'static [u8] {
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
}

#[no_mangle]
pub extern "C" fn cudaGetErrorString(error: i32) -> *const c_char {
    CudaError::text(error, false).as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn cudaGetErrorName(error: i32) -> *const c_char {
    CudaError::text(error, true).as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn cudaGetLastError() -> i32 {
    ShimState::with(|s| std::mem::replace(&mut s.last_error, CUDART_SUCCESS))
}

#[no_mangle]
pub extern "C" fn cudaPeekAtLastError() -> i32 {
    ShimState::with(|s| s.last_error)
}

#[no_mangle]
pub extern "C" fn cudaDeviceReset() -> i32 {
    ShimState::with(|s| {
        s.last_error = CUDART_SUCCESS;
        s.device = 0;
    });
    CUDART_SUCCESS
}
