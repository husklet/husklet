use super::*;

const KMEANS_PTX: &str = r#"
    .visible .entry kmeans(
        .param .u64 p_px,
        .param .u64 p_py,
        .param .u64 p_cx,
        .param .u64 p_cy,
        .param .u64 p_assign,
        .param .u64 p_sumx,
        .param .u64 p_sumy,
        .param .u64 p_count,
        .param .u32 p_n,
        .param .u32 p_k
    )
    {
        ld.param.u64 %rpx, [p_px];
        ld.param.u64 %rpy, [p_py];
        ld.param.u64 %rcx, [p_cx];
        ld.param.u64 %rcy, [p_cy];
        ld.param.u64 %rassign, [p_assign];
        ld.param.u64 %rsumx, [p_sumx];
        ld.param.u64 %rsumy, [p_sumy];
        ld.param.u64 %rcount, [p_count];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rk, [p_k];
        mad.lo.s32 %i, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %i, %rn;
        @%pdone bra DONE;
        cvta.to.global.u64 %gpx, %rpx;
        cvta.to.global.u64 %gpy, %rpy;
        cvta.to.global.u64 %gcx, %rcx;
        cvta.to.global.u64 %gcy, %rcy;
        mul.wide.s32 %io, %i, 4;
        add.s64 %pxp, %gpx, %io;
        ld.global.u32 %px, [%pxp];
        add.s64 %pyp, %gpy, %io;
        ld.global.u32 %py, [%pyp];
        // scan centroids for the nearest (strict < keeps the lowest index on ties)
        mov.u32 %best, 2147483647;
        mov.u32 %bestk, 0;
        mov.u32 %c, 0;
    KLOOP:
        setp.ge.s32 %pk, %c, %rk;
        @%pk bra ASSIGN;
        mul.wide.s32 %co, %c, 4;
        add.s64 %cxp, %gcx, %co;
        ld.global.u32 %cxv, [%cxp];
        add.s64 %cyp, %gcy, %co;
        ld.global.u32 %cyv, [%cyp];
        sub.s32 %dx, %px, %cxv;
        sub.s32 %dy, %py, %cyv;
        mul.lo.s32 %dxx, %dx, %dx;
        mul.lo.s32 %dyy, %dy, %dy;
        add.s32 %dist, %dxx, %dyy;
        setp.ge.s32 %pnb, %dist, %best;
        @%pnb bra KNEXT;
        mov.u32 %best, %dist;
        mov.u32 %bestk, %c;
    KNEXT:
        add.s32 %c, %c, 1;
        bra KLOOP;
    ASSIGN:
        cvta.to.global.u64 %gassign, %rassign;
        add.s64 %ap, %gassign, %io;
        st.global.u32 [%ap], %bestk;
        // atomic accumulate into cluster bestk
        mul.wide.s32 %bko, %bestk, 4;
        cvta.to.global.u64 %gsumx, %rsumx;
        add.s64 %sxp, %gsumx, %bko;
        red.global.add.u32 [%sxp], %px;
        cvta.to.global.u64 %gsumy, %rsumy;
        add.s64 %syp, %gsumy, %bko;
        red.global.add.u32 [%syp], %py;
        cvta.to.global.u64 %gcount, %rcount;
        add.s64 %ctp, %gcount, %bko;
        red.global.add.u32 [%ctp], 1;
    DONE:
        ret;
    }
"#;

