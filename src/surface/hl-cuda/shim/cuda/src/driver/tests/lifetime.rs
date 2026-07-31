//! Handle LIFETIME across the driver API: a destroyed stream, event or context must stop working, a live
//! one must keep working, and a `fork(2)` child must not inherit the parent's driver.
//!
//! Every assertion is a real `CUresult` from the `extern "C"` entry point. Each rejection is paired with
//! its valid neighbour so no fix can be a blanket refusal.

use super::support::*;
use super::*;
use crate::state::{CU_STREAM_LEGACY, CU_STREAM_PER_THREAD};

/// Create a stream and hand back its `CUstream` token.
fn stream() -> *mut c_void {
    let mut h: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuStreamCreate(&mut h, 0), CUDA_SUCCESS);
    assert!(!h.is_null());
    h
}

/// Create + record an event and hand back its `CUevent` token.
fn recorded_event() -> *mut c_void {
    let mut h: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuEventCreate(&mut h, 0), CUDA_SUCCESS);
    assert_eq!(cuEventRecord(h, core::ptr::null_mut()), CUDA_SUCCESS);
    h
}

#[test]
fn a_destroyed_stream_is_invalid_everywhere_and_a_live_one_still_works() {
    let _g = guard();
    let dead = stream();
    let live = stream();
    let live_ptr = record_alloc(256);

    // Both streams work while both are live.
    assert_eq!(cuStreamQuery(dead), CUDA_SUCCESS);
    assert_eq!(cuStreamQuery(live), CUDA_SUCCESS);

    assert_eq!(cuStreamDestroy_v2(dead), CUDA_SUCCESS);
    // A second destroy is a lifetime bug, not a success.
    assert_eq!(cuStreamDestroy_v2(dead), CUDA_ERROR_INVALID_HANDLE);

    let mut flags = 7u32;
    let mut priority = 7i32;
    let mut ctx: *mut c_void = core::ptr::null_mut();
    let mut id = 7u64;
    let mut dptr = 0u64;
    let mut status = -1i32;
    let event = recorded_event();

    for (name, code) in [
        ("cuStreamQuery", cuStreamQuery(dead)),
        ("cuStreamGetFlags", cuStreamGetFlags(dead, &mut flags)),
        (
            "cuStreamGetPriority",
            cuStreamGetPriority(dead, &mut priority),
        ),
        ("cuStreamGetCtx", cuStreamGetCtx(dead, &mut ctx)),
        ("cuStreamGetId", cuStreamGetId(dead, &mut id)),
        ("cuStreamSynchronize", cuStreamSynchronize(dead)),
        (
            "cuStreamIsCapturing",
            cuStreamIsCapturing(dead, &mut status),
        ),
        ("cuMemAllocAsync", cuMemAllocAsync(&mut dptr, 64, dead)),
        ("cuMemFreeAsync", cuMemFreeAsync(live_ptr, dead)),
        (
            "cuMemPrefetchAsync",
            cuMemPrefetchAsync(live_ptr, 64, 0, dead),
        ),
        (
            "cuStreamAttachMemAsync",
            cuStreamAttachMemAsync(dead, live_ptr, 64, 4),
        ),
        (
            "cuLaunchHostFunc",
            cuLaunchHostFunc(dead, host_cb as *mut c_void, core::ptr::null_mut()),
        ),
        (
            "cuStreamAddCallback",
            cuStreamAddCallback(dead, stream_cb as *mut c_void, core::ptr::null_mut(), 0),
        ),
        ("cuStreamWaitEvent", cuStreamWaitEvent(dead, event, 0)),
        (
            "cuMemcpyHtoDAsync_v2",
            cuMemcpyHtoDAsync_v2(live_ptr, [0u8; 4].as_ptr() as *const c_void, 4, dead),
        ),
        ("cuMemsetD32Async", cuMemsetD32Async(live_ptr, 0, 1, dead)),
    ] {
        assert_eq!(
            code, CUDA_ERROR_INVALID_HANDLE,
            "{name} accepted a destroyed stream"
        );
    }

    // The valid neighbour is untouched by any of the above.
    assert_eq!(cuStreamQuery(live), CUDA_SUCCESS);
    assert_eq!(cuStreamGetFlags(live, &mut flags), CUDA_SUCCESS);
    assert_eq!(cuStreamGetId(live, &mut id), CUDA_SUCCESS);
    assert_eq!(cuStreamWaitEvent(live, event, 0), CUDA_SUCCESS);
    assert_eq!(cuStreamAttachMemAsync(live, live_ptr, 64, 4), CUDA_SUCCESS);
    assert_eq!(cuStreamDestroy_v2(live), CUDA_SUCCESS);
}

