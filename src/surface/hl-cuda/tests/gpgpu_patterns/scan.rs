use super::*;

// ==================================================================================================
// 2. prefix_scan — multi-block inclusive scan. Each block does a Hillis-Steele inclusive scan of its
//    slice in `.shared` (a read barrier + a write barrier per doubling step, so no thread reads a slot
//    another is mid-writing), writes the block total to `sums[block]`; the host exclusively-scans the
//    block totals; a second kernel adds each block's offset. Result = the global inclusive scan.
// ==================================================================================================

const BLOCK_SCAN_PTX: &str = r#"
    .visible .entry block_scan(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u64 p_sums,
        .param .u32 p_n
    )
    {
        .shared .align 4 .b32 sh[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u64 %rsums, [p_sums];
        ld.param.u32 %rn, [p_n];
        mov.u32 %tid, %tid.x;
        mov.u32 %bd, %ntid.x;
        mov.u32 %cx, %ctaid.x;
        mad.lo.s32 %gid, %cx, %bd, %tid;
        mul.lo.s32 %toff, %tid, 4;
        // val = (gid < n) ? in[gid] : 0
        mov.u32 %val, 0;
        setp.ge.s32 %poob, %gid, %rn;
        @%poob bra STORE0;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %ioff, %gid, 4;
        add.s64 %iptr, %gin, %ioff;
        ld.global.u32 %val, [%iptr];
    STORE0:
        st.shared.u32 [%toff], %val;
        bar.sync;
        mov.u32 %d, 1;
    DLOOP:
        setp.ge.s32 %pd, %d, %bd;
        @%pd bra ENDD;
        // read phase: t = (tid >= d) ? sh[tid] + sh[tid-d] : sh[tid]
        ld.shared.u32 %tcur, [%toff];
        setp.lt.s32 %plt, %tid, %d;
        @%plt bra HAVE;
        sub.s32 %jidx, %tid, %d;
        mul.lo.s32 %joff, %jidx, 4;
        ld.shared.u32 %tprev, [%joff];
        add.s32 %tcur, %tcur, %tprev;
    HAVE:
        bar.sync;
        st.shared.u32 [%toff], %tcur;
        bar.sync;
        shl.b32 %d, %d, 1;
        bra DLOOP;
    ENDD:
        // out[gid] = sh[tid] for in-range lanes
        ld.shared.u32 %res, [%toff];
        setp.ge.s32 %poob2, %gid, %rn;
        @%poob2 bra MAYBESUM;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %ooff, %gid, 4;
        add.s64 %optr, %gout, %ooff;
        st.global.u32 [%optr], %res;
    MAYBESUM:
        // last lane writes the block total (sh[bd-1]) to sums[blockIdx]
        sub.s32 %last, %bd, 1;
        setp.ne.s32 %pnl, %tid, %last;
        @%pnl bra DONE;
        cvta.to.global.u64 %gsums, %rsums;
        mul.wide.s32 %soff2, %cx, 4;
        add.s64 %sptr, %gsums, %soff2;
        st.global.u32 [%sptr], %res;
    DONE:
        ret;
    }

    .visible .entry add_offset(
        .param .u64 p_out,
        .param .u64 p_off,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rout, [p_out];
        ld.param.u64 %roff, [p_off];
        ld.param.u32 %rn, [p_n];
        mov.u32 %tid, %tid.x;
        mov.u32 %bd, %ntid.x;
        mov.u32 %cx, %ctaid.x;
        mad.lo.s32 %gid, %cx, %bd, %tid;
        setp.ge.s32 %poob, %gid, %rn;
        @%poob bra DONE;
        cvta.to.global.u64 %goff, %roff;
        mul.wide.s32 %co, %cx, 4;
        add.s64 %offp, %goff, %co;
        ld.global.u32 %delta, [%offp];
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %go, %gid, 4;
        add.s64 %outp, %gout, %go;
        ld.global.u32 %v, [%outp];
        add.s32 %v, %v, %delta;
        st.global.u32 [%outp], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn prefix_scan_inclusive_and_exclusive_exact() {
    let block = 256usize;
    let n = 1000usize; // NOT a multiple of the block → last block is partial (remainder handling)
    let grid = (n + block - 1) / block; // 4 blocks
    let input: Vec<i32> = (0..n).map(|i| (i as i32 % 7) + 1).collect(); // 1..=7, sum ≤ 7000 (no overflow)

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(BLOCK_SCAN_PTX.as_bytes()).unwrap();
    let scan_fn = load_module::module_get_function(&ctx, module, "block_scan").unwrap();
    let add_fn = load_module::module_get_function(&ctx, module, "add_offset").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_sums = alloc_zeroed_i32(&mut sink, &mut ctx, grid);

    // Phase 1: per-block inclusive scan + block totals.
    let args1 = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_out),
        KernelArg::Ptr(d_sums),
        sc(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        scan_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args1,
    )
    .unwrap();

    // Phase 2 (host): exclusive scan of the per-block totals → per-block offsets.
    let sums = bytes_to_i32s(&readback(&mut sink, &ctx, d_sums, grid * 4));
    let mut offsets = vec![0i32; grid];
    let mut running = 0i32;
    for b in 0..grid {
        offsets[b] = running;
        running += sums[b];
    }
    let d_off = upload(&mut sink, &mut ctx, &i32s_to_bytes(&offsets));

    // Phase 3: add each block's offset into its elements → global inclusive scan.
    let args3 = vec![KernelArg::Ptr(d_out), KernelArg::Ptr(d_off), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        add_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args3,
    )
    .unwrap();

    let got_inclusive = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, n * 4));

    // Reference inclusive + exclusive scans.
    let mut want_incl = vec![0i32; n];
    let mut acc = 0i32;
    for i in 0..n {
        acc += input[i];
        want_incl[i] = acc;
    }
    assert_eq!(
        got_inclusive, want_incl,
        "multi-block inclusive prefix scan, every element exact"
    );

    // Exclusive scan derived from the device inclusive result: excl[i] = incl[i] - input[i].
    let got_exclusive: Vec<i32> = (0..n).map(|i| got_inclusive[i] - input[i]).collect();
    let mut want_excl = vec![0i32; n];
    let mut e = 0i32;
    for i in 0..n {
        want_excl[i] = e;
        e += input[i];
    }
    assert_eq!(
        got_exclusive, want_excl,
        "exclusive prefix scan, every element exact"
    );
    assert_eq!(
        want_incl[n - 1],
        input.iter().sum::<i32>(),
        "final inclusive = total sum"
    );
    assert_eq!(
        sink.executor().dispatches,
        2,
        "two kernel dispatches: block-scan + add-offset"
    );
}
