use super::*;

const RUNNING_MINMAX_PTX: &str = r#"
    .visible .entry running_minmax(
        .param .u64 p_in,
        .param .u64 p_omin,
        .param .u64 p_omax
    )
    {
        .shared .align 4 .b32 smin[256];
        .shared .align 4 .b32 smax[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %romin, [p_omin];
        ld.param.u64 %romax, [p_omax];
        mov.u32 %tid, %tid.x;
        mov.u32 %bd, %ntid.x;
        mul.lo.s32 %toff, %tid, 4;
        mov.u32 %sminb, smin;
        mov.u32 %smaxb, smax;
        add.s32 %minaddr, %sminb, %toff;
        add.s32 %maxaddr, %smaxb, %toff;
        // load in[tid] into both shared arrays
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %io, %tid, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %v, [%ip];
        st.shared.u32 [%minaddr], %v;
        st.shared.u32 [%maxaddr], %v;
        bar.sync;
        mov.u32 %d, 1;
    DLOOP:
        setp.ge.s32 %pd, %d, %bd;
        @%pd bra ENDD;
        // current values
        ld.shared.u32 %curmin, [%minaddr];
        ld.shared.u32 %curmax, [%maxaddr];
        // if tid >= d, combine with the value d slots back
        setp.lt.s32 %plt, %tid, %d;
        @%plt bra HAVE;
        sub.s32 %jidx, %tid, %d;
        mul.lo.s32 %joff, %jidx, 4;
        add.s32 %jminaddr, %sminb, %joff;
        add.s32 %jmaxaddr, %smaxb, %joff;
        ld.shared.u32 %pmin, [%jminaddr];
        ld.shared.u32 %pmax, [%jmaxaddr];
        // curmin = min(curmin, pmin)
        setp.le.s32 %pkeepmin, %curmin, %pmin;
        @%pkeepmin bra MAXCMP;
        mov.u32 %curmin, %pmin;
    MAXCMP:
        // curmax = max(curmax, pmax)
        setp.ge.s32 %pkeepmax, %curmax, %pmax;
        @%pkeepmax bra HAVE;
        mov.u32 %curmax, %pmax;
    HAVE:
        bar.sync;
        st.shared.u32 [%minaddr], %curmin;
        st.shared.u32 [%maxaddr], %curmax;
        bar.sync;
        shl.b32 %d, %d, 1;
        bra DLOOP;
    ENDD:
        ld.shared.u32 %rmin, [%minaddr];
        ld.shared.u32 %rmax, [%maxaddr];
        cvta.to.global.u64 %gomin, %romin;
        cvta.to.global.u64 %gomax, %romax;
        add.s64 %ominp, %gomin, %io;
        st.global.u32 [%ominp], %rmin;
        add.s64 %omaxp, %gomax, %io;
        st.global.u32 [%omaxp], %rmax;
        ret;
    }
"#;

#[test]
fn running_minmax_prefix_scan_exact() {
    let n = 200usize; // single block, blockDim == N; NOT a power of two (Hillis-Steele still exact)
                      // Signed values that wander up and down so the running min/max genuinely change over the sequence.
                      // Index 0 is deliberately a moderate value; the deep dip / high spike land later, so the running
                      // min strictly decreases and the running max strictly increases past the start (anti-false-pass).
    let input: Vec<i32> = (0..n)
        .map(|i| {
            let t = i as i32;
            let base = ((t * 37 + 50) % 191) - 95; // in [-95, 95], value 0 at t=0 is ~-45
            let dip = if t % 13 == 6 { -40 } else { 0 };
            let spike = if t % 17 == 3 { 40 } else { 0 };
            base + dip + spike
        })
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(RUNNING_MINMAX_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "running_minmax").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_omin = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_omax = alloc_zeroed_i32(&mut sink, &mut ctx, n);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_omin),
        KernelArg::Ptr(d_omax),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, 1),
        (n as u32, 1, 1),
        &args,
    )
    .unwrap();

    let got_min = bytes_to_i32s(&readback(&mut sink, &ctx, d_omin, n * 4));
    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_omax, n * 4));

    // CPU reference: inclusive running min / max.
    let mut want_min = vec![0i32; n];
    let mut want_max = vec![0i32; n];
    let mut cmin = i32::MAX;
    let mut cmax = i32::MIN;
    for i in 0..n {
        cmin = cmin.min(input[i]);
        cmax = cmax.max(input[i]);
        want_min[i] = cmin;
        want_max[i] = cmax;
    }
    assert_eq!(got_min, want_min, "inclusive running minimum exact");
    assert_eq!(got_max, want_max, "inclusive running maximum exact");
    // Anti-false-pass: the scans are monotone and actually move (not a constant array).
    assert!(
        want_min.windows(2).all(|w| w[1] <= w[0]),
        "running min is non-increasing"
    );
    assert!(
        want_max.windows(2).all(|w| w[1] >= w[0]),
        "running max is non-decreasing"
    );
    assert!(
        want_min[n - 1] < want_min[0],
        "running min genuinely decreased over the sequence"
    );
    assert!(
        want_max[n - 1] > want_max[0],
        "running max genuinely increased over the sequence"
    );
    assert_eq!(
        want_min[n - 1],
        *input.iter().min().unwrap(),
        "final running min = global min"
    );
    assert_eq!(
        want_max[n - 1],
        *input.iter().max().unwrap(),
        "final running max = global max"
    );
}
