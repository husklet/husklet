use super::support::*;
use super::*;
#[test]
fn host_alloc_gives_a_usable_host_buffer() {
    let _g = guard();
    // cuMemAllocHost hands back real, writable host memory of the requested size.
    let mut p: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuMemAllocHost_v2(&mut p, 64), CUDA_SUCCESS);
    assert!(!p.is_null());
    unsafe {
        let b = p as *mut u8;
        for i in 0..64u8 {
            *b.add(i as usize) = i.wrapping_mul(3);
        }
        assert_eq!(*b.add(7), 21);
    }
    // free it; a second free of the same pointer is rejected (not a fake success).
    assert_eq!(cuMemFreeHost(p), CUDA_SUCCESS);
    assert_eq!(cuMemFreeHost(p), CUDA_ERROR_INVALID_VALUE);
    // a null out-pointer / null free are rejected.
    assert_eq!(
        cuMemAllocHost_v2(core::ptr::null_mut(), 16),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        cuMemFreeHost(core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );

    // the flagged form shares the same body.
    let mut q: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuMemHostAlloc(&mut q, 8, 0), CUDA_SUCCESS);
    assert!(!q.is_null());
    assert_eq!(cuMemFreeHost(q), CUDA_SUCCESS);
}

#[test]
fn host_register_unregister_round_trips() {
    let _g = guard();
    let mut buf = [0u8; 32];
    let p = buf.as_mut_ptr() as *mut c_void;
    assert_eq!(cuMemHostRegister_v2(p, 32, 0), CUDA_SUCCESS);
    // double-register is rejected.
    assert_eq!(cuMemHostRegister_v2(p, 32, 0), CUDA_ERROR_INVALID_VALUE);
    assert_eq!(cuMemHostUnregister(p), CUDA_SUCCESS);
    // unregister of an unknown range is rejected.
    assert_eq!(cuMemHostUnregister(p), CUDA_ERROR_INVALID_VALUE);
    // an unknown host pointer has no device mapping.
    let mut d = 0u64;
    assert_eq!(
        cuMemHostGetDevicePointer_v2(&mut d, p, 0),
        CUDA_ERROR_INVALID_VALUE
    );
    // null args rejected.
    assert_eq!(
        cuMemHostRegister_v2(core::ptr::null_mut(), 32, 0),
        CUDA_ERROR_INVALID_VALUE
    );
}

#[test]
fn pointer_attribute_reports_managed_memory() {
    let _g = guard();
    let managed = record_managed_alloc(4096);
    let device = record_alloc(4096);

    let mut m = 9u32;
    assert_eq!(
        cuPointerGetAttribute(
            &mut m as *mut u32 as *mut c_void,
            CU_POINTER_ATTRIBUTE_IS_MANAGED,
            managed
        ),
        CUDA_SUCCESS
    );
    assert_eq!(m, 1, "a managed allocation reports IS_MANAGED = 1");

    assert_eq!(
        cuPointerGetAttribute(
            &mut m as *mut u32 as *mut c_void,
            CU_POINTER_ATTRIBUTE_IS_MANAGED,
            device
        ),
        CUDA_SUCCESS
    );
    assert_eq!(m, 0, "a plain device allocation reports IS_MANAGED = 0");
}

