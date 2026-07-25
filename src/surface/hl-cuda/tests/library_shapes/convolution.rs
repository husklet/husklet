use super::*;

// ==================================================================================================
// 2. conv2d_nchw — a cuDNN convolution layer: input [N=1, C_in=3, H, W], weights [C_out=4, C_in=3, 3, 3],
//    NCHW layout, valid padding → output [1, C_out=4, H−2, W−2]. Each thread owns one output pixel of one
//    output channel (`ctaid.z` = out channel); it accumulates the full C_in·3·3 = 27-tap dot product with
//    real NCHW strides. A dropped channel or a transposed weight index would fail element-exact.
// ==================================================================================================

const CONV2D_NCHW_PTX: &str = r#"
    .visible .entry conv2d_nchw(
        .param .u64 p_in,
        .param .u64 p_w,
        .param .u64 p_out,
        .param .u32 p_H,
        .param .u32 p_W,
        .param .u32 p_OH,
        .param .u32 p_OW,
        .param .u32 p_Cin
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rw, [p_w];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rH, [p_H];
        ld.param.u32 %rW, [p_W];
        ld.param.u32 %rOH, [p_OH];
        ld.param.u32 %rOW, [p_OW];
        ld.param.u32 %rCin, [p_Cin];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gw, %rw;
        cvta.to.global.u64 %gout, %rout;
        mov.u32 %ox, %tid.x;
        mov.u32 %oy, %tid.y;
        mov.u32 %oc, %ctaid.z;
        setp.ge.s32 %px, %ox, %rOW;
        @%px bra DONE;
        setp.ge.s32 %py, %oy, %rOH;
        @%py bra DONE;
        mov.u32 %acc, 0;
        mov.u32 %ic, 0;
    ICLOOP:
        setp.ge.s32 %pic, %ic, %rCin;
        @%pic bra ICEND;
        mov.u32 %ky, 0;
    KYLOOP:
        setp.gt.s32 %pky, %ky, 2;
        @%pky bra KYEND;
        add.s32 %iy, %oy, %ky;
        mov.u32 %kx, 0;
    KXLOOP:
        setp.gt.s32 %pkx, %kx, 2;
        @%pkx bra KXEND;
        add.s32 %ix, %ox, %kx;
        // in index = (ic*H + iy)*W + ix
        mad.lo.s32 %tmp, %ic, %rH, %iy;
        mad.lo.s32 %iidx, %tmp, %rW, %ix;
        mul.wide.s32 %ioff, %iidx, 4;
        add.s64 %ip, %gin, %ioff;
        ld.global.u32 %pv, [%ip];
        // w index = ((oc*Cin + ic)*3 + ky)*3 + kx
        mad.lo.s32 %w1, %oc, %rCin, %ic;
        mad.lo.s32 %w2, %w1, 3, %ky;
        mad.lo.s32 %w3, %w2, 3, %kx;
        mul.wide.s32 %woff, %w3, 4;
        add.s64 %wp, %gw, %woff;
        ld.global.u32 %wv, [%wp];
        mad.lo.s32 %acc, %pv, %wv, %acc;
        add.s32 %kx, %kx, 1;
        bra KXLOOP;
    KXEND:
        add.s32 %ky, %ky, 1;
        bra KYLOOP;
    KYEND:
        add.s32 %ic, %ic, 1;
        bra ICLOOP;
    ICEND:
        // out index = (oc*OH + oy)*OW + ox
        mad.lo.s32 %o1, %oc, %rOH, %oy;
        mad.lo.s32 %oidx, %o1, %rOW, %ox;
        mul.wide.s32 %oo, %oidx, 4;
        add.s64 %op, %gout, %oo;
        st.global.u32 [%op], %acc;
    DONE:
        ret;
    }
"#;

#[test]
fn conv2d_nchw_multichannel_exact() {
    let (cin, cout) = (3usize, 4usize);
    let (h, w) = (8usize, 8usize);
    let (oh, ow) = (h - 2, w - 2); // valid padding, 3×3 → 6×6
    let input: Vec<i32> = (0..cin * h * w).map(|i| (i as i32 * 3 + 1) % 10).collect();
    let weight: Vec<i32> = (0..cout * cin * 9)
        .map(|i| (i as i32 * 7 + 2) % 9 - 4)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(CONV2D_NCHW_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "conv2d_nchw").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_w = upload(&mut sink, &mut ctx, &i32s_to_bytes(&weight));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, cout * oh * ow);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_w),
        KernelArg::Ptr(d_out),
        sc(h as i32),
        sc(w as i32),
        sc(oh as i32),
        sc(ow as i32),
        sc(cin as i32),
    ];
    // block (8,8) covers the 6×6 output (guarded); grid.z = C_out.
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, cout as u32),
        (8, 8, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, cout * oh * ow * 4));

    // CPU reference: NCHW valid convolution, exact multi-channel accumulation.
    let mut want = vec![0i32; cout * oh * ow];
    for oc in 0..cout {
        for oy in 0..oh {
            for ox in 0..ow {
                let mut acc = 0i32;
                for ic in 0..cin {
                    for ky in 0..3 {
                        for kx in 0..3 {
                            let iv = input[(ic * h + (oy + ky)) * w + (ox + kx)];
                            let wv = weight[((oc * cin + ic) * 3 + ky) * 3 + kx];
                            acc = acc.wrapping_add(iv.wrapping_mul(wv));
                        }
                    }
                }
                want[(oc * oh + oy) * ow + ox] = acc;
            }
        }
    }
    assert_eq!(got, want, "NCHW 3×3 valid conv, every output element exact");
    assert!(
        want.iter().any(|&v| v != 0),
        "reference must be non-degenerate"
    );
}
