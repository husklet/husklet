use super::*;

// ==================================================================================================
// 7. gemv_argmax — matrix–vector `y = A·x` (A is M×N, one thread per output row over a multi-block grid,
//    an `mad`-accumulated N-tap dot product) followed by a large cross-block reduction over y: the max
//    (`red.global.max.s32` into one slot) and the arg-max index (`red.global.min.s32` over exactly the
//    indices whose y equals the max → the LOWEST index, matching the standard tie rule). Exact y, exact
//    max, exact arg-max.
// ==================================================================================================

const GEMV_PTX: &str = r#"
    .visible .entry gemv(
        .param .u64 p_a,
        .param .u64 p_x,
        .param .u64 p_y,
        .param .u32 p_m,
        .param .u32 p_n
    )
    {
        ld.param.u64 %ra, [p_a];
        ld.param.u64 %rx, [p_x];
        ld.param.u64 %ry, [p_y];
        ld.param.u32 %rm, [p_m];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %row, %ct, %nt, %tt;
        setp.ge.s32 %pg, %row, %rm;
        @%pg bra DONE;
        cvta.to.global.u64 %gA, %ra;
        cvta.to.global.u64 %gx, %rx;
        cvta.to.global.u64 %gy, %ry;
        mul.lo.s32 %rowbase, %row, %rn;
        mov.u32 %acc, 0;
        mov.u32 %j, 0;
    JLOOP:
        setp.ge.s32 %pj, %j, %rn;
        @%pj bra JEND;
        add.s32 %aidx, %rowbase, %j;
        mul.wide.s32 %ao, %aidx, 4;
        add.s64 %ap, %gA, %ao;
        ld.global.u32 %av, [%ap];
        mul.wide.s32 %xo, %j, 4;
        add.s64 %xp, %gx, %xo;
        ld.global.u32 %xv, [%xp];
        mad.lo.s32 %acc, %av, %xv, %acc;
        add.s32 %j, %j, 1;
        bra JLOOP;
    JEND:
        mul.wide.s32 %yo, %row, 4;
        add.s64 %yp, %gy, %yo;
        st.global.u32 [%yp], %acc;
    DONE:
        ret;
    }

    .visible .entry reduce_max(
        .param .u64 p_y,
        .param .u64 p_max,
        .param .u32 p_m
    )
    {
        ld.param.u64 %ry, [p_y];
        ld.param.u64 %rmax, [p_max];
        ld.param.u32 %rm, [p_m];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rm;
        @%pg bra DONE;
        cvta.to.global.u64 %gy, %ry;
        mul.wide.s32 %yo, %i, 4;
        add.s64 %yp, %gy, %yo;
        ld.global.u32 %v, [%yp];
        cvta.to.global.u64 %gmax, %rmax;
        red.global.max.s32 [%gmax], %v;
    DONE:
        ret;
    }

    .visible .entry arg_of_max(
        .param .u64 p_y,
        .param .u64 p_max,
        .param .u64 p_idx,
        .param .u32 p_m
    )
    {
        ld.param.u64 %ry, [p_y];
        ld.param.u64 %rmax, [p_max];
        ld.param.u64 %ridx, [p_idx];
        ld.param.u32 %rm, [p_m];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rm;
        @%pg bra DONE;
        cvta.to.global.u64 %gy, %ry;
        mul.wide.s32 %yo, %i, 4;
        add.s64 %yp, %gy, %yo;
        ld.global.u32 %v, [%yp];
        cvta.to.global.u64 %gmax, %rmax;
        ld.global.u32 %mv, [%gmax];
        setp.ne.s32 %pne, %v, %mv;
        @%pne bra DONE;
        cvta.to.global.u64 %gidx, %ridx;
        red.global.min.s32 [%gidx], %i;
    DONE:
        ret;
    }
"#;

#[test]
fn gemv_and_argmax_exact() {
    let (m, n) = (1000usize, 64usize);
    let a: Vec<i32> = (0..m * n)
        .map(|i| (i as i32 * 7 + 1).rem_euclid(11) - 5)
        .collect();
    let x: Vec<i32> = (0..n)
        .map(|i| (i as i32 * 3 + 2).rem_euclid(11) - 5)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(GEMV_PTX.as_bytes()).unwrap();
    let gemv_fn = load_module::module_get_function(&ctx, module, "gemv").unwrap();
    let max_fn = load_module::module_get_function(&ctx, module, "reduce_max").unwrap();
    let arg_fn = load_module::module_get_function(&ctx, module, "arg_of_max").unwrap();

    let d_a = upload(&mut sink, &mut ctx, &i32s_to_bytes(&a));
    let d_x = upload(&mut sink, &mut ctx, &i32s_to_bytes(&x));
    let d_y = alloc_zeroed_i32(&mut sink, &mut ctx, m);
    // max slot seeded to i32::MIN, arg-index slot seeded to M (a sentinel above any real index).
    let d_max = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_max, &i32s_to_bytes(&[i32::MIN])).unwrap();
    let d_idx = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_idx, &i32s_to_bytes(&[m as i32])).unwrap();

    let grid = m.div_ceil(256) as u32; // 4 blocks × 256

    let args_gemv = vec![
        KernelArg::Ptr(d_a),
        KernelArg::Ptr(d_x),
        KernelArg::Ptr(d_y),
        sc(m as i32),
        sc(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        gemv_fn,
        (grid, 1, 1),
        (256, 1, 1),
        &args_gemv,
    )
    .unwrap();

    let args_max = vec![KernelArg::Ptr(d_y), KernelArg::Ptr(d_max), sc(m as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        max_fn,
        (grid, 1, 1),
        (256, 1, 1),
        &args_max,
    )
    .unwrap();

    let args_arg = vec![
        KernelArg::Ptr(d_y),
        KernelArg::Ptr(d_max),
        KernelArg::Ptr(d_idx),
        sc(m as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        arg_fn,
        (grid, 1, 1),
        (256, 1, 1),
        &args_arg,
    )
    .unwrap();

    let got_y = bytes_to_i32s(&readback(&mut sink, &ctx, d_y, m * 4));
    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, 4))[0];
    let got_idx = bytes_to_i32s(&readback(&mut sink, &ctx, d_idx, 4))[0];

    // CPU reference: gemv, then max + lowest-index arg-max.
    let mut want_y = vec![0i32; m];
    for row in 0..m {
        let mut acc = 0i32;
        for j in 0..n {
            acc = acc.wrapping_add(a[row * n + j].wrapping_mul(x[j]));
        }
        want_y[row] = acc;
    }
    let want_max = *want_y.iter().max().unwrap();
    let want_idx = want_y.iter().position(|&v| v == want_max).unwrap() as i32;

    assert_eq!(got_y, want_y, "gemv y = A·x, every row exact");
    assert_eq!(got_max, want_max, "cross-block max reduction exact");
    assert_eq!(got_idx, want_idx, "arg-max (lowest tie index) exact");
    assert_eq!(
        got_y[got_idx as usize], got_max,
        "arg-max index indeed points at the max value"
    );
}
