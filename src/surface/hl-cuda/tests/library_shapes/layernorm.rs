use super::*;

// ==================================================================================================
// 5. layernorm_stats — the per-row LayerNorm statistics a normalization layer computes, in fixed point.
//    One block per row (block dim = feature count N, a power of two): stage the row, barrier-fenced sum
//    tree → mean = Σ/N (exact arithmetic shift, N a power of two), each lane writes the centered residual
//    x − mean, then a second barrier-fenced tree over the squared residuals → variance = Σ(x−mean)²/N.
//    Centered residuals, per-row mean, and per-row variance are all asserted exact. (The final divide by
//    the standard deviation is the sole float step and is omitted.)
// ==================================================================================================

const LAYERNORM_PTX: &str = r#"
    .visible .entry layernorm_stats(
        .param .u64 p_in,
        .param .u64 p_cent,
        .param .u64 p_mean,
        .param .u64 p_var,
        .param .u32 p_n,
        .param .u32 p_log2n
    )
    {
        .shared .align 4 .b32 sh[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rcent, [p_cent];
        ld.param.u64 %rmean, [p_mean];
        ld.param.u64 %rvar, [p_var];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rlog, [p_log2n];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gcent, %rcent;
        cvta.to.global.u64 %gmean, %rmean;
        cvta.to.global.u64 %gvar, %rvar;
        mov.u32 %tid, %tid.x;
        mov.u32 %row, %ctaid.x;
        mad.lo.s32 %gid, %row, %rn, %tid;
        mul.lo.s32 %toff, %tid, 4;
        mul.wide.s32 %io, %gid, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %x, [%ip];
        mov.u32 %shb, sh;
        add.s32 %saddr, %shb, %toff;
        st.shared.u32 [%saddr], %x;
        bar.sync;
        // sum reduction → mean
        shr.s32 %stride, %rn, 1;
    SUMLOOP:
        setp.le.s32 %pend, %stride, 0;
        @%pend bra SUMDONE;
        setp.lt.s32 %pact, %tid, %stride;
        @!%pact bra SUMAFTER;
        ld.shared.u32 %a, [%saddr];
        add.s32 %j, %tid, %stride;
        mul.lo.s32 %joff, %j, 4;
        add.s32 %jaddr, %shb, %joff;
        ld.shared.u32 %b, [%jaddr];
        add.s32 %a, %a, %b;
        st.shared.u32 [%saddr], %a;
    SUMAFTER:
        bar.sync;
        shr.s32 %stride, %stride, 1;
        bra SUMLOOP;
    SUMDONE:
        ld.shared.u32 %total, [%shb];
        bar.sync;
        shr.s32 %mean, %total, %rlog;
        // centered residual
        sub.s32 %c, %x, %mean;
        add.s64 %cp, %gcent, %io;
        st.global.u32 [%cp], %c;
        // lane 0 records the mean
        setp.ne.s32 %pn0, %tid, 0;
        @%pn0 bra AFTERMEAN;
        mul.wide.s32 %mo, %row, 4;
        add.s64 %mp, %gmean, %mo;
        st.global.u32 [%mp], %mean;
    AFTERMEAN:
        // stage squared residual for the variance reduction
        mul.lo.s32 %sq, %c, %c;
        st.shared.u32 [%saddr], %sq;
        bar.sync;
        shr.s32 %vstride, %rn, 1;
    VLOOP:
        setp.le.s32 %pvend, %vstride, 0;
        @%pvend bra VDONE;
        setp.lt.s32 %pvact, %tid, %vstride;
        @!%pvact bra VAFTER;
        ld.shared.u32 %va, [%saddr];
        add.s32 %vj, %tid, %vstride;
        mul.lo.s32 %vjoff, %vj, 4;
        add.s32 %vjaddr, %shb, %vjoff;
        ld.shared.u32 %vb, [%vjaddr];
        add.s32 %va, %va, %vb;
        st.shared.u32 [%saddr], %va;
    VAFTER:
        bar.sync;
        shr.s32 %vstride, %vstride, 1;
        bra VLOOP;
    VDONE:
        setp.ne.s32 %pn1, %tid, 0;
        @%pn1 bra DONE;
        ld.shared.u32 %sqtotal, [%shb];
        shr.s32 %var, %sqtotal, %rlog;
        mul.wide.s32 %vo, %row, 4;
        add.s64 %vp, %gvar, %vo;
        st.global.u32 [%vp], %var;
    DONE:
        ret;
    }
"#;

#[test]
fn layernorm_stats_fixedpoint_exact() {
    let rows = 5usize;
    let n = 8usize; // power of two
    let log2n = 3u32;
    let input: Vec<i32> = (0..rows * n).map(|i| (i as i32 * 9 + 4) % 50).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(LAYERNORM_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "layernorm_stats").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_cent = alloc_zeroed_i32(&mut sink, &mut ctx, rows * n);
    let d_mean = alloc_zeroed_i32(&mut sink, &mut ctx, rows);
    let d_var = alloc_zeroed_i32(&mut sink, &mut ctx, rows);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_cent),
        KernelArg::Ptr(d_mean),
        KernelArg::Ptr(d_var),
        sc(n as i32),
        sc(log2n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (rows as u32, 1, 1),
        (n as u32, 1, 1),
        &args,
    )
    .unwrap();

    let got_cent = bytes_to_i32s(&readback(&mut sink, &ctx, d_cent, rows * n * 4));
    let got_mean = bytes_to_i32s(&readback(&mut sink, &ctx, d_mean, rows * 4));
    let got_var = bytes_to_i32s(&readback(&mut sink, &ctx, d_var, rows * 4));

    let mut want_cent = vec![0i32; rows * n];
    let mut want_mean = vec![0i32; rows];
    let mut want_var = vec![0i32; rows];
    for r in 0..rows {
        let sum: i32 = (0..n).map(|c| input[r * n + c]).sum();
        let mean = sum >> log2n; // exact arithmetic-shift mean (N a power of two)
        want_mean[r] = mean;
        let mut sq = 0i32;
        for c in 0..n {
            let cent = input[r * n + c] - mean;
            want_cent[r * n + c] = cent;
            sq += cent * cent;
        }
        want_var[r] = sq >> log2n;
    }
    assert_eq!(got_cent, want_cent, "LayerNorm centered residuals exact");
    assert_eq!(got_mean, want_mean, "LayerNorm per-row mean exact");
    assert_eq!(got_var, want_var, "LayerNorm per-row variance exact");
}