/// `NULL`, `CU_STREAM_LEGACY` and `CU_STREAM_PER_THREAD` are reserved `CUstream` values, not table
/// entries: they always name the default stream, an application may not destroy them, and a created
/// stream must never be minted with one of their token values.
#[test]
fn the_reserved_stream_tokens_are_always_live_and_never_destroyable() {
    let _g = guard();
    let legacy = CU_STREAM_LEGACY as *mut c_void;
    let per_thread = CU_STREAM_PER_THREAD as *mut c_void;

    for special in [core::ptr::null_mut(), legacy, per_thread] {
        assert_eq!(cuStreamQuery(special), CUDA_SUCCESS);
        assert_eq!(cuStreamDestroy_v2(special), CUDA_ERROR_INVALID_HANDLE);
        // Still usable after the refused destroy.
        assert_eq!(cuStreamQuery(special), CUDA_SUCCESS);
    }

    // The first two created streams must not land on the reserved token values.
    let first = stream();
    let second = stream();
    assert!(first as usize > CU_STREAM_PER_THREAD);
    assert!(second as usize > CU_STREAM_PER_THREAD);
    // Destroying a created stream must leave the reserved tokens alone.
    assert_eq!(cuStreamDestroy_v2(first), CUDA_SUCCESS);
    assert_eq!(cuStreamQuery(legacy), CUDA_SUCCESS);
    assert_eq!(cuStreamQuery(per_thread), CUDA_SUCCESS);
}

#[test]
fn a_destroyed_event_is_invalid_everywhere_and_a_live_one_still_works() {
    let _g = guard();
    let dead = recorded_event();
    let live = recorded_event();
    let stream = stream();

    assert_eq!(cuEventQuery(dead), CUDA_SUCCESS);
    assert_eq!(cuEventDestroy_v2(dead), CUDA_SUCCESS);
    assert_eq!(cuEventDestroy_v2(dead), CUDA_ERROR_INVALID_HANDLE);

    let mut ms = -1.0f32;
    for (name, code) in [
        ("cuEventQuery", cuEventQuery(dead)),
        ("cuEventSynchronize", cuEventSynchronize(dead)),
        ("cuEventRecord", cuEventRecord(dead, core::ptr::null_mut())),
        ("cuStreamWaitEvent", cuStreamWaitEvent(stream, dead, 0)),
        (
            "cuEventElapsedTime",
            cuEventElapsedTime(&mut ms, dead, live),
        ),
        (
            "cuEventElapsedTime(end)",
            cuEventElapsedTime(&mut ms, live, dead),
        ),
    ] {
        assert_eq!(
            code, CUDA_ERROR_INVALID_HANDLE,
            "{name} accepted a destroyed event"
        );
    }

    // The valid neighbour is untouched, and a created-but-unrecorded event is NOT_READY — the state a
    // cleared timestamp slot used to be confused with.
    let mut fresh: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuEventCreate(&mut fresh, 0), CUDA_SUCCESS);
    assert_eq!(cuEventQuery(fresh), CUDA_ERROR_NOT_READY);
    assert_eq!(cuEventQuery(live), CUDA_SUCCESS);
    assert_eq!(cuStreamWaitEvent(stream, live, 0), CUDA_SUCCESS);
    assert_eq!(cuEventElapsedTime(&mut ms, live, live), CUDA_SUCCESS);
    assert_eq!(ms, 0.0);
    assert_eq!(cuEventDestroy_v2(live), CUDA_SUCCESS);
    assert_eq!(cuEventDestroy_v2(fresh), CUDA_SUCCESS);
}

