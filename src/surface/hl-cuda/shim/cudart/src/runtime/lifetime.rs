//! Handle LIFETIME across the CUDA Runtime API: a destroyed stream or event must stop working, a live one
//! must keep working, `cudaDeviceReset` must really tear the context down, and a `fork(2)` child must not
//! inherit the parent's driver.
//!
//! Every assertion is a real `cudaError_t` from the `extern "C"` entry point, and each rejection is paired
//! with its valid neighbour so no fix can be a blanket refusal.

use super::*;
use crate::state::{reset, serial, CUDA_STREAM_LEGACY, CUDA_STREAM_PER_THREAD};
use core::ffi::c_void;

const SUCCESS: i32 = 0;
const INVALID_VALUE: i32 = 1;
const INVALID_DEVICE_FUNCTION: i32 = 98;
const INVALID_RESOURCE_HANDLE: i32 = 400;
const NOT_READY: i32 = 600;

fn stream() -> *mut c_void {
    let mut h: *mut c_void = core::ptr::null_mut();
    assert_eq!(cudaStreamCreate(&mut h), SUCCESS);
    assert!(!h.is_null());
    h
}

fn recorded_event() -> *mut c_void {
    let mut h: *mut c_void = core::ptr::null_mut();
    assert_eq!(cudaEventCreate(&mut h), SUCCESS);
    assert_eq!(cudaEventRecord(h, core::ptr::null_mut()), SUCCESS);
    h
}

/// A live device allocation recorded straight in the model, so these tests need no `$HL_GPU_EXEC` server.
fn record_alloc(size: u64) -> *mut c_void {
    crate::state::ShimState::with(|s| {
        let buffer = s.ctx.alloc_buffer();
        s.ctx.mem.insert(buffer, size).0 as *mut c_void
    })
}

#[test]
fn a_destroyed_stream_is_invalid_everywhere_and_a_live_one_still_works() {
    let _serial = serial();
    reset();
    let dead = stream();
    let live = stream();
    let ptr = record_alloc(64);
    let event = recorded_event();

    assert_eq!(cudaStreamQuery(dead), SUCCESS);
    assert_eq!(cudaStreamDestroy(dead), SUCCESS);
    // A second destroy is a lifetime bug, not a success.
    assert_eq!(cudaStreamDestroy(dead), INVALID_RESOURCE_HANDLE);

    let src = [0u8; 4];
    for (name, code) in [
        ("cudaStreamQuery", cudaStreamQuery(dead)),
        ("cudaStreamWaitEvent", cudaStreamWaitEvent(dead, event, 0)),
        (
            "cudaMemcpyAsync",
            cudaMemcpyAsync(ptr, src.as_ptr() as *const c_void, 4, 1, dead),
        ),
        ("cudaMemsetAsync", cudaMemsetAsync(ptr, 0, 4, dead)),
    ] {
        assert_eq!(
            code, INVALID_RESOURCE_HANDLE,
            "{name} accepted a destroyed stream"
        );
    }

    // The valid neighbour is untouched: the same calls on a live stream still reach the lowering.
    assert_eq!(cudaStreamQuery(live), SUCCESS);
    assert_eq!(cudaStreamWaitEvent(live, event, 0), SUCCESS);
    assert_ne!(
        cudaMemsetAsync(ptr, 0, 4, live),
        INVALID_RESOURCE_HANDLE,
        "a live stream must not be refused"
    );
    assert_eq!(cudaStreamDestroy(live), SUCCESS);
    assert_eq!(cudaEventDestroy(event), SUCCESS);
}

/// `NULL`, `cudaStreamLegacy` and `cudaStreamPerThread` are reserved `cudaStream_t` values: always the
/// default stream, never destroyable, and never minted as a created stream's token.
#[test]
fn the_reserved_stream_tokens_are_always_live_and_never_destroyable() {
    let _serial = serial();
    reset();
    for special in [
        core::ptr::null_mut(),
        CUDA_STREAM_LEGACY as *mut c_void,
        CUDA_STREAM_PER_THREAD as *mut c_void,
    ] {
        assert_eq!(cudaStreamQuery(special), SUCCESS);
        assert_eq!(cudaStreamDestroy(special), INVALID_RESOURCE_HANDLE);
        assert_eq!(cudaStreamQuery(special), SUCCESS);
    }
    let first = stream();
    assert!(first as usize > CUDA_STREAM_PER_THREAD);
    assert_eq!(cudaStreamDestroy(first), SUCCESS);
    assert_eq!(cudaStreamQuery(CUDA_STREAM_LEGACY as *mut c_void), SUCCESS);
}