#[test]
fn device_attribute_reports_configured_and_fixed_values() {
    let _g = guard();
    let want = ShimState::with(|s| s.ctx.device.clone());
    let mut v = -1i32;
    let get = |attr: i32, out: &mut i32| cuDeviceGetAttribute(out as *mut i32, attr, 0);

    assert_eq!(
        get(CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR, &mut v),
        CUDA_SUCCESS
    );
    assert_eq!(v, want.compute_capability.0 as i32);
    assert_eq!(
        get(CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR, &mut v),
        CUDA_SUCCESS
    );
    assert_eq!(v, want.compute_capability.1 as i32);
    assert_eq!(get(CU_DEVICE_ATTRIBUTE_WARP_SIZE, &mut v), CUDA_SUCCESS);
    assert_eq!(v, want.warp_size as i32);
    assert_eq!(
        get(CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT, &mut v),
        CUDA_SUCCESS
    );
    assert_eq!(v, want.multiprocessor_count as i32);
    assert_eq!(
        get(CU_DEVICE_ATTRIBUTE_MAX_THREADS_PER_BLOCK, &mut v),
        CUDA_SUCCESS
    );
    assert_eq!(v, want.max_threads_per_block as i32);
    // A fixed, truthful property of the modeled unified-memory device.
    assert_eq!(
        get(CU_DEVICE_ATTRIBUTE_UNIFIED_ADDRESSING, &mut v),
        CUDA_SUCCESS
    );
    assert_eq!(v, 1);
    // A bad device ordinal is rejected.
    assert_eq!(
        cuDeviceGetAttribute(&mut v as *mut i32, CU_DEVICE_ATTRIBUTE_WARP_SIZE, 1),
        CUDA_ERROR_INVALID_VALUE
    );
    // A null out-pointer is rejected.
    assert_eq!(
        cuDeviceGetAttribute(core::ptr::null_mut(), CU_DEVICE_ATTRIBUTE_WARP_SIZE, 0),
        CUDA_ERROR_INVALID_VALUE
    );
}

#[test]
fn ctx_push_pop_round_trips() {
    let _g = guard();
    let mut a: *mut c_void = core::ptr::null_mut();
    let mut b: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut a, 0, 0), CUDA_SUCCESS);
    assert_eq!(cuCtxCreate_v2(&mut b, 0, 0), CUDA_SUCCESS);
    assert_ne!(a, b);

    // After creating `b`, it is current.
    let mut cur: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
    assert_eq!(cur, b);

    // Push `a`; it becomes current. Pop restores `b` and hands back `a`.
    assert_eq!(cuCtxPushCurrent_v2(a), CUDA_SUCCESS);
    assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
    assert_eq!(cur, a);
    let mut popped: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxPopCurrent_v2(&mut popped), CUDA_SUCCESS);
    assert_eq!(popped, a);
    assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
    assert_eq!(cur, b);

    // A null context handle can't be pushed.
    assert_eq!(
        cuCtxPushCurrent_v2(core::ptr::null_mut()),
        CUDA_ERROR_INVALID_HANDLE
    );
}

#[test]
fn ctx_api_version_and_flags() {
    let _g = guard();
    let mut ctx: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut ctx, 7, 0), CUDA_SUCCESS);
    let mut ver = 0u32;
    assert_eq!(cuCtxGetApiVersion(ctx, &mut ver), CUDA_SUCCESS);
    assert_eq!(ver, CTX_API_VERSION);
    // Flags recorded at create are readable; SetFlags updates them.
    let mut flags = 0u32;
    assert_eq!(cuCtxGetFlags(&mut flags), CUDA_SUCCESS);
    assert_eq!(flags, 7);
    assert_eq!(cuCtxSetFlags(3), CUDA_SUCCESS);
    assert_eq!(cuCtxGetFlags(&mut flags), CUDA_SUCCESS);
    assert_eq!(flags, 3);
}

#[test]
fn primary_ctx_retain_release_refcounts() {
    let _g = guard();
    let mut p1: *mut c_void = core::ptr::null_mut();
    let mut p2: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuDevicePrimaryCtxRetain(&mut p1, 0), CUDA_SUCCESS);
    assert_eq!(cuDevicePrimaryCtxRetain(&mut p2, 0), CUDA_SUCCESS);
    assert_eq!(p1, p2, "the single device has one primary context");
    assert!(!p1.is_null());

    let mut active = -1i32;
    let mut flags = 0u32;
    assert_eq!(
        cuDevicePrimaryCtxGetState(0, &mut flags, &mut active),
        CUDA_SUCCESS
    );
    assert_eq!(active, 1, "active while a reference is held");

    // Two retains → two releases before it goes inactive.
    assert_eq!(cuDevicePrimaryCtxRelease_v2(0), CUDA_SUCCESS);
    assert_eq!(
        cuDevicePrimaryCtxGetState(0, &mut flags, &mut active),
        CUDA_SUCCESS
    );
    assert_eq!(active, 1);
    assert_eq!(cuDevicePrimaryCtxRelease_v2(0), CUDA_SUCCESS);
    assert_eq!(
        cuDevicePrimaryCtxGetState(0, &mut flags, &mut active),
        CUDA_SUCCESS
    );
    assert_eq!(active, 0, "last release deactivates the primary context");

    // A bad device ordinal is rejected.
    assert_eq!(cuDevicePrimaryCtxRelease_v2(1), CUDA_ERROR_INVALID_DEVICE);
}

