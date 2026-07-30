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
        // The child: the inherited tokens must be dead and the driver uninitialized.
        let inherited_stream = cuStreamQuery(parent_stream) == CUDA_ERROR_INVALID_HANDLE;
        let inherited_event = cuEventQuery(parent_event) == CUDA_ERROR_INVALID_HANDLE;
        let uninitialized = cuCtxSynchronize() == CUDA_ERROR_NOT_INITIALIZED;
        // Re-initializing in the child gives it its OWN driver: a fresh stream of its own works.
        let reinit = cuInit(0) == CUDA_SUCCESS;
        let mut own: *mut c_void = core::ptr::null_mut();
        let own_stream =
            cuStreamCreate(&mut own, 0) == CUDA_SUCCESS && cuStreamQuery(own) == CUDA_SUCCESS;
        let ok = inherited_stream && inherited_event && uninitialized && reinit && own_stream;
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