#[test]
fn kmeans_step_assign_and_accumulate_exact() {
    let k = 3usize;
    // Three loose clusters of integer points around (10,10), (40,12), (25,40).
    let centers = [(10i32, 10i32), (40, 12), (25, 40)];
    let mut px = Vec::new();
    let mut py = Vec::new();
    for (ci, &(bx, by)) in centers.iter().enumerate() {
        for j in 0..10i32 {
            // deterministic scatter around each center
            let ox = ((j * 7 + ci as i32 * 3) % 11) - 5;
            let oy = ((j * 5 + ci as i32 * 2) % 9) - 4;
            px.push(bx + ox);
            py.push(by + oy);
        }
    }
    let n = px.len(); // 30
                      // Initial centroids (deliberately offset from the true centers so assignment is non-trivial).
    let cx: Vec<i32> = vec![8, 42, 27];
    let cy: Vec<i32> = vec![12, 10, 38];

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(KMEANS_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "kmeans").unwrap();

    let d_px = upload(&mut sink, &mut ctx, &i32s_to_bytes(&px));
    let d_py = upload(&mut sink, &mut ctx, &i32s_to_bytes(&py));
    let d_cx = upload(&mut sink, &mut ctx, &i32s_to_bytes(&cx));
    let d_cy = upload(&mut sink, &mut ctx, &i32s_to_bytes(&cy));
    let d_assign = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_sumx = alloc_zeroed_i32(&mut sink, &mut ctx, k);
    let d_sumy = alloc_zeroed_i32(&mut sink, &mut ctx, k);
    let d_count = alloc_zeroed_i32(&mut sink, &mut ctx, k);

    let block = 16u32;
    let grid = (n as u32 + block - 1) / block; // 2 blocks
    let args = vec![
        KernelArg::Ptr(d_px),
        KernelArg::Ptr(d_py),
        KernelArg::Ptr(d_cx),
        KernelArg::Ptr(d_cy),
        KernelArg::Ptr(d_assign),
        KernelArg::Ptr(d_sumx),
        KernelArg::Ptr(d_sumy),
        KernelArg::Ptr(d_count),
        sc(n as i32),
        sc(k as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (grid, 1, 1),
        (block, 1, 1),
        &args,
    )
    .unwrap();

    let got_assign = bytes_to_i32s(&readback(&mut sink, &ctx, d_assign, n * 4));
    let got_sumx = bytes_to_i32s(&readback(&mut sink, &ctx, d_sumx, k * 4));
    let got_sumy = bytes_to_i32s(&readback(&mut sink, &ctx, d_sumy, k * 4));
    let got_count = bytes_to_i32s(&readback(&mut sink, &ctx, d_count, k * 4));

    // CPU reference: nearest-centroid assignment (lowest index on ties) + per-cluster accumulation.
    let mut want_assign = vec![0i32; n];
    let mut want_sumx = vec![0i32; k];
    let mut want_sumy = vec![0i32; k];
    let mut want_count = vec![0i32; k];
    for i in 0..n {
        let mut best = i32::MAX;
        let mut bestk = 0usize;
        for c in 0..k {
            let dx = px[i] - cx[c];
            let dy = py[i] - cy[c];
            let dist = dx * dx + dy * dy;
            if dist < best {
                best = dist;
                bestk = c;
            }
        }
        want_assign[i] = bestk as i32;
        want_sumx[bestk] += px[i];
        want_sumy[bestk] += py[i];
        want_count[bestk] += 1;
    }
    assert_eq!(
        got_assign, want_assign,
        "k-means point→cluster assignment exact"
    );
    assert_eq!(
        got_sumx, want_sumx,
        "per-cluster x-sum exact (cross-block atomic)"
    );
    assert_eq!(
        got_sumy, want_sumy,
        "per-cluster y-sum exact (cross-block atomic)"
    );
    assert_eq!(got_count, want_count, "per-cluster count exact");
    // Anti-false-pass: every point counted once, and the clustering is genuinely spread (no empty/monopoly).
    assert_eq!(
        got_count.iter().sum::<i32>(),
        n as i32,
        "every point assigned exactly once"
    );
    assert!(
        got_count.iter().all(|&c| c > 0),
        "no empty cluster — assignment is non-degenerate"
    );
}

// ==================================================================================================
// 8. running_minmax — inclusive Hillis-Steele running MIN and running MAX in one pass, computed
//    simultaneously in two `.shared` arrays with a `bar.sync` per doubling step (a read barrier + a write
//    barrier, so no lane reads a slot mid-write). blockDim == N so every lane is live and hits every
//    barrier identically. Exact prefix-min and prefix-max vs the CPU running reductions.
// ==================================================================================================
