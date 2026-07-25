use super::*;

// ==================================================================================================
// 6. relu_gelu — the two most common activation layers, elementwise over a multi-block grid.
//    ReLU is a branch-selected `max(x,0)`; the GELU-style activation is a fixed-point cubic
//    `((x³ + 20·x) >> 4)` (a monotone smooth-ish approximation), each asserted bit-exact against the
//    identical integer polynomial on CPU (no float tolerance).
// ==================================================================================================

const ACT_PTX: &str = r#"
    .visible .entry relu(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %ip, %gin, %off;
        ld.global.u32 %x, [%ip];
        mov.u32 %y, 0;
        setp.gt.s32 %pp, %x, 0;
        @!%pp bra ST;
        mov.u32 %y, %x;
    ST:
        cvta.to.global.u64 %gout, %rout;
        add.s64 %op, %gout, %off;
        st.global.u32 [%op], %y;
    DONE:
        ret;
    }

    .visible .entry gelu_cubic(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %ip, %gin, %off;
        ld.global.u32 %x, [%ip];
        mul.lo.s32 %x2, %x, %x;
        mul.lo.s32 %x3, %x2, %x;
        // num = x*20 + x3  (== x^3 + 20x)
        mad.lo.s32 %num, %x, 20, %x3;
        shr.s32 %y, %num, 4;
        cvta.to.global.u64 %gout, %rout;
        add.s64 %op, %gout, %off;
        st.global.u32 [%op], %y;
    DONE:
        ret;
    }
"#;

#[test]
fn relu_and_gelu_cubic_exact() {
    let n = 500usize; // multi-block: > one 128-wide block
    let input: Vec<i32> = (0..n).map(|i| (i as i32 % 13) - 6).collect(); // spans [-6, 6]

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(ACT_PTX.as_bytes()).unwrap();
    let relu_fn = load_module::module_get_function(&ctx, module, "relu").unwrap();
    let gelu_fn = load_module::module_get_function(&ctx, module, "gelu_cubic").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_relu = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_gelu = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let grid = n.div_ceil(128) as u32; // 4 blocks × 128

    let args_r = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_relu), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        relu_fn,
        (grid, 1, 1),
        (128, 1, 1),
        &args_r,
    )
    .unwrap();
    let args_g = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_gelu), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        gelu_fn,
        (grid, 1, 1),
        (128, 1, 1),
        &args_g,
    )
    .unwrap();

    let got_relu = bytes_to_i32s(&readback(&mut sink, &ctx, d_relu, n * 4));
    let got_gelu = bytes_to_i32s(&readback(&mut sink, &ctx, d_gelu, n * 4));

    let want_relu: Vec<i32> = input.iter().map(|&x| x.max(0)).collect();
    let want_gelu: Vec<i32> = input
        .iter()
        .map(|&x| {
            let x3 = x.wrapping_mul(x).wrapping_mul(x);
            (x.wrapping_mul(20).wrapping_add(x3)) >> 4
        })
        .collect();
    assert_eq!(got_relu, want_relu, "ReLU elementwise exact");
    assert_eq!(
        got_gelu, want_gelu,
        "fixed-point GELU-cubic elementwise exact"
    );
    assert!(
        want_relu.contains(&0) && want_relu.iter().any(|&v| v > 0),
        "ReLU exercises both sides"
    );
    assert!(
        want_gelu.iter().any(|&v| v < 0),
        "GELU-cubic produces negative outputs too"
    );
}
