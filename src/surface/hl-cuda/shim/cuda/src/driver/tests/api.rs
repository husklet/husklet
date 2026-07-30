use super::support::*;
use super::*;
#[test]
fn module_globals_and_unload_error_paths() {
    let _g = guard();
    // A module declaring a `.global` variable (module load emits no IR — pure model bookkeeping).
    let src = ".visible .global .align 4 .b8 gState[128];\n.visible .entry noop() { ret; }\n";
    let img = std::ffi::CString::new(src).unwrap();
    let mut module: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuModuleLoadData(&mut module, img.as_ptr() as *const c_void),
        CUDA_SUCCESS
    );

    // An undeclared symbol is honestly NOT_FOUND (this path resolves the size first and never submits).
    let missing = std::ffi::CString::new("nope").unwrap();
    let (mut dptr, mut bytes) = (0u64, 0usize);
    assert_eq!(
        cuModuleGetGlobal_v2(&mut dptr, &mut bytes, module, missing.as_ptr()),
        CUDA_ERROR_NOT_FOUND
    );
    // A bogus module handle is INVALID_HANDLE; a null name is INVALID_VALUE.
    let name = std::ffi::CString::new("gState").unwrap();
    assert_eq!(
        cuModuleGetGlobal_v2(&mut dptr, &mut bytes, 0x9999 as *mut c_void, name.as_ptr()),
        CUDA_ERROR_INVALID_HANDLE
    );
    assert_eq!(
        cuModuleGetGlobal_v2(&mut dptr, &mut bytes, module, core::ptr::null()),
        CUDA_ERROR_INVALID_VALUE
    );

    // Unload validates the handle: a real module succeeds, a bogus one is INVALID_HANDLE.
    assert_eq!(cuModuleUnload(module), CUDA_SUCCESS);
    assert_eq!(
        cuModuleUnload(0x9999 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );

    // Texref/surfref are unmodeled → NOT_FOUND for a valid module, INVALID_HANDLE for a bogus one.
    let mut tref: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuModuleGetTexRef(&mut tref, module, name.as_ptr()),
        CUDA_ERROR_NOT_FOUND
    );
    assert_eq!(
        cuModuleGetSurfRef(&mut tref, module, name.as_ptr()),
        CUDA_ERROR_NOT_FOUND
    );
    assert_eq!(
        cuModuleGetTexRef(&mut tref, 0x9999 as *mut c_void, name.as_ptr()),
        CUDA_ERROR_INVALID_HANDLE
    );

    // Loading mode is reported (eager).
    let mut mode = -1i32;
    assert_eq!(cuModuleGetLoadingMode(&mut mode), CUDA_SUCCESS);
    assert_eq!(mode, 1);
    assert_eq!(
        cuModuleGetLoadingMode(core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );
}