#[test]
fn mem_get_info_reflects_allocations() {
    let _g = guard();
    let (mut free0, mut total0) = (0usize, 0usize);
    assert_eq!(cuMemGetInfo_v2(&mut free0, &mut total0), CUDA_SUCCESS);
    assert_eq!(free0, total0, "no allocations → all memory free");

    record_alloc(1 << 20); // 1 MiB
    let (mut free1, mut total1) = (0usize, 0usize);
    assert_eq!(cuMemGetInfo_v2(&mut free1, &mut total1), CUDA_SUCCESS);
    assert_eq!(total1, total0, "total VRAM is fixed");
    assert_eq!(
        free0 - free1,
        1 << 20,
        "free dropped by exactly the allocation size"
    );
}

#[test]
fn pointer_get_attribute_reports_device_memory() {
    let _g = guard();
    let ptr = record_alloc(4096);

    // MEMORY_TYPE of a live allocation is DEVICE.
    let mut mtype = 0u32;
    assert_eq!(
        cuPointerGetAttribute(
            &mut mtype as *mut u32 as *mut c_void,
            CU_POINTER_ATTRIBUTE_MEMORY_TYPE,
            ptr
        ),
        CUDA_SUCCESS
    );
    assert_eq!(mtype, CU_MEMORYTYPE_DEVICE);

    // RANGE_START_ADDR / RANGE_SIZE report the allocation base + size (query mid-range).
    let mut start = 0u64;
    let mut size = 0usize;
    assert_eq!(
        cuPointerGetAttribute(
            &mut start as *mut u64 as *mut c_void,
            CU_POINTER_ATTRIBUTE_RANGE_START_ADDR,
            ptr + 8
        ),
        CUDA_SUCCESS
    );
    assert_eq!(start, ptr);
    assert_eq!(
        cuPointerGetAttribute(
            &mut size as *mut usize as *mut c_void,
            CU_POINTER_ATTRIBUTE_RANGE_SIZE,
            ptr + 8
        ),
        CUDA_SUCCESS
    );
    assert_eq!(size, 4096);

    // IS_MANAGED is false (no managed-alloc path modeled).
    let mut managed = 9u32;
    assert_eq!(
        cuPointerGetAttribute(
            &mut managed as *mut u32 as *mut c_void,
            CU_POINTER_ATTRIBUTE_IS_MANAGED,
            ptr
        ),
        CUDA_SUCCESS
    );
    assert_eq!(managed, 0);

    // An unknown pointer can't honestly report a memory type.
    let mut junk = 0u32;
    assert_eq!(
        cuPointerGetAttribute(
            &mut junk as *mut u32 as *mut c_void,
            CU_POINTER_ATTRIBUTE_MEMORY_TYPE,
            0xdead_beef
        ),
        CUDA_ERROR_INVALID_VALUE
    );

    // cuMemGetAddressRange resolves the same base/size.
    let mut base = 0u64;
    let mut rsize = 0usize;
    assert_eq!(
        cuMemGetAddressRange_v2(&mut base, &mut rsize, ptr + 16),
        CUDA_SUCCESS
    );
    assert_eq!((base, rsize), (ptr, 4096));
    assert_eq!(
        cuMemGetAddressRange_v2(&mut base, &mut rsize, 0xdead_beef),
        CUDA_ERROR_INVALID_VALUE
    );
}

