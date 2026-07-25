use super::*;

const PREDICATE_PTX: &str = r#"
    .visible .entry predicate(
        .param .u64 p_in,
        .param .u64 p_flag,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rflag, [p_flag];
        ld.param.u32 %rn, [p_n];
        mad.lo.s32 %i, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %i, %rn;
        @%pdone bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %o, %i, 4;
        add.s64 %ip, %gin, %o;
        ld.global.u32 %v, [%ip];
        // flag = (v & 1) == 0 ? 1 : 0   (keep evens)
        and.b32 %lsb, %v, 1;
        mov.u32 %flag, 0;
        setp.ne.s32 %podd, %lsb, 0;
        @%podd bra WRITE;
        mov.u32 %flag, 1;
    WRITE:
        cvta.to.global.u64 %gflag, %rflag;
        add.s64 %fp, %gflag, %o;
        st.global.u32 [%fp], %flag;
    DONE:
        ret;
    }
"#;

// Per-block inclusive Hillis-Steele scan (+ block totals) — the gpgpu_patterns::prefix_scan skeleton.
const SCAN_PTX: &str = r#"
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
        ld.shared.u32 %res, [%toff];
        setp.ge.s32 %poob2, %gid, %rn;
        @%poob2 bra MAYBESUM;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %ooff, %gid, 4;
        add.s64 %optr, %gout, %ooff;
        st.global.u32 [%optr], %res;
    MAYBESUM:
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

const SCATTER_PTX: &str = r#"
    .visible .entry scatter(
        .param .u64 p_in,
        .param .u64 p_flag,
        .param .u64 p_incl,
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rflag, [p_flag];
        ld.param.u64 %rincl, [p_incl];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rn, [p_n];
        mad.lo.s32 %i, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %i, %rn;
        @%pdone bra DONE;
        cvta.to.global.u64 %gflag, %rflag;
        mul.wide.s32 %o, %i, 4;
        add.s64 %fp, %gflag, %o;
        ld.global.u32 %f, [%fp];
        setp.eq.s32 %pskip, %f, 0;
        @%pskip bra DONE;
        // pos = incl[i] - 1
        cvta.to.global.u64 %gincl, %rincl;
        add.s64 %sp, %gincl, %o;
        ld.global.u32 %s, [%sp];
        sub.s32 %pos, %s, 1;
        // out[pos] = in[i]
        cvta.to.global.u64 %gin, %rin;
        add.s64 %ip, %gin, %o;
        ld.global.u32 %v, [%ip];
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %po, %pos, 4;
        add.s64 %op, %gout, %po;
        st.global.u32 [%op], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn stream_compaction_predicate_scan_scatter_exact() {
    let block = 256usize;
    let n = 1000usize; // multi-block, non-multiple of block → partial last block
    let grid = n.div_ceil(block); // 4 blocks
    let input: Vec<i32> = (0..n).map(|i| (i as i32 * 37 + 11) % 100).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let m_pred = ctx.load_module(PREDICATE_PTX.as_bytes()).unwrap();
    let pred_fn = load_module::module_get_function(&ctx, m_pred, "predicate").unwrap();
    let m_scan = ctx.load_module(SCAN_PTX.as_bytes()).unwrap();
    let scan_fn = load_module::module_get_function(&ctx, m_scan, "block_scan").unwrap();
    let add_fn = load_module::module_get_function(&ctx, m_scan, "add_offset").unwrap();
    let m_scat = ctx.load_module(SCATTER_PTX.as_bytes()).unwrap();
    let scat_fn = load_module::module_get_function(&ctx, m_scat, "scatter").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_flag = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_incl = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_sums = alloc_zeroed_i32(&mut sink, &mut ctx, grid);
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, n);

    // (a) predicate → flags
    let args_p = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_flag), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        pred_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args_p,
    )
    .unwrap();

    // (b) inclusive scan of flags → incl (per-block scan, host offsets, add_offset)
    let args_s = vec![
        KernelArg::Ptr(d_flag),
        KernelArg::Ptr(d_incl),
        KernelArg::Ptr(d_sums),
        sc(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        scan_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args_s,
    )
    .unwrap();
    let sums = bytes_to_i32s(&readback(&mut sink, &ctx, d_sums, grid * 4));
    let mut offsets = vec![0i32; grid];
    let mut running = 0i32;
    for b in 0..grid {
        offsets[b] = running;
        running += sums[b];
    }
    let d_off = upload(&mut sink, &mut ctx, &i32s_to_bytes(&offsets));
    let args_a = vec![KernelArg::Ptr(d_incl), KernelArg::Ptr(d_off), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        add_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args_a,
    )
    .unwrap();

    // (c) scatter kept elements to their scanned positions
    let args_c = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_flag),
        KernelArg::Ptr(d_incl),
        KernelArg::Ptr(d_out),
        sc(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        scat_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args_c,
    )
    .unwrap();

    let incl = bytes_to_i32s(&readback(&mut sink, &ctx, d_incl, n * 4));
    let count = incl[n - 1] as usize;
    let out = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, n * 4));

    // CPU reference: filter evens, preserving order.
    let want: Vec<i32> = input.iter().copied().filter(|v| v & 1 == 0).collect();
    assert_eq!(count, want.len(), "compacted count exact");
    assert_eq!(
        &out[..count],
        &want[..],
        "compacted prefix equals the filtered elements, in order"
    );
    // Anti-false-pass: the predicate actually filtered (some kept, some dropped).
    assert!(
        count > 0 && count < n,
        "predicate is non-trivial: 0 < count < n"
    );
    assert!(
        out[..count].iter().all(|v| v & 1 == 0),
        "every compacted element passes the predicate"
    );
}

// ==================================================================================================
// 6. monte_carlo_pi — deterministic per-thread LCG, count-in-quarter-circle. Thread t seeds an LCG from
//    its GLOBAL index (NO clock, NO device rng), draws M samples: two LCG steps give (x,y) in
//    [0, 2^12)^2 (top 12 bits of each 32-bit state), and a sample is a hit when `x²+y² ≤ R²`
//    (R = 2^12−1, so x²+y² ≤ ~3.4e7 stays well inside i32). Each thread accumulates its local hit count,
//    then one `red.global.add` folds it into a global counter. The result is a deterministic INTEGER hit
//    count — asserted bit-exact against the identical LCG replayed on CPU (this is NOT a statistical π).
// ==================================================================================================