#[test]
fn func_get_attribute_reports_modeled_function() {
    let _g = guard();
    let f = load_vecadd();
    let want_max = ShimState::with(|s| s.ctx.device.max_threads_per_block as i32);

    let mut v = -1i32;
    let get =
        |attr: i32, out: &mut i32, f: *mut c_void| cuFuncGetAttribute(out as *mut i32, attr, f);

    // MAX_THREADS_PER_BLOCK is the modeled device's real value.
    assert_eq!(
        get(CU_FUNC_ATTRIBUTE_MAX_THREADS_PER_BLOCK, &mut v, f),
        CUDA_SUCCESS
    );
    assert_eq!(v, want_max);
    // NUM_REGS is the function's real recovered register count (> 0 for vecadd).
    assert_eq!(get(CU_FUNC_ATTRIBUTE_NUM_REGS, &mut v, f), CUDA_SUCCESS);
    assert!(v > 0, "vecadd uses registers, got {v}");
    // PTX/BINARY version derive from the device compute capability (sm_86 → 86).
    assert_eq!(get(CU_FUNC_ATTRIBUTE_PTX_VERSION, &mut v, f), CUDA_SUCCESS);
    assert_eq!(v, 86);
    // vecadd declares no static shared memory.
    assert_eq!(
        get(CU_FUNC_ATTRIBUTE_SHARED_SIZE_BYTES, &mut v, f),
        CUDA_SUCCESS
    );
    assert_eq!(v, 0);

    // A bad handle / null out-pointer are rejected honestly.
    assert_eq!(
        get(CU_FUNC_ATTRIBUTE_NUM_REGS, &mut v, 0x1234 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );
    assert_eq!(
        cuFuncGetAttribute(core::ptr::null_mut(), CU_FUNC_ATTRIBUTE_NUM_REGS, f),
        CUDA_ERROR_INVALID_VALUE
    );
}

#[test]
fn func_set_attribute_and_cache_config_round_trip() {
    let _g = guard();
    let f = load_vecadd();

    // Opting in to dynamic shared memory is REFUSED: `.extern .shared` is rejected by the PTX front-end,
    // so no kernel can ever be handed the bytes. Accepting the opt-in and echoing it back would advertise
    // a resource that does not exist.
    assert_eq!(
        cuFuncSetAttribute(f, CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES, 2048),
        CUDA_ERROR_INVALID_VALUE
    );
    // Setting it to 0 (the modeled value) is the honest no-op, and the getter reports 0.
    assert_eq!(
        cuFuncSetAttribute(f, CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES, 0),
        CUDA_SUCCESS
    );
    let mut v = -1i32;
    assert_eq!(
        cuFuncGetAttribute(&mut v, CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES, f),
        CUDA_SUCCESS
    );
    assert_eq!(v, 0);

    // cache config records (no-op hint) for a valid handle; a bad handle is rejected.
    assert_eq!(cuFuncSetCacheConfig(f, 1), CUDA_SUCCESS);
    assert_eq!(
        cuFuncSetCacheConfig(0x1234 as *mut c_void, 1),
        CUDA_ERROR_INVALID_HANDLE
    );
    assert_eq!(
        cuFuncSetAttribute(
            0x1234 as *mut c_void,
            CU_FUNC_ATTRIBUTE_MAX_DYNAMIC_SHARED_SIZE_BYTES,
            1
        ),
        CUDA_ERROR_INVALID_HANDLE
    );
}

#[test]
fn occupancy_is_sane_for_vecadd() {
    let _g = guard();
    let f = load_vecadd();

    let mut n = -1i32;
    assert_eq!(
        cuOccupancyMaxActiveBlocksPerMultiprocessor(&mut n, f, 256, 0),
        CUDA_SUCCESS
    );
    assert!(n > 0 && n <= 32, "expected a sane block count, got {n}");
    // WithFlags shares the body.
    let mut n2 = -1i32;
    assert_eq!(
        cuOccupancyMaxActiveBlocksPerMultiprocessorWithFlags(&mut n2, f, 256, 0, 0),
        CUDA_SUCCESS
    );
    assert_eq!(n, n2);

    // Invalid args rejected.
    assert_eq!(
        cuOccupancyMaxActiveBlocksPerMultiprocessor(core::ptr::null_mut(), f, 256, 0),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        cuOccupancyMaxActiveBlocksPerMultiprocessor(&mut n, f, 0, 0),
        CUDA_ERROR_INVALID_VALUE
    );

    // Potential-block-size returns a positive block size + a grid that fills the modeled device.
    let (mut min_grid, mut block) = (-1i32, -1i32);
    assert_eq!(
        cuOccupancyMaxPotentialBlockSize(&mut min_grid, &mut block, f, core::ptr::null_mut(), 0, 0),
        CUDA_SUCCESS
    );
    assert!(block > 0, "block size {block}");
    assert!(min_grid > 0, "min grid {min_grid}");
}

#[test]
fn ctx_limits_and_cache_config_round_trip() {
    let _g = guard();
    // A modeled limit reads its default, then round-trips a set value.
    let mut v = 0usize;
    assert_eq!(cuCtxGetLimit(&mut v, 0), CUDA_SUCCESS);
    assert_eq!(v, 1024, "CU_LIMIT_STACK_SIZE default");
    assert_eq!(cuCtxSetLimit(0, 4096), CUDA_SUCCESS);
    assert_eq!(cuCtxGetLimit(&mut v, 0), CUDA_SUCCESS);
    assert_eq!(v, 4096);

    // Out-of-range limits are rejected on both get and set.
    assert_eq!(
        cuCtxGetLimit(&mut v, CU_LIMIT_MAX),
        CUDA_ERROR_UNSUPPORTED_LIMIT
    );
    assert_eq!(cuCtxSetLimit(CU_LIMIT_MAX, 1), CUDA_ERROR_UNSUPPORTED_LIMIT);
    assert_eq!(
        cuCtxGetLimit(core::ptr::null_mut(), 0),
        CUDA_ERROR_INVALID_VALUE
    );

    // Cache config round-trips.
    let mut c = -1i32;
    assert_eq!(cuCtxGetCacheConfig(&mut c), CUDA_SUCCESS);
    assert_eq!(c, 0);
    assert_eq!(cuCtxSetCacheConfig(2), CUDA_SUCCESS);
    assert_eq!(cuCtxGetCacheConfig(&mut c), CUDA_SUCCESS);
    assert_eq!(c, 2);

    // Stream priority range collapses to a single band.
    let (mut lo, mut hi) = (-9i32, -9i32);
    assert_eq!(cuCtxGetStreamPriorityRange(&mut lo, &mut hi), CUDA_SUCCESS);
    assert_eq!((lo, hi), (0, 0));
}

#[test]
fn stream_getters_report_creation_state() {
    let _g = guard();
    let mut ctx: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut ctx, 0, 0), CUDA_SUCCESS);

    let mut stream: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuStreamCreate(&mut stream, 1), CUDA_SUCCESS);

    // Flags/priority reflect creation; ctx is the current context; id is a stable non-null value.
    let mut flags = 0u32;
    assert_eq!(cuStreamGetFlags(stream, &mut flags), CUDA_SUCCESS);
    assert_eq!(flags, 1);
    let mut prio = -9i32;
    assert_eq!(cuStreamGetPriority(stream, &mut prio), CUDA_SUCCESS);
    assert_eq!(prio, 0);
    let mut owner: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuStreamGetCtx(stream, &mut owner), CUDA_SUCCESS);
    assert_eq!(owner, ctx);
    let mut id = 12345u64;
    assert_eq!(cuStreamGetId(stream, &mut id), CUDA_SUCCESS);
    assert_eq!(id, stream as u64);

    // The default (null) stream reports flags/priority 0.
    assert_eq!(
        cuStreamGetFlags(core::ptr::null_mut(), &mut flags),
        CUDA_SUCCESS
    );
    assert_eq!(flags, 0);

    // A bad handle is rejected.
    assert_eq!(
        cuStreamGetFlags(0x9999 as *mut c_void, &mut flags),
        CUDA_ERROR_INVALID_HANDLE
    );
    assert_eq!(
        cuStreamGetId(0x9999 as *mut c_void, &mut id),
        CUDA_ERROR_INVALID_HANDLE
    );
}

// ---- newly-implemented tail: device identity / context / function / dispatch / hints ----------
