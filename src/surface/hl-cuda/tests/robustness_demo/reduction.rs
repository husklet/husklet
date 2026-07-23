use super::*;

// ==================================================================================================
// 5. large_reduction_determinism — a large sum + max reduction, run twice, is identical bit-exact and
//    matches an independent CPU reference.
// ==================================================================================================

/// Run the grid-wide sum + max reduction over `input` once, returning `(sum, max)`.
fn run_reduction(input: &[i32]) -> (i32, i32) {
    let n = input.len();
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    // grid sized so grid.x * 128 >= n; the `i >= n` guard masks the tail lanes.
    let blocks = ((n + 127) / 128) as u32;

    let sum_mod = ctx.load_module(REDUCE_SUM_PTX.as_bytes()).unwrap();
    let sum_fn = load_module::module_get_function(&ctx, sum_mod, "reduce_sum").unwrap();
    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(input));
    let d_sum = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_sum, &0i32.to_le_bytes()).unwrap();
    let sum_args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_sum),
        sc_i32(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        sum_fn,
        (blocks, 1, 1),
        (128, 1, 1),
        &sum_args,
    )
    .unwrap();
    let sum = bytes_to_i32s(&readback(&mut sink, &ctx, d_sum, 4))[0];

    let max_mod = ctx.load_module(REDUCE_MAX_PTX.as_bytes()).unwrap();
    let max_fn = load_module::module_get_function(&ctx, max_mod, "reduce_max").unwrap();
    let d_max = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_max, &i32::MIN.to_le_bytes()).unwrap();
    let max_args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_max),
        sc_i32(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        max_fn,
        (blocks, 1, 1),
        (128, 1, 1),
        &max_args,
    )
    .unwrap();
    let max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, 4))[0];

    (sum, max)
}

#[test]
fn large_reduction_is_deterministic_and_matches_cpu() {
    let n = 100_000usize;
    // A mix of signs so the signed max and wrapping sum are both non-trivial.
    let input: Vec<i32> = (0..n)
        .map(|i| ((i as i32).wrapping_mul(2654435761u32 as i32)) % 100_003 - 50_000)
        .collect();

    let (sum1, max1) = run_reduction(&input);
    let (sum2, max2) = run_reduction(&input);

    // Determinism: two independent runs are bit-identical.
    assert_eq!(
        (sum1, max1),
        (sum2, max2),
        "the reduction is deterministic across runs"
    );

    // Correctness: matches an independent CPU reference (wrapping add is associative → order-independent).
    let cpu_sum = input.iter().fold(0i32, |acc, v| acc.wrapping_add(*v));
    let cpu_max = *input.iter().max().unwrap();
    assert_eq!(
        sum1, cpu_sum,
        "grid sum matches the CPU reference over {n} elements"
    );
    assert_eq!(
        max1, cpu_max,
        "grid max matches the CPU reference over {n} elements"
    );
}