#[test]
fn a_destroyed_event_is_invalid_everywhere_and_a_live_one_still_works() {
    let _serial = serial();
    reset();
    let dead = recorded_event();
    let live = recorded_event();
    let on = stream();

    assert_eq!(cudaEventQuery(dead), SUCCESS);
    assert_eq!(cudaEventDestroy(dead), SUCCESS);
    assert_eq!(cudaEventDestroy(dead), INVALID_RESOURCE_HANDLE);

    let mut ms = -1.0f32;
    for (name, code) in [
        ("cudaEventQuery", cudaEventQuery(dead)),
        ("cudaEventSynchronize", cudaEventSynchronize(dead)),
        (
            "cudaEventRecord",
            cudaEventRecord(dead, core::ptr::null_mut()),
        ),
        ("cudaStreamWaitEvent", cudaStreamWaitEvent(on, dead, 0)),
        (
            "cudaEventElapsedTime",
            cudaEventElapsedTime(&mut ms, dead, live),
        ),
    ] {
        assert_eq!(
            code, INVALID_RESOURCE_HANDLE,
            "{name} accepted a destroyed event"
        );
    }

    // A created-but-unrecorded event is NOT_READY — the state a cleared timestamp slot was confused with.
    let mut fresh: *mut c_void = core::ptr::null_mut();
    assert_eq!(cudaEventCreate(&mut fresh), SUCCESS);
    assert_eq!(cudaEventQuery(fresh), NOT_READY);
    assert_eq!(cudaEventElapsedTime(&mut ms, fresh, live), NOT_READY);
    // The valid neighbour is untouched.
    assert_eq!(cudaEventQuery(live), SUCCESS);
    assert_eq!(cudaEventElapsedTime(&mut ms, live, live), SUCCESS);
    assert_eq!(ms, 0.0);
    assert_eq!(cudaEventDestroy(live), SUCCESS);
    assert_eq!(cudaEventDestroy(fresh), SUCCESS);
}

/// `cudaDeviceReset` destroys the primary context: every allocation, stream and event goes with it, so a
/// handle or pointer taken beforehand stops working. A freshly created one afterwards works.
#[test]
fn device_reset_invalidates_every_allocation_stream_and_event() {
    let _serial = serial();
    reset();
    let ptr = record_alloc(64);
    let before = stream();
    let event = recorded_event();
    assert_eq!(cudaStreamQuery(before), SUCCESS);
    assert_eq!(cudaEventQuery(event), SUCCESS);
    let (mut free, mut total) = (0usize, 0usize);
    assert_eq!(cudaMemGetInfo(&mut free, &mut total), SUCCESS);
    assert!(free < total, "the 64-byte allocation is accounted for");

    assert_eq!(cudaDeviceReset(), SUCCESS);

    // The allocation table is gone: all device memory is free again and the old pointer no longer
    // resolves. The stream and event handles are gone with the context they belonged to.
    assert_eq!(cudaMemGetInfo(&mut free, &mut total), SUCCESS);
    assert_eq!(free, total, "cudaDeviceReset must release every allocation");
    assert_eq!(cudaFree(ptr), INVALID_VALUE, "a pre-reset pointer is dead");
    assert_eq!(cudaStreamQuery(before), INVALID_RESOURCE_HANDLE);
    assert_eq!(cudaEventQuery(event), INVALID_RESOURCE_HANDLE);
    // A fresh context works, so the reset is not a permanent shutdown.
    let fresh = stream();
    assert_eq!(cudaStreamQuery(fresh), SUCCESS);
    let mut fresh_event: *mut c_void = core::ptr::null_mut();
    assert_eq!(cudaEventCreate(&mut fresh_event), SUCCESS);
    assert_eq!(cudaEventQuery(fresh_event), NOT_READY);
}

/// `cudaFuncGetAttributes` on a host pointer nvcc never registered describes a function that does not
/// exist; real CUDA answers `cudaErrorInvalidDeviceFunction`. A REGISTERED stub still reports its real
/// figures, so the rejection is not a blanket refusal.
#[test]
fn func_get_attributes_refuses_an_unregistered_function() {
    let _serial = serial();
    reset();
    let mut attr = vec![0u8; 256];
    let unregistered = 0x9999_usize as *const c_void;
    assert_eq!(
        cudaFuncGetAttributes(attr.as_mut_ptr() as *mut c_void, unregistered),
        INVALID_DEVICE_FUNCTION
    );
    assert_eq!(
        cudaFuncGetAttributes(attr.as_mut_ptr() as *mut c_void, core::ptr::null()),
        INVALID_DEVICE_FUNCTION
    );
    assert_eq!(
        cudaFuncGetAttributes(core::ptr::null_mut(), unregistered),
        INVALID_VALUE
    );
}

