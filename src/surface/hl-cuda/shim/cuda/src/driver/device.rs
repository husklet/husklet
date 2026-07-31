use super::*;
#[no_mangle]
pub extern "C" fn cuDeviceCanAccessPeer(can_access_peer: *mut i32, a: i32, b: i32) -> i32 {
    let _ = (a, b);
    if can_access_peer.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *can_access_peer = 0 };
    CUDA_SUCCESS
}

/// `cuDeviceGetByPCIBusId(dev, pciBusId)` — resolve a device by its PCI bus-id string. There is one
/// simulated device, so any well-formed request resolves to device 0.
#[no_mangle]
pub extern "C" fn cuDeviceGetByPCIBusId(dev: *mut i32, s: *const c_char) -> i32 {
    if dev.is_null() || s.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    unsafe { *dev = 0 };
    CUDA_SUCCESS
}

/// `cuDeviceGetPCIBusId(dst, len, dev)` — write the device's PCI bus id (`domain:bus:device.function`)
/// into the caller's buffer.
#[no_mangle]
pub extern "C" fn cuDeviceGetPCIBusId(s: *mut c_char, len: i32, dev: i32) -> i32 {
    if s.is_null() || len <= 0 || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with(|st| unsafe { write_cstr(s, len, &st.ctx.device.pci_bus_id) });
    CUDA_SUCCESS
}

/// `cuDeviceGetLuid(luid, deviceNodeMask, dev)` — the device LUID is a Windows/TCC-only identity; the
/// simulated Linux device has none, so this is honestly `CUDA_ERROR_NOT_SUPPORTED` (never a fake LUID).
#[no_mangle]
pub extern "C" fn cuDeviceGetLuid(luid: *mut c_char, mask: *mut u32, dev: i32) -> i32 {
    let _ = (luid, mask);
    if dev != 0 {
        return CUDA_ERROR_INVALID_DEVICE;
    }
    crate::stub::Call::unsupported("cuDeviceGetLuid", "LUID is Windows/TCC-only; not modeled");
    CUDA_ERROR_NOT_SUPPORTED
}

/// The original `CUdevprop` struct `cuDeviceGetProperties` fills (layout matches cuda.h). Superseded by
/// `cuDeviceGetAttribute`, but still queried by old apps.
#[repr(C)]
pub(super) struct CuDevprop {
    pub(super) max_threads_per_block: i32,
    pub(super) max_threads_dim: [i32; 3],
    pub(super) max_grid_size: [i32; 3],
    pub(super) shared_mem_per_block: i32,
    pub(super) total_constant_memory: i32,
    pub(super) simd_width: i32,
    pub(super) mem_pitch: i32,
    pub(super) regs_per_block: i32,
    pub(super) clock_rate: i32,
    pub(super) texture_align: i32,
}

/// `cuDeviceGetProperties(prop, dev)` — the deprecated bulk device-properties query. Every field mirrors
/// the value `cuDeviceGetAttribute` reports for the same property, sourced from the modeled device.
#[no_mangle]
pub extern "C" fn cuDeviceGetProperties(prop: *mut c_void, dev: i32) -> i32 {
    if prop.is_null() || dev != 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    let p = ShimState::with(|s| {
        let d = &s.ctx.device;
        CuDevprop {
            max_threads_per_block: d.max_threads_per_block as i32,
            max_threads_dim: [1024, 1024, 64],
            max_grid_size: [2147483647, 65535, 65535],
            shared_mem_per_block: 49152,
            total_constant_memory: 65536,
            simd_width: d.warp_size as i32,
            mem_pitch: 2147483647,
            regs_per_block: 65536,
            clock_rate: d.clock_khz as i32,
            texture_align: 512,
        }
    });
    unsafe { *(prop as *mut CuDevprop) = p };
    CUDA_SUCCESS
}

// ==================================================================================================
// context: v3 create, id, shared-mem config, peer access, persisting-L2 reset
// ==================================================================================================

/// `cuCtxCreate_v3(pctx, execAffinityParams, numParams, flags, dev)` — context creation with execution
/// affinity. The single simulated device has no partitionable SMs, so the affinity params are ignored
/// and this shares `cuCtxCreate_v2`'s body.
#[no_mangle]
pub extern "C" fn cuCtxCreate_v3(
    pctx: *mut *mut c_void,
    params_array: *mut c_void,
    num_params: i32,
    flags: u32,
    dev: i32,
) -> i32 {
    let _ = (params_array, num_params);
    cuCtxCreate_v2(pctx, flags, dev)
}

