use super::*;

// ==================================================================================================
// 4. oom_alloc_rejected — an allocation past the modeled device budget returns the OOM code, mints no
//    state, and never fakes success. (Regression guard for the allocator minting impossible pointers.)
// ==================================================================================================

#[test]
fn oom_alloc_rejected_with_honest_code() {
    // A deliberately tiny device budget: 1 MiB of modeled VRAM.
    let budget: u64 = 1 << 20;
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(budget));

    // A within-budget allocation succeeds and is real.
    let ok = allocate::mem_alloc(&mut ctx, &mut sink, 4096).unwrap();
    assert_eq!(ctx.mem.len(), 1);
    transfer::memcpy_htod(&mut ctx, &mut sink, ok, &i32s_to_bytes(&[1, 2, 3, 4])).unwrap();

    // A single request larger than the whole device budget is rejected with the honest OOM status — NOT a
    // minted pointer into memory the host could never back (a fake success), and no null-deref.
    let err = allocate::mem_alloc(&mut ctx, &mut sink, budget + 1).unwrap_err();
    assert_eq!(
        result::DriverStatus::from(&err).code(),
        result::CUDA_ERROR_OUT_OF_MEMORY,
        "over-budget cuMemAlloc → CUDA_ERROR_OUT_OF_MEMORY"
    );
    assert_eq!(
        result::RuntimeStatus::from(&err).code(),
        result::CUDART_ERROR_MEMORY_ALLOCATION,
        "over-budget cudaMalloc → cudaErrorMemoryAllocation"
    );
    assert_ne!(
        result::DriverStatus::from(&err).code(),
        result::CUDA_SUCCESS,
        "NOT a faked success"
    );

    // The rejected allocation minted NO state — still exactly one live allocation.
    assert_eq!(
        ctx.mem.len(),
        1,
        "the rejected alloc left the allocation table untouched"
    );

    // A cumulative over-budget request (would push total past the budget) is also rejected.
    let big = allocate::mem_alloc(&mut ctx, &mut sink, budget).unwrap_err();
    assert_eq!(
        result::DriverStatus::from(&big).code(),
        result::CUDA_ERROR_OUT_OF_MEMORY
    );
    assert_eq!(ctx.mem.len(), 1);

    // …and the allocator is still healthy afterward: a fresh within-budget alloc still works.
    let ok2 = allocate::mem_alloc(&mut ctx, &mut sink, 4096).unwrap();
    assert_ne!(ok2, ok, "post-OOM allocation is a fresh, distinct pointer");
    assert_eq!(ctx.mem.len(), 2);
}
