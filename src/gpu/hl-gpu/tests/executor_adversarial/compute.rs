use super::*;

#[test]
fn over_cap_dispatch_grid_over_a_real_kernel_errors_before_iterating() {
    // DoS gap: a validated Dispatch with a huge grid over a REAL KernelProgram iterated blocks unbounded
    // (the 1M per-thread step cap bounds work WITHIN a block, but the block COUNT was uncapped). A maximal
    // grid must now be rejected with a typed ResourceLimit BEFORE a single block iterates — the checked
    // grid-product (u32::MAX^3) overflows u64 and trips the ceiling.
    let mut exec = CpuExecutor::new();
    exec.define_kernel(1, store_one_program());
    let mut res = SessionResources::new();
    exec.execute(&mut res, &store_one_setup())
        .expect("setup must run cleanly");
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::Dispatch {
                    x: u32::MAX,
                    y: u32::MAX,
                    z: u32::MAX,
                },
                Enc::EndComputePass,
            ])],
        )
        .expect_err("an over-cap grid over a real kernel must error, not iterate");
    assert_eq!(err, GpuError::ResourceLimit("dispatch grid blocks"));
    // The dispatch was rejected before touching memory: the output buffer is still its zero-initialized
    // value (the kernel's 1.0f store never ran).
    let mut out = [0u8; 4];
    exec.read_buffer(&res, BufferId(2), 0, &mut out).unwrap();
    assert_eq!(out, [0u8; 4], "over-cap dispatch left no partial effect");
}

#[test]
fn normal_dispatch_grid_over_a_real_kernel_still_computes() {
    // The companion to the cap test: a normal grid (well under the ceiling) over the same real kernel runs
    // unchanged and produces the correct result — the ceiling never perturbs a legitimate dispatch.
    let mut exec = CpuExecutor::new();
    exec.define_kernel(1, store_one_program());
    let mut res = SessionResources::new();
    exec.execute(&mut res, &store_one_setup())
        .expect("setup must run cleanly");
    exec.execute(
        &mut res,
        &[submit(vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(1),
            Enc::SetBindGroup { index: 0, group: 1 },
            Enc::Dispatch { x: 4, y: 1, z: 1 },
            Enc::EndComputePass,
        ])],
    )
    .expect("a normal grid runs cleanly");
    let mut out = [0u8; 4];
    exec.read_buffer(&res, BufferId(2), 0, &mut out).unwrap();
    assert_eq!(
        f32::from_le_bytes(out),
        1.0,
        "the kernel stored 1.0f under a normal grid"
    );
}

/// The grid ceiling's siblings: the per-block THREAD count and the shared-memory size. Both come from the
/// compiled `KernelProgram` (`block` / `shared_bytes`), which a guest front end derives from guest-supplied
/// PTX, and neither was capped — so a maximal block dimension pushes billions of thread records and a
/// maximal `.shared` declaration asks for gigabytes, both per block and both before any work is useful.
#[test]
fn an_over_cap_block_shape_is_rejected_before_any_thread_runs() {
    let mut program = store_one_program();
    program.block = [u32::MAX, u32::MAX, u32::MAX];
    let mut exec = CpuExecutor::new();
    exec.define_kernel(1, program);
    let mut res = SessionResources::new();
    exec.execute(&mut res, &store_one_setup())
        .expect("setup must run cleanly");
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::Dispatch { x: 1, y: 1, z: 1 },
                Enc::EndComputePass,
            ])],
        )
        .expect_err("an over-cap block shape must error, not allocate per-thread state");
    assert_eq!(err, GpuError::ResourceLimit("kernel block threads"));
    let mut out = [0u8; 4];
    exec.read_buffer(&res, BufferId(2), 0, &mut out).unwrap();
    assert_eq!(out, [0u8; 4], "rejected dispatch left no partial effect");
}

#[test]
fn an_over_cap_shared_memory_request_is_rejected() {
    let mut program = store_one_program();
    program.shared_bytes = u32::MAX;
    let mut exec = CpuExecutor::new();
    exec.define_kernel(1, program);
    let mut res = SessionResources::new();
    exec.execute(&mut res, &store_one_setup())
        .expect("setup must run cleanly");
    let err = exec
        .execute(
            &mut res,
            &[submit(vec![
                Enc::BeginComputePass,
                Enc::SetPipeline(1),
                Enc::SetBindGroup { index: 0, group: 1 },
                Enc::Dispatch { x: 1, y: 1, z: 1 },
                Enc::EndComputePass,
            ])],
        )
        .expect_err("an over-cap shared-memory request must error, not allocate");
    assert_eq!(err, GpuError::ResourceLimit("kernel shared memory"));
}
