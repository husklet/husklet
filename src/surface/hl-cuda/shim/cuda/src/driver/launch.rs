use super::*;
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
    _shared_mem_bytes: u32,
    _stream: *mut c_void,
    kernel_params: *mut *mut c_void,
    _extra: *mut *mut c_void,
) -> i32 {
    launch_kernel_impl(
        f,
        (gx, gy, gz),
        (bx, by, bz),
        kernel_params,
        "cuLaunchKernel",
    )
}

/// Shared launch lowering for `cuLaunchKernel` / `cuLaunchKernelEx`: recover the kernel's parameter
/// layout by compiling its PTX with the launch block dims, marshal each `kernelParams[i]` slot per that
/// layout, and submit the same compute IR through the sink. Both entry points funnel here so the lowered
/// command stream is identical.
///
/// # Safety
/// `kernel_params`, when non-null, must point at `prog.params.len()` valid `void*` slots, each pointing at
/// a value of the parameter's natural width (the `cuLaunchKernel` ABI).
fn launch_kernel_impl(
    f: *mut c_void,
    grid: (u32, u32, u32),
    block: (u32, u32, u32),
    kernel_params: *mut *mut c_void,
    who: &'static str,
) -> i32 {
    ShimState::with(|s| {
        let Some(func) = s.function(f) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        let block_arr = [block.0, block.1, block.2];
        // Recover the kernel's parameter layout (which args are pointers vs scalars, and each width) by
        // compiling the module's PTX with the launch block dims — the same front-end the executor uses.
        let Some((src, entry)) = s.ctx.entry_source(func) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        let prog = match ptx::compile(&src, &entry, block_arr) {
            Ok(p) => p,
            Err(e) => {
                crate::stub::Call::unsupported(who, &format!("entry `{entry}`: {e:?}"));
                return DriverStatus::from(&e).code();
            }
        };
        if kernel_params.is_null() && !prog.params.is_empty() {
            // The `extra`-packed parameter form is not modeled; a kernel with params needs kernelParams.
            return CUDA_ERROR_NOT_SUPPORTED;
        }
        // Marshal each argument from its `kernelParams[i]` slot per the recovered layout.
        let mut args: Vec<KernelArg> = Vec::with_capacity(prog.params.len());
        for (i, p) in prog.params.iter().enumerate() {
            let slot = unsafe { *kernel_params.add(i) };
            if slot.is_null() {
                return CUDA_ERROR_INVALID_VALUE;
            }
            if p.is_ptr {
                let v = unsafe { std::ptr::read_unaligned(slot as *const u64) };
                args.push(KernelArg::Ptr(DevicePtr(v)));
            } else {
                let w = p.width as usize;
                let raw = unsafe { std::slice::from_raw_parts(slot as *const u8, w) };
                args.push(KernelArg::Scalar(raw.to_vec()));
            }
        }
        match launch_service::launch(&mut s.ctx, &mut s.sink, func, grid, block, &args) {
            Ok(_) => CUDA_SUCCESS,
            Err(e) => DriverStatus::from(&e).code(),
        }
    })
}

/// The `CUlaunchConfig` prefix `cuLaunchKernelEx` reads (cuda.h layout): `gridDimX/Y/Z`, `blockDimX/Y/Z`,
/// `sharedMemBytes` are the first seven `u32`s (the trailing `hStream` + attribute list are not modeled).
/// Returns `(grid, block, shared_mem_bytes)`; `None` if `cfg` is null.
///
/// # Safety
/// `cfg`, when non-null, must point at a `CUlaunchConfig` whose first seven `u32` fields are initialized.
struct LaunchConfig;