/// A non-zero `sharedMem` asks for dynamic (`extern __shared__`) shared memory, which the PTX front-end
/// rejects outright, and a destroyed stream must not carry a launch. Both are refused BEFORE the host-fn
/// pointer is even resolved, so the codes are the argument faults and not a registration fault.
#[test]
fn a_launch_refuses_dynamic_shared_memory_and_a_destroyed_stream() {
    let _serial = serial();
    reset();
    let dead = stream();
    assert_eq!(cudaStreamDestroy(dead), SUCCESS);
    let grid = crate::Dim3 { x: 1, y: 1, z: 1 };
    let block = crate::Dim3 { x: 1, y: 1, z: 1 };
    let stub = 0x9999_usize as *const c_void;

    assert_eq!(
        cudaLaunchKernel(
            stub,
            grid,
            block,
            core::ptr::null_mut(),
            64,
            core::ptr::null_mut()
        ),
        INVALID_VALUE,
        "a non-zero sharedMem must be refused"
    );
    assert_eq!(
        cudaLaunchKernel(stub, grid, block, core::ptr::null_mut(), 0, dead),
        INVALID_RESOURCE_HANDLE,
        "a destroyed stream must be refused"
    );
    // The valid neighbour (zero shared memory, default stream) gets past both checks and fails only on
    // the unregistered stub — proving the two refusals above are specific, not a blanket rejection.
    assert_ne!(
        cudaLaunchKernel(
            stub,
            grid,
            block,
            core::ptr::null_mut(),
            0,
            core::ptr::null_mut()
        ),
        INVALID_RESOURCE_HANDLE
    );
}

/// A `fork(2)` child must not inherit the parent's driver: CUDA does not inherit a context across fork,
/// and the engine implements a guest `fork()` as a real host fork, so otherwise both processes would
/// interleave frames on one `$HL_GPU_EXEC` socket and claim the same buffer ids. The child sees every
/// inherited handle and pointer as dead; the parent is unaffected.
#[test]
fn a_fork_child_does_not_inherit_the_parents_driver() {
    extern "C" {
        fn fork() -> i32;
        fn _exit(code: i32) -> !;
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }

    let _serial = serial();
    reset();
    let parent_stream = stream();
    let parent_event = recorded_event();
    let parent_ptr = record_alloc(64);
    assert_eq!(cudaStreamQuery(parent_stream), SUCCESS);
    let (mut free, mut total) = (0usize, 0usize);
    assert_eq!(cudaMemGetInfo(&mut free, &mut total), SUCCESS);
    assert!(free < total, "the parent's allocation is accounted for");

    // SAFETY: `fork` is the libc entry point; the child only calls `cuda*` and `_exit`.
    let pid = unsafe { fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        let dead_stream = cudaStreamQuery(parent_stream) == INVALID_RESOURCE_HANDLE;
        let dead_event = cudaEventQuery(parent_event) == INVALID_RESOURCE_HANDLE;
        let dead_ptr = cudaFree(parent_ptr) == INVALID_VALUE;
        // The child's allocation table is empty — it did not inherit the parent's buffer ids.
        let (mut cfree, mut ctotal) = (0usize, 0usize);
        let fresh_table = cudaMemGetInfo(&mut cfree, &mut ctotal) == SUCCESS && cfree == ctotal;
        // The child gets its OWN context: a stream it creates itself works.
        let mut own: *mut c_void = core::ptr::null_mut();
        let own_stream = cudaStreamCreate(&mut own) == SUCCESS && cudaStreamQuery(own) == SUCCESS;
        // Distinct exit codes so a failure names the check that did not hold.
        let code = match (dead_stream, dead_event, dead_ptr, fresh_table, own_stream) {
            (true, true, true, true, true) => 0,
            (false, ..) => 1,
            (_, false, ..) => 2,
            (_, _, false, ..) => 3,
            (_, _, _, false, _) => 4,
            _ => 5,
        };
        unsafe { _exit(code) };
    }

    let mut status = 0i32;
    // SAFETY: reaping the child we just forked.
    assert_eq!(unsafe { waitpid(pid, &mut status, 0) }, pid);
    assert_eq!(status & 0x7f, 0, "child was signalled: {status:#x}");
    assert_eq!(
        (status >> 8) & 0xff,
        0,
        "the fork child observed the parent's driver as usable"
    );

    // The parent is unaffected by the child's disowning.
    assert_eq!(cudaStreamQuery(parent_stream), SUCCESS);
    assert_eq!(cudaEventQuery(parent_event), SUCCESS);
    assert_eq!(cudaMemGetInfo(&mut free, &mut total), SUCCESS);
    assert!(free < total, "the parent's allocation survived the child");
}
