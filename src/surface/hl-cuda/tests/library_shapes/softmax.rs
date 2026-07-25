use super::*;

// ==================================================================================================
// 4. softmax_rowwise — a numerically-stable per-row softmax in fixed point. One block per row (block dim
//    = number of columns, a power of two); the row is staged into `.shared`, a barrier-fenced compare
//    tree finds the row max, each lane forms the base-2 fixed-point weight `w = (1<<Q) >> (max − x)`
//    (the max-subtraction is the stability trick, an exact right shift), and a second barrier-fenced
//    tree sums the weights into the row denominator. Weights AND denominators are asserted exact; the
//    final normalize-divide (`w / Σ`) is the sole floating-point step and is intentionally omitted.
// ==================================================================================================

const SOFTMAX_PTX: &str = r#"
    .visible .entry softmax_rows(
        .param .u64 p_in,
        .param .u64 p_w,
        .param .u64 p_sum,
        .param .u32 p_cols
    )
    {
        .shared .align 4 .b32 sh[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rw, [p_w];
        ld.param.u64 %rsum, [p_sum];
        ld.param.u32 %rcols, [p_cols];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gw, %rw;
        cvta.to.global.u64 %gsum, %rsum;
        mov.u32 %tid, %tid.x;
        mov.u32 %row, %ctaid.x;
        mad.lo.s32 %gid, %row, %rcols, %tid;
        mul.lo.s32 %toff, %tid, 4;
        // stage x into shared
        mul.wide.s32 %io, %gid, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %x, [%ip];
        mov.u32 %shb, sh;
        add.s32 %saddr, %shb, %toff;
        st.shared.u32 [%saddr], %x;
        bar.sync;
        // max reduction over shared
        shr.s32 %stride, %rcols, 1;
    MAXLOOP:
        setp.le.s32 %pend, %stride, 0;
        @%pend bra MAXDONE;
        setp.lt.s32 %pact, %tid, %stride;
        @!%pact bra MAXAFTER;
        ld.shared.u32 %a, [%saddr];
        add.s32 %jidx, %tid, %stride;
        mul.lo.s32 %joff, %jidx, 4;
        add.s32 %jaddr, %shb, %joff;
        ld.shared.u32 %bv, [%jaddr];
        setp.gt.s32 %pg, %bv, %a;
        @!%pg bra MAXAFTER;
        st.shared.u32 [%saddr], %bv;
    MAXAFTER:
        bar.sync;
        shr.s32 %stride, %stride, 1;
        bra MAXLOOP;
    MAXDONE:
        ld.shared.u32 %m, [%shb];
        bar.sync;
        // w = (1<<16) >> (m - x)
        sub.s32 %shift, %m, %x;
        mov.u32 %one, 65536;
        shr.u32 %w, %one, %shift;
        add.s64 %wp, %gw, %io;
        st.global.u32 [%wp], %w;
        // stage w for the sum reduction
        st.shared.u32 [%saddr], %w;
        bar.sync;
        shr.s32 %sstride, %rcols, 1;
    SUMLOOP:
        setp.le.s32 %psend, %sstride, 0;
        @%psend bra SUMDONE;
        setp.lt.s32 %psact, %tid, %sstride;
        @!%psact bra SUMAFTER;
        ld.shared.u32 %sa, [%saddr];
        add.s32 %sj, %tid, %sstride;
        mul.lo.s32 %sjoff, %sj, 4;
        add.s32 %sjaddr, %shb, %sjoff;
        ld.shared.u32 %sb, [%sjaddr];
        add.s32 %sa, %sa, %sb;
        st.shared.u32 [%saddr], %sa;
    SUMAFTER:
        bar.sync;
        shr.s32 %sstride, %sstride, 1;
        bra SUMLOOP;
    SUMDONE:
        setp.ne.s32 %pnl, %tid, 0;
        @%pnl bra DONE;
        ld.shared.u32 %total, [%shb];
        mul.wide.s32 %so, %row, 4;
        add.s64 %sp, %gsum, %so;
        st.global.u32 [%sp], %total;
    DONE:
        ret;
    }
"#;

#[test]
fn softmax_rowwise_fixedpoint_exact() {
    let rows = 6usize;
    let cols = 8usize; // power of two for the reduction tree
    const Q: u32 = 16;
    // Bounded so (max − x) stays well under 32 (fixed-point base-2 exp shift domain).
    let input: Vec<i32> = (0..rows * cols).map(|i| (i as i32 * 5 + 3) % 19).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SOFTMAX_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "softmax_rows").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_w = alloc_zeroed_i32(&mut sink, &mut ctx, rows * cols);
    let d_sum = alloc_zeroed_i32(&mut sink, &mut ctx, rows);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_w),
        KernelArg::Ptr(d_sum),
        sc(cols as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (rows as u32, 1, 1),
        (cols as u32, 1, 1),
        &args,
    )
    .unwrap();

    let got_w = bytes_to_i32s(&readback(&mut sink, &ctx, d_w, rows * cols * 4));
    let got_sum = bytes_to_i32s(&readback(&mut sink, &ctx, d_sum, rows * 4));

    // CPU reference: stable base-2 fixed-point softmax weights + row denominators.
    let mut want_w = vec![0i32; rows * cols];
    let mut want_sum = vec![0i32; rows];
    for r in 0..rows {
        let m = (0..cols).map(|c| input[r * cols + c]).max().unwrap();
        let mut s = 0i32;
        for c in 0..cols {
            let shift = (m - input[r * cols + c]) as u32; // >= 0
            let w = (1u32 << Q).wrapping_shr(shift) as i32; // wrapping_shr masks to &31, matching the interpreter
            want_w[r * cols + c] = w;
            s += w;
        }
        want_sum[r] = s;
    }
    assert_eq!(got_w, want_w, "row-softmax fixed-point weights exact");
    assert_eq!(got_sum, want_sum, "row-softmax denominators exact");
    // The lane holding the row max must weight exactly 1<<Q (its shift is 0) — the stability anchor.
    for r in 0..rows {
        assert!(
            want_w[r * cols..(r + 1) * cols].contains(&(1i32 << Q)),
            "row max lane weights 1<<Q"
        );
    }
}
