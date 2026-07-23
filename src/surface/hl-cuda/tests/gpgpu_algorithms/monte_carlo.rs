use super::*;

const MONTECARLO_PTX: &str = r#"
    .visible .entry mc_pi(
        .param .u64 p_cnt,
        .param .u32 p_iters,
        .param .u32 p_r2
    )
    {
        ld.param.u64 %rcnt, [p_cnt];
        ld.param.u32 %riters, [p_iters];
        ld.param.u32 %rr2, [p_r2];
        mad.lo.s32 %gid, %ctaid.x, %ntid.x, %tid.x;
        // seed = gid * 2654435761 + 1   (32-bit)
        mul.lo.u32 %s, %gid, 2654435761;
        add.s32 %s, %s, 1;
        mov.u32 %hits, 0;
        mov.u32 %it, 0;
    LOOP:
        setp.ge.s32 %pl, %it, %riters;
        @%pl bra STORE;
        // x from one LCG step
        mul.lo.u32 %s, %s, 1664525;
        add.s32 %s, %s, 1013904223;
        shr.u32 %x, %s, 20;
        // y from the next LCG step
        mul.lo.u32 %s, %s, 1664525;
        add.s32 %s, %s, 1013904223;
        shr.u32 %y, %s, 20;
        // d = x*x + y*y
        mul.lo.s32 %xx, %x, %x;
        mul.lo.s32 %yy, %y, %y;
        add.s32 %d, %xx, %yy;
        setp.gt.s32 %pmiss, %d, %rr2;
        @%pmiss bra NEXT;
        add.s32 %hits, %hits, 1;
    NEXT:
        add.s32 %it, %it, 1;
        bra LOOP;
    STORE:
        cvta.to.global.u64 %gcnt, %rcnt;
        red.global.add.u32 [%gcnt], %hits;
        ret;
    }
"#;

// The identical LCG replayed on CPU — must produce the exact same integer hit count.
fn mc_reference(threads: u32, iters: u32, r2: u32) -> u32 {
    let mut total = 0u32;
    for gid in 0..threads {
        let mut s = gid.wrapping_mul(2654435761).wrapping_add(1);
        for _ in 0..iters {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let x = s >> 20;
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let y = s >> 20;
            let d = (x * x + y * y) as i32; // matches the kernel's signed compare (values < 2^31)
            if d <= r2 as i32 {
                total += 1;
            }
        }
    }
    total
}

#[test]
fn monte_carlo_pi_deterministic_hit_count_exact() {
    let block = 128u32;
    let grid = 8u32;
    let threads = block * grid; // 1024 deterministic streams
    let iters = 32u32; // 32 768 samples total
    let r = (1u32 << 12) - 1; // 4095
    let r2 = r * r; // 16 769 025

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(MONTECARLO_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "mc_pi").unwrap();

    let d_cnt = alloc_zeroed_i32(&mut sink, &mut ctx, 1);
    let args = vec![KernelArg::Ptr(d_cnt), sc(iters as i32), sc(r2 as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (grid, 1, 1),
        (block, 1, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_u32s(&readback(&mut sink, &ctx, d_cnt, 4))[0];
    let want = mc_reference(threads, iters, r2);

    assert_eq!(
        got, want,
        "Monte-Carlo quarter-circle hit count exact vs the identical CPU LCG"
    );
    // Anti-false-pass: a non-degenerate count (roughly π/4 of the samples, but asserted EXACT above).
    let total = threads * iters;
    assert!(
        got > 0 && got < total,
        "hit count is a real interior count: 0 < hits < samples"
    );
    // Sanity band only (NOT the assertion): π/4 ≈ 0.785 of samples land inside.
    assert!(
        got > total / 2,
        "roughly π/4 of samples are hits (sanity band, not the exactness check)"
    );
}

// ==================================================================================================
// 7. kmeans_step — one Lloyd iteration. Each of N 2-D integer points is assigned to its nearest of K
//    centroids by integer squared-L2 distance (ties → lowest centroid index, from strict `<`), then the
//    per-cluster coordinate sums and counts are accumulated with cross-block atomics. One thread per
//    point over a multi-block grid. Outputs the assignment array + the (pre-division) new-centroid
//    accumulators — the exact inputs the next mean-update would divide.
// ==================================================================================================