impl LaunchConfig {
    unsafe fn parse(cfg: *const c_void) -> Option<((u32, u32, u32), (u32, u32, u32), u32)> {
        if cfg.is_null() {
            return None;
        }
        let d = cfg as *const u32;
        Some((
            (*d.add(0), *d.add(1), *d.add(2)),
            (*d.add(3), *d.add(4), *d.add(5)),
            *d.add(6),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_config_parses_kernel_dimensions() {
        let cfg = [2_u32, 3, 4, 8, 16, 1, 96];
        let parsed = unsafe { LaunchConfig::parse(cfg.as_ptr() as *const c_void) };
        assert_eq!(parsed, Some(((2, 3, 4), (8, 16, 1), 96)));
        assert_eq!(unsafe { LaunchConfig::parse(core::ptr::null()) }, None);
        assert_eq!(
            cuLaunchKernelEx(
                core::ptr::null(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
            ),
            CUDA_ERROR_INVALID_VALUE
        );
    }
}

/// `cuLaunchKernelEx(config, f, kernelParams, extra)` — the config-struct launch form. Parses the
/// `CUlaunchConfig` grid/block dims and lowers through the exact same path as `cuLaunchKernel`.
#[no_mangle]
pub extern "C" fn cuLaunchKernelEx(
    cfg: *const c_void,
    f: *mut c_void,
    kernel_params: *mut *mut c_void,
    _extra: *mut *mut c_void,
) -> i32 {
    let Some((grid, block, _smem)) = (unsafe { LaunchConfig::parse(cfg) }) else {
        return CUDA_ERROR_INVALID_VALUE;
    };
    launch_kernel_impl(f, grid, block, kernel_params, "cuLaunchKernelEx")
}

// ==================================================================================================
// function attributes + occupancy (computed from the modeled function + device limits)
// ==================================================================================================

// The modeled Ampere-class SM limits (the same fixed values `cuDeviceGetAttribute` reports), used by the
// occupancy math. Kept here as named constants so the two entry points can't drift from the device table.
const MAX_THREADS_PER_SM: i32 = 2048;
const MAX_REGS_PER_SM: i32 = 65536;
pub(super) const MAX_SHARED_PER_SM: i32 = 102_400;
const MAX_BLOCKS_PER_SM: i32 = 32;

/// Compile the function's PTX to recover its real per-thread resource use `(num_regs, static_shared)`.
/// `None` if the handle is bad; falls back to `Some((0, 0))` if the kernel is outside the modeled subset
/// (so an attribute/occupancy query still answers rather than fabricating a value it cannot derive).
pub(super) struct FunctionResources;

impl FunctionResources {
    pub(super) fn get(s: &crate::state::State, f: *mut c_void) -> Option<(u32, u32)> {
        let func = s.function(f)?;
        let Some((src, entry)) = s.ctx.entry_source(func) else {
            return Some((0, 0));
        };
        match ptx::compile(&src, &entry, [1, 1, 1]) {
            Ok(p) => Some((p.reg_count as u32, p.shared_bytes)),
            Err(_) => Some((0, 0)),
        }
    }
}

/// The standard CUDA occupancy calculation: max resident blocks per SM, taken as the min over the
/// per-block thread / register / shared-memory limits, capped at the hardware blocks-per-SM. `reg_regs`
/// is registers/thread, `shared` is total static+dynamic shared bytes/block.
fn max_blocks_per_sm(reg_regs: u32, shared: u32, block_size: i32) -> i32 {
    if block_size <= 0 {
        return 0;
    }
    let mut limit = MAX_THREADS_PER_SM / block_size; // thread-slot bound
    if reg_regs > 0 {
        let per_block = reg_regs as i64 * block_size as i64;
        limit = limit.min((MAX_REGS_PER_SM as i64 / per_block.max(1)) as i32); // register bound
    }
    if shared > 0 {
        limit = limit.min(MAX_SHARED_PER_SM / shared as i32); // shared-memory bound
    }
    limit.clamp(0, MAX_BLOCKS_PER_SM)
}

#[no_mangle]
pub extern "C" fn cuFuncGetAttribute(pi: *mut i32, attrib: i32, f: *mut c_void) -> i32 {
    if pi.is_null() {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with(|s| {
        // Validate the handle + recover the modeled function's resource use.
        let Some((num_regs, static_shared)) = FunctionResources::get(s, f) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        let dyn_shared = s.func_dyn_shared(f).unwrap_or(0);
        let cc = s.ctx.device.compute_capability;
        let arch = cc.0 as i32 * 10 + cc.1 as i32; // e.g. sm_86 → 86
        let val = match attrib {
            CU_FUNC_ATTRIBUTE_MAX_THREADS_PER_BLOCK => s.ctx.device.max_threads_per_block as i32,
            CU_FUNC_ATTRIBUTE_SHARED_SIZE_BYTES => static_shared as i32,
            CU_FUNC_ATTRIBUTE_CONST_SIZE_BYTES => 0, // constant banks not modeled
            CU_FUNC_ATTRIBUTE_LOCAL_SIZE_BYTES => 0, // local (stack) spill not modeled
            CU_FUNC_ATTRIBUTE_NUM_REGS => num_regs as i32,
            CU_FUNC_ATTRIBUTE_PTX_VERSION => arch,
            CU_FUNC_ATTRIBUTE_BINARY_VERSION => arch,
            CU_FUNC_ATTRIBUTE_CACHE_MODE_CA => 0,
            CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES => dyn_shared,
            CU_FUNC_ATTRIBUTE_PREFERRED_SHARED_MEMORY_CARVEOUT => -1, // no preference
            _ => 0, // spec-faithful default for the unmodeled attribute tail
        };
        unsafe { *pi = val };
        CUDA_SUCCESS
    })
}

/// `cuFuncSetAttribute` — record `MAX_DYNAMIC_SHARED_SIZE_BYTES` (the one attribute the model honors);
/// any other attribute is accepted as a no-op once the handle validates.
#[no_mangle]
pub extern "C" fn cuFuncSetAttribute(f: *mut c_void, attrib: i32, value: i32) -> i32 {
    ShimState::with(|s| {
        let ok = if attrib == CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES {
            s.set_func_dyn_shared(f, value)
        } else {
            s.func_dyn_shared(f).is_some() // validate the handle
        };
        if ok {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

/// `cuFuncSetCacheConfig` — record the function's preferred L1/shared split (a hint the synchronous
/// executor does not need to act on, but tracks faithfully). A bad handle is `INVALID_HANDLE`.
#[no_mangle]
pub extern "C" fn cuFuncSetCacheConfig(f: *mut c_void, config: i32) -> i32 {
    ShimState::with(|s| {
        if s.set_func_cache_config(f, config) {
            CUDA_SUCCESS
        } else {
            CUDA_ERROR_INVALID_HANDLE
        }
    })
}

#[no_mangle]
pub extern "C" fn cuOccupancyMaxActiveBlocksPerMultiprocessor(
    num_blocks: *mut i32,
    f: *mut c_void,
    block_size: i32,
    dyn_smem: usize,
) -> i32 {
    if num_blocks.is_null() || block_size <= 0 {
        return CUDA_ERROR_INVALID_VALUE;
    }
    ShimState::with(|s| {
        let Some((num_regs, static_shared)) = FunctionResources::get(s, f) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        let shared = static_shared.saturating_add(dyn_smem.min(i32::MAX as usize) as u32);
        let n = max_blocks_per_sm(num_regs, shared, block_size);
        unsafe { *num_blocks = n };
        CUDA_SUCCESS
    })
}

#[no_mangle]
pub extern "C" fn cuOccupancyMaxActiveBlocksPerMultiprocessorWithFlags(
    num_blocks: *mut i32,
    f: *mut c_void,
    block_size: i32,
    dyn_smem: usize,
    _flags: u32,
) -> i32 {
    cuOccupancyMaxActiveBlocksPerMultiprocessor(num_blocks, f, block_size, dyn_smem)
}

#[no_mangle]
pub extern "C" fn cuOccupancyMaxPotentialBlockSize(
    min_grid_size: *mut i32,
    block_size: *mut i32,
    f: *mut c_void,
    _b2d: *mut c_void,
    dyn_smem: usize,
    block_size_limit: i32,
) -> i32 {
    ShimState::with(|s| {
        let Some((num_regs, static_shared)) = FunctionResources::get(s, f) else {
            return CUDA_ERROR_INVALID_HANDLE;
        };
        let max_threads = s.ctx.device.max_threads_per_block as i32;
        let sm_count = s.ctx.device.multiprocessor_count as i32;
        // Search block sizes (in warp-size steps) for the one giving the most resident threads/SM, i.e.
        // the highest occupancy — the same objective the real API optimizes.
        let warp = s.ctx.device.warp_size as i32;
        let cap = {
            let mut c = max_threads;
            if block_size_limit > 0 {
                c = c.min(block_size_limit);
            }
            c
        };
        let shared = static_shared.saturating_add(dyn_smem.min(i32::MAX as usize) as u32);
        let (mut best_bs, mut best_threads) = (warp.max(1), 0i32);
        let mut bs = warp.max(1);
        while bs <= cap {
            let blocks = max_blocks_per_sm(num_regs, shared, bs);
            let threads = blocks * bs;
            if threads >= best_threads {
                best_threads = threads;
                best_bs = bs;
            }
            bs += warp.max(1);
        }
        if !block_size.is_null() {
            unsafe { *block_size = best_bs };
        }
        if !min_grid_size.is_null() {
            // Minimum grid to fill the device at peak occupancy: SMs × resident blocks/SM.
            unsafe { *min_grid_size = sm_count * max_blocks_per_sm(num_regs, shared, best_bs) };
        }
        CUDA_SUCCESS
    })
}

#[no_mangle]
pub extern "C" fn cuOccupancyMaxPotentialBlockSizeWithFlags(
    min_grid_size: *mut i32,
    block_size: *mut i32,
    f: *mut c_void,
    b2d: *mut c_void,
    dyn_smem: usize,
    block_size_limit: i32,
    _flags: u32,
) -> i32 {
    cuOccupancyMaxPotentialBlockSize(
        min_grid_size,
        block_size,
        f,
        b2d,
        dyn_smem,
        block_size_limit,
    )
}

// ==================================================================================================
// IR-wired: stream + event synchronization
// ==================================================================================================