#[test]
fn event_query_and_elapsed_time() {
    let _g = guard();
    let mut start: *mut c_void = core::ptr::null_mut();
    let mut end: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuEventCreate(&mut start, 0), CUDA_SUCCESS);
    assert_eq!(cuEventCreate(&mut end, 0), CUDA_SUCCESS);

    // Unrecorded → NOT_READY.
    assert_eq!(cuEventQuery(start), CUDA_ERROR_NOT_READY);
    // Unknown handle → INVALID_HANDLE.
    assert_eq!(
        cuEventQuery(0x1234 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );

    // Record both; a recorded event queries ready and elapsed time is finite and non-negative.
    assert_eq!(cuEventRecord(start, core::ptr::null_mut()), CUDA_SUCCESS);
    assert_eq!(
        cuEventRecordWithFlags(end, core::ptr::null_mut(), 0),
        CUDA_SUCCESS
    );
    assert_eq!(cuEventQuery(start), CUDA_SUCCESS);
    assert_eq!(cuEventQuery(end), CUDA_SUCCESS);
    let mut ms = -1.0f32;
    assert_eq!(cuEventElapsedTime(&mut ms, start, end), CUDA_SUCCESS);
    assert!(ms >= 0.0 && ms.is_finite(), "elapsed ms was {ms}");
}

#[test]
fn stream_query_and_wait_event() {
    let _g = guard();
    // The default (null) stream is always valid and ready.
    assert_eq!(cuStreamQuery(core::ptr::null_mut()), CUDA_SUCCESS);

    let mut stream: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuStreamCreate(&mut stream, 0), CUDA_SUCCESS);
    assert_eq!(cuStreamQuery(stream), CUDA_SUCCESS);
    // An unknown stream handle is rejected.
    assert_eq!(
        cuStreamQuery(0x9999 as *mut c_void),
        CUDA_ERROR_INVALID_HANDLE
    );

    let mut ev: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuEventCreate(&mut ev, 0), CUDA_SUCCESS);
    assert_eq!(cuStreamWaitEvent(stream, ev, 0), CUDA_SUCCESS);
    assert_eq!(
        cuStreamWaitEvent(stream, 0x9999 as *mut c_void, 0),
        CUDA_ERROR_INVALID_HANDLE
    );
}

/// Every pointer attribute that cannot be honestly answered for a pointer outside any live allocation
/// reports `CUDA_ERROR_INVALID_VALUE`, rather than succeeding with a zero.
///
/// `pointer_attr` resolves the containing allocation once and lets the miss fall through as `base = 0`
/// and `size = 0`. `MEMORY_TYPE` and `DEVICE_POINTER` consult the `found` flag and refuse, but
/// `BUFFER_ID`, `RANGE_START_ADDR` and `RANGE_SIZE` did not: they returned `CUDA_SUCCESS` and wrote a
/// zero, which tells the caller the value is valid. A caller cannot then distinguish "this pointer is
/// not a live allocation" from "this allocation starts at 0 and is 0 bytes long", and code that sizes a
/// copy from `RANGE_SIZE` gets a silent zero-length transfer instead of an error at the point of the
/// mistake.
///
/// Real CUDA returns `CUDA_ERROR_INVALID_VALUE` from `cuPointerGetAttribute` for a pointer it does not
/// know, and this function's own contract already says the same.
#[test]
fn pointer_attributes_refuse_a_pointer_outside_any_allocation() {
    let _g = guard();
    // A live allocation exists, so the model is not simply empty; the queried pointer is elsewhere.
    let live = record_alloc(4096);
    let dangling = live + (1 << 20);

    let mut buffer_id = 0xDEADu64;
    let mut start = 0xDEADu64;
    let mut size = 0xDEADusize;
    for (label, attr, data) in [
        (
            "BUFFER_ID",
            CU_POINTER_ATTRIBUTE_BUFFER_ID,
            &mut buffer_id as *mut u64 as *mut c_void,
        ),
        (
            "RANGE_START_ADDR",
            CU_POINTER_ATTRIBUTE_RANGE_START_ADDR,
            &mut start as *mut u64 as *mut c_void,
        ),
        (
            "RANGE_SIZE",
            CU_POINTER_ATTRIBUTE_RANGE_SIZE,
            &mut size as *mut usize as *mut c_void,
        ),
    ] {
        let code = cuPointerGetAttribute(data, attr, dangling);
        assert_eq!(
            code, CUDA_ERROR_INVALID_VALUE,
            "{label} on a pointer outside any allocation returned {code}, so the caller was told a \
             zero was a valid answer and cannot tell a missing allocation from an empty one",
        );
    }
}