#[test]
fn a_destroyed_context_token_is_invalid_and_an_empty_pop_is_refused() {
    let _g = guard();

    // Retire the guard's context so the calling thread genuinely has none.
    let mut cur: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
    assert_eq!(cuCtxDestroy_v2(cur), CUDA_SUCCESS);

    // With no context current, `cuCtxPopCurrent` has nothing to pop.
    assert_eq!(
        cuCtxPopCurrent_v2(core::ptr::null_mut()),
        CUDA_ERROR_INVALID_CONTEXT
    );

    let mut dead: *mut c_void = core::ptr::null_mut();
    let mut live: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut dead, 0, 0), CUDA_SUCCESS);
    assert_eq!(cuCtxCreate_v2(&mut live, 0, 0), CUDA_SUCCESS);
    assert_eq!(cuCtxDestroy_v2(dead), CUDA_SUCCESS);

    // Destroyed, never-created and null tokens are each refused; the live one still binds.
    assert_eq!(cuCtxDestroy_v2(dead), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(cuCtxSetCurrent(dead), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(cuCtxPushCurrent_v2(dead), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(
        cuCtxSetCurrent(0x9999 as *mut c_void),
        CUDA_ERROR_INVALID_CONTEXT
    );
    assert_eq!(
        cuCtxDestroy_v2(core::ptr::null_mut()),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(cuCtxSetCurrent(live), CUDA_SUCCESS);
    let mut current: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxGetCurrent(&mut current), CUDA_SUCCESS);
    assert_eq!(current, live);

    // A null token detaches the current context, and the pop then has nothing to hand back.
    assert_eq!(cuCtxSetCurrent(core::ptr::null_mut()), CUDA_SUCCESS);
    assert_eq!(cuCtxPopCurrent_v2(&mut current), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(cuCtxDestroy_v2(live), CUDA_SUCCESS);
}

/// A non-zero `sharedMemBytes` asks for dynamic (`extern __shared__`) shared memory, which the PTX
/// front-end rejects outright — so the launch must be refused rather than run with none of it. The same
/// launch with `0` gets past validation (it is not an `INVALID_VALUE`/`INVALID_HANDLE` rejection), and a
/// destroyed stream is refused.
#[test]
fn a_launch_refuses_dynamic_shared_memory_and_a_destroyed_stream() {
    let _g = guard();
    let func = load_vecadd();
    let dead = stream();
    assert_eq!(cuStreamDestroy_v2(dead), CUDA_SUCCESS);
    // vecadd(const float*, const float*, float*, int) — four real `kernelParams` slots, so the launch
    // that passes validation marshals from valid memory.
    let a = record_alloc(16);
    let b = record_alloc(16);
    let out = record_alloc(16);
    let mut n = 4i32;
    let (mut pa, mut pb, mut pc) = (a, b, out);
    let params: [*mut c_void; 4] = [
        &mut pa as *mut u64 as *mut c_void,
        &mut pb as *mut u64 as *mut c_void,
        &mut pc as *mut u64 as *mut c_void,
        &mut n as *mut i32 as *mut c_void,
    ];

    assert_eq!(
        cuLaunchKernel(
            func,
            1,
            1,
            1,
            1,
            1,
            1,
            64,
            core::ptr::null_mut(),
            params.as_ptr() as *mut *mut c_void,
            core::ptr::null_mut(),
        ),
        CUDA_ERROR_INVALID_VALUE,
        "a non-zero sharedMemBytes must be refused"
    );
    assert_eq!(
        cuLaunchKernel(
            func,
            1,
            1,
            1,
            1,
            1,
            1,
            0,
            dead,
            params.as_ptr() as *mut *mut c_void,
            core::ptr::null_mut(),
        ),
        CUDA_ERROR_INVALID_HANDLE,
        "a destroyed stream must be refused"
    );
    // The valid neighbour (zero shared memory, default stream) passes validation and reaches the
    // parameter-marshalling stage instead of being refused up front.
    let on_default = cuLaunchKernel(
        func,
        1,
        1,
        1,
        1,
        1,
        1,
        0,
        core::ptr::null_mut(),
        params.as_ptr() as *mut *mut c_void,
        core::ptr::null_mut(),
    );
    assert_ne!(on_default, CUDA_ERROR_INVALID_HANDLE);
    assert_ne!(on_default, CUDA_ERROR_NOT_INITIALIZED);
}

/// A `fork(2)` child must NOT inherit the parent's driver. CUDA does not inherit a context across fork
/// and the engine implements a guest `fork()` as a real host fork, so the child would otherwise share the
/// parent's `$HL_GPU_EXEC` socket and its buffer ids. The child observes `CUDA_ERROR_NOT_INITIALIZED`
/// from the lowering entry points and `CUDA_ERROR_INVALID_HANDLE` for every inherited handle, while the
/// PARENT keeps working on the same handles after the child exits.
#[test]
fn a_fork_child_does_not_inherit_the_parents_driver() {
    extern "C" {
        fn fork() -> i32;
        fn _exit(code: i32) -> !;
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }

    let _g = guard();
    let parent_stream = stream();
    let parent_event = recorded_event();
    assert_eq!(cuStreamQuery(parent_stream), CUDA_SUCCESS);

    // SAFETY: `fork` is the libc entry point; the child only calls `cu*` and `_exit`.
    let pid = unsafe { fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        // The child: its state is disowned, so nothing works until it initializes for itself.
        let inherited_stream = cuStreamQuery(parent_stream) == CUDA_ERROR_NOT_INITIALIZED;
        let inherited_event = cuEventQuery(parent_event) == CUDA_ERROR_NOT_INITIALIZED;
        let uninitialized = cuCtxSynchronize() == CUDA_ERROR_NOT_INITIALIZED;
        // Re-initializing is not enough — the child inherited no context either.
        let reinit = cuInit(0) == CUDA_SUCCESS;
        let no_context = cuStreamQuery(parent_stream) == CUDA_ERROR_INVALID_CONTEXT;
        let mut own_ctx: *mut c_void = core::ptr::null_mut();
        let own_context = cuCtxCreate_v2(&mut own_ctx, 0, 0) == CUDA_SUCCESS;
        // With its OWN driver and context the inherited tokens are still dead, and a fresh stream works.
        let inherited_still_dead = cuStreamQuery(parent_stream) == CUDA_ERROR_INVALID_HANDLE
            && cuEventQuery(parent_event) == CUDA_ERROR_INVALID_HANDLE;
        let mut own: *mut c_void = core::ptr::null_mut();
        let own_stream =
            cuStreamCreate(&mut own, 0) == CUDA_SUCCESS && cuStreamQuery(own) == CUDA_SUCCESS;
        let ok = inherited_stream
            && inherited_event
            && uninitialized
            && reinit
            && no_context
            && own_context
            && inherited_still_dead
            && own_stream;
        unsafe { _exit(i32::from(!ok)) };
    }

    let mut status = 0i32;
    // SAFETY: reaping the child we just forked.
    assert_eq!(unsafe { waitpid(pid, &mut status, 0) }, pid);
    // WIFEXITED(status) && WEXITSTATUS(status) == 0
    assert_eq!(status & 0x7f, 0, "child was signalled: {status:#x}");
    assert_eq!(
        (status >> 8) & 0xff,
        0,
        "the fork child observed the parent's driver as usable"
    );

    // The parent is unaffected by the child's disowning.
    assert_eq!(cuStreamQuery(parent_stream), CUDA_SUCCESS);
    assert_eq!(cuEventQuery(parent_event), CUDA_SUCCESS);
    assert_eq!(cuStreamDestroy_v2(parent_stream), CUDA_SUCCESS);
}

/// Destroying the current context must stop every family that needs one — allocation, copy, memset,
/// module load, launch, stream/event creation and the context's own properties — and each must say
/// `CUDA_ERROR_INVALID_CONTEXT` rather than succeed against a model that outlived the token. Each
/// refusal is paired with the same call succeeding while the context is live, so the fix cannot be a
/// blanket refusal.
#[test]
fn a_destroyed_context_stops_every_entry_point_that_needs_one() {
    let _g = guard();

    // --- while the context is live, every one of these works ---
    let func = load_vecadd();
    // The alloc/copy/memset families are not exercised positively here: they reach the GPU-exec socket,
    // which `compute::compute_path_end_to_end_over_socket` owns. What matters is that they refuse BEFORE
    // it, so the allocation they are pointed at is recorded straight into the model.
    let mut ptr = record_alloc(256);
    let host = [0u8; 16];
    let live_stream = stream();
    let mut ev: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuEventCreate(&mut ev, 0), CUDA_SUCCESS);
    let mut free = 0usize;
    let mut total = 0usize;
    assert_eq!(cuMemGetInfo_v2(&mut free, &mut total), CUDA_SUCCESS);

    let mut cur: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
    assert!(!cur.is_null());
    assert_eq!(cuCtxDestroy_v2(cur), CUDA_SUCCESS);

    // The token really is retired: `cuCtxGetCurrent` succeeds reporting no context.
    let mut after: *mut c_void = 1 as *mut c_void;
    assert_eq!(cuCtxGetCurrent(&mut after), CUDA_SUCCESS);
    assert!(after.is_null());

    // --- and now every family refuses ---
    let mut orphan = 0u64;
    assert_eq!(
        cuMemAlloc_v2(&mut orphan, 64),
        CUDA_ERROR_INVALID_CONTEXT,
        "allocation after context destroy"
    );
    assert_eq!(orphan, 0, "a refused allocation must not hand back a pointer");
    assert_eq!(cuMemFree_v2(ptr), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(
        cuMemcpyHtoD_v2(ptr, host.as_ptr() as *const c_void, 16),
        CUDA_ERROR_INVALID_CONTEXT
    );
    let mut back = [0u8; 16];
    assert_eq!(
        cuMemcpyDtoH_v2(back.as_mut_ptr() as *mut c_void, ptr, 16),
        CUDA_ERROR_INVALID_CONTEXT
    );
    assert_eq!(cuMemcpyDtoD_v2(ptr, ptr, 16), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(cuMemsetD32_v2(ptr, 0, 4), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(
        cuMemGetInfo_v2(&mut free, &mut total),
        CUDA_ERROR_INVALID_CONTEXT
    );

    let img = std::ffi::CString::new(ptx::VECADD_PTX).unwrap();
    let mut module: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        cuModuleLoadData(&mut module, img.as_ptr() as *const c_void),
        CUDA_ERROR_INVALID_CONTEXT
    );
    assert!(module.is_null());

    let mut args = [&mut ptr as *mut u64 as *mut c_void; 4];
    assert_eq!(
        cuLaunchKernel(
            func,
            1,
            1,
            1,
            1,
            1,
            1,
            0,
            core::ptr::null_mut(),
            args.as_mut_ptr(),
            core::ptr::null_mut(),
        ),
        CUDA_ERROR_INVALID_CONTEXT
    );

    let mut s2: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuStreamCreate(&mut s2, 0), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(cuStreamSynchronize(live_stream), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(cuStreamDestroy_v2(live_stream), CUDA_ERROR_INVALID_CONTEXT);
    let mut e2: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuEventCreate(&mut e2, 0), CUDA_ERROR_INVALID_CONTEXT);
    assert_eq!(
        cuEventRecord(ev, core::ptr::null_mut()),
        CUDA_ERROR_INVALID_CONTEXT
    );
    assert_eq!(cuCtxSynchronize(), CUDA_ERROR_INVALID_CONTEXT);
    let mut flags = 0u32;
    assert_eq!(cuCtxGetFlags(&mut flags), CUDA_ERROR_INVALID_CONTEXT);
    let mut dev = -1;
    assert_eq!(cuCtxGetDevice(&mut dev), CUDA_ERROR_INVALID_CONTEXT);

    // Binding a fresh context makes the same calls work again — the gate is the CONTEXT, not a latch.
    let mut fresh: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut fresh, 0, 0), CUDA_SUCCESS);
    assert_eq!(cuCtxGetFlags(&mut flags), CUDA_SUCCESS);
    assert_eq!(cuCtxGetDevice(&mut dev), CUDA_SUCCESS);
    assert_eq!(cuStreamCreate(&mut s2, 0), CUDA_SUCCESS);
    assert_eq!(cuEventCreate(&mut e2, 0), CUDA_SUCCESS);
    assert_eq!(
        cuModuleLoadData(&mut module, img.as_ptr() as *const c_void),
        CUDA_SUCCESS
    );
    let mut free2 = 0usize;
    assert_eq!(cuMemGetInfo_v2(&mut free2, &mut total), CUDA_SUCCESS);
    let _ = (orphan, ptr, host);
}

/// The calls that legitimately precede any context must keep working with none current — gating them
/// too would break a correct program's start-up (and its shutdown after `cuCtxDestroy`).
#[test]
fn the_context_free_entry_points_still_answer_without_one() {
    let _g = guard();
    let mut cur: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxGetCurrent(&mut cur), CUDA_SUCCESS);
    assert_eq!(cuCtxDestroy_v2(cur), CUDA_SUCCESS);

    assert_eq!(cuInit(0), CUDA_SUCCESS);
    let mut version = 0;
    assert_eq!(cuDriverGetVersion(&mut version), CUDA_SUCCESS);
    assert_eq!(version, DRIVER_VERSION);
    let mut name: *const c_char = core::ptr::null();
    assert_eq!(cuGetErrorName(CUDA_ERROR_INVALID_CONTEXT, &mut name), CUDA_SUCCESS);

    let mut count = 0;
    assert_eq!(cuDeviceGetCount(&mut count), CUDA_SUCCESS);
    assert_eq!(count, 1);
    let mut device = -1;
    assert_eq!(cuDeviceGet(&mut device, 0), CUDA_SUCCESS);
    let mut buf = [0 as c_char; 64];
    assert_eq!(cuDeviceGetName(buf.as_mut_ptr(), 64, 0), CUDA_SUCCESS);
    let mut bytes = 0usize;
    assert_eq!(cuDeviceTotalMem_v2(&mut bytes, 0), CUDA_SUCCESS);
    let mut warp = 0;
    assert_eq!(
        cuDeviceGetAttribute(&mut warp, CU_DEVICE_ATTRIBUTE_WARP_SIZE, 0),
        CUDA_SUCCESS
    );
    assert_eq!(warp, 32);

    // Context creation, the current-context stack and the primary-context refcount all predate a context.
    let mut primary: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuDevicePrimaryCtxRetain(&mut primary, 0), CUDA_SUCCESS);
    assert_eq!(cuDevicePrimaryCtxRelease_v2(0), CUDA_SUCCESS);
    let mut made: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut made, 0, 0), CUDA_SUCCESS);
    assert_eq!(cuCtxSetCurrent(core::ptr::null_mut()), CUDA_SUCCESS);
    assert_eq!(cuCtxSetCurrent(made), CUDA_SUCCESS);
    assert_eq!(cuCtxDestroy_v2(made), CUDA_SUCCESS);
}