/// `cuCtxGetId(ctx, id)` — a unique id for `ctx` (the current context when `ctx` is null). The context
/// token is already a stable per-process id, so it is reported directly.
#[no_mangle]
pub extern "C" fn cuCtxGetId(ctx: *mut c_void, id: *mut u64) -> i32 {
    if id.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with(|s| {
        let token = if ctx.is_null() {
            match s.require_context() {
                Ok(()) => s.current_ctx_token(),
                Err(code) => return code,
            }
        } else if s.ctx_is_live(ctx) {
            ctx as usize
        } else {
            return CUDA_ERROR_INVALID_CONTEXT;
        };
        unsafe { *id = token as u64 };
        CUDA_SUCCESS
    })
}

/// `cuCtxGetSharedMemConfig(config)` — the current context's shared-memory bank width config.
#[no_mangle]
pub extern "C" fn cuCtxGetSharedMemConfig(c: *mut i32) -> i32 {
    if c.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| {
        let v = s.ctx_shared_config();
        unsafe { *c = v };
        CUDA_SUCCESS
    })
}

/// `cuCtxSetSharedMemConfig(config)` — record the context's preferred shared-memory bank config (a hint
/// the synchronous executor honors as a no-op but reports faithfully via the getter).
#[no_mangle]
pub extern "C" fn cuCtxSetSharedMemConfig(c: i32) -> i32 {
    ShimState::with_context(|s| {
        s.set_ctx_shared_config(c);
        CUDA_SUCCESS
    })
}

/// `cuCtxEnablePeerAccess(peerContext, flags)` — the single simulated device has no peers, so enabling
/// peer access is honestly unsupported (never a fake success).
#[no_mangle]
pub extern "C" fn cuCtxEnablePeerAccess(peer: *mut c_void, flags: u32) -> i32 {
    let _ = (peer, flags);
    CUDA_ERROR_PEER_ACCESS_UNSUPPORTED
}

/// `cuCtxDisablePeerAccess(peerContext)` — peer access was never (and can never be) enabled on the
/// single simulated device, so this is honestly `CUDA_ERROR_PEER_ACCESS_NOT_ENABLED`.
#[no_mangle]
pub extern "C" fn cuCtxDisablePeerAccess(peer: *mut c_void) -> i32 {
    let _ = peer;
    CUDA_ERROR_PEER_ACCESS_NOT_ENABLED
}

/// `cuCtxResetPersistingL2Cache()` — the model advertises no persisting-L2 window (the attribute reports
/// 0), so resetting it is a valid no-op.
#[no_mangle]
pub extern "C" fn cuCtxResetPersistingL2Cache() -> i32 {
    CUDA_SUCCESS
}

// ==================================================================================================
// function: owning module, entry name, shared-mem config
// ==================================================================================================

/// `cuFuncGetModule(hmod, hfunc)` — the module a function was resolved from.
#[no_mangle]
pub extern "C" fn cuFuncGetModule(m: *mut *mut c_void, f: *mut c_void) -> i32 {
    if m.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| match s.function(f) {
        Some(func) => {
            // The module handle is `module id` interned as `index + 1` (see `intern_module`); the resolved
            // `Function.module` IS that model id, so the guest handle is the id itself.
            unsafe { *m = func.module as usize as *mut c_void };
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_HANDLE,
    })
}

/// `cuFuncGetName(name, hfunc)` — the entry-point name the function was resolved by. Returns a pointer to
/// the interned name (stable for the process lifetime).
#[no_mangle]
pub extern "C" fn cuFuncGetName(name: *mut *const c_char, f: *mut c_void) -> i32 {
    if name.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with_context(|s| match s.func_name_ptr(f) {
        Some(p) => {
            unsafe { *name = p };
            CUDA_SUCCESS
        }
        None => CUDA_ERROR_INVALID_HANDLE,
    })
}

/// `cuFuncSetSharedMemConfig(hfunc, config)` — record a per-function shared-memory bank config hint (a
/// no-op for the synchronous executor). Validates the handle so a bogus function is rejected honestly.
#[no_mangle]
pub extern "C" fn cuFuncSetSharedMemConfig(f: *mut c_void, config: i32) -> i32 {
    let _ = config;
    ShimState::with_context(|s| {
        if s.function(f).is_some() {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

// ==================================================================================================
// entry-point dispatch: cuGetProcAddress (+_v2) — resolve a cu* symbol to its function pointer
// ==================================================================================================