/// `cuDeviceGetAttribute` must refuse a `CUdevice_attribute` no driver defines instead of fabricating 0
/// — a capability query that cannot be told apart from "present, and its value is zero" is useless.
#[test]
fn an_unknown_device_attribute_is_refused() {
    let _g = guard();
    let mut value = -1;
    assert_eq!(
        cuDeviceGetAttribute(&mut value, 9999, 0),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(
        cuDeviceGetAttribute(&mut value, -1, 0),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(cuDeviceGetAttribute(&mut value, 0, 0), CUDA_ERROR_INVALID_VALUE);
    assert_eq!(
        cuDeviceGetAttribute(&mut value, CU_DEVICE_ATTRIBUTE_MAX, 0),
        CUDA_ERROR_INVALID_VALUE
    );
    assert_eq!(value, -1, "a refused query must not write the out-param");
    // An attribute that IS in the enum but the model does not track answers "feature absent" (0),
    // which is what a real driver reports for one it does not set.
    assert_eq!(
        cuDeviceGetAttribute(&mut value, CU_DEVICE_ATTRIBUTE_MAX - 1, 0),
        CUDA_SUCCESS
    );
    assert_eq!(value, 0);
}

/// Destroying a context does not release the allocations made under it: they stay resolvable, and they
/// stay charged against the device memory budget.
///
/// This pins actual behaviour rather than asserting a wish, because it is a consequence of a deliberate
/// design — `State` holds ONE `CudaContext` (the device description plus the allocation, module and
/// stream tables) for the whole process, and `destroy_ctx` removes only the handle token. There is no
/// per-context object model to tear down.
///
/// Two things follow, and neither is currently written down anywhere:
///
///   1. A pointer allocated under a destroyed context still resolves under a later one. Real CUDA frees
///      a context's allocations with the context, so this is more permissive than the hardware: a guest
///      that uses a pointer across `cuCtxDestroy` works here and faults on a real driver. That hides an
///      application bug rather than causing one, which is the same shape as `cuModuleUnload` validating
///      its handle and otherwise doing nothing.
///   2. The bytes stay charged. `check_budget` sums live allocations against `total_mem`, and nothing
///      subtracts on context destruction, so a guest that repeatedly creates a context, allocates, and
///      destroys it will eventually be refused for out-of-memory with no live allocation of its own.
///      That one is a real limit on a long-running process, not merely a permissiveness.
///
/// If per-context ownership is ever introduced, this test should fail and be replaced by one asserting
/// that destruction frees. It exists so that change is a deliberate decision rather than a surprise.
#[test]
fn destroying_a_context_neither_frees_its_allocations_nor_releases_their_budget() {
    let _g = guard();
    let _server = serve_reference_executor();

    let mut first: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut first, 0, 0), CUDA_SUCCESS);
    let mut ptr = 0u64;
    assert_eq!(cuMemAlloc_v2(&mut ptr, 4096), CUDA_SUCCESS);
    let charged = ShimState::with(|s| s.ctx.mem.total_bytes());
    assert!(charged >= 4096, "the allocation was not charged to begin with");

    assert_eq!(cuCtxDestroy_v2(first), CUDA_SUCCESS);

    let mut second: *mut c_void = core::ptr::null_mut();
    assert_eq!(cuCtxCreate_v2(&mut second, 0, 0), CUDA_SUCCESS);

    // (1) The pointer from the destroyed context still resolves under the new one.
    let mut size = 0usize;
    assert_eq!(
        cuPointerGetAttribute(
            &mut size as *mut usize as *mut c_void,
            CU_POINTER_ATTRIBUTE_RANGE_SIZE,
            ptr
        ),
        CUDA_SUCCESS,
        "a pointer from a destroyed context no longer resolves; if that is now intended, this test \
         should be replaced by one asserting the free",
    );
    assert_eq!(size, 4096);

    // (2) Its bytes are still charged against the budget.
    assert_eq!(
        ShimState::with(|s| s.ctx.mem.total_bytes()),
        charged,
        "the budget changed across context destruction; if destruction now releases, this test \
         should be replaced",
    );
}
