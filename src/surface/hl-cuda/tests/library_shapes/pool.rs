use super::*;

// ==================================================================================================
// 3. pool2x2 — the cuDNN 2×2 stride-2 pooling shape, computing BOTH max-pool and avg-pool in one pass.
//    Max is a register compare tree (no min/max ALU in the subset); avg is `Σ >> 2` (exact floor of the
//    4-tap mean over the non-negative inputs). One thread per output cell.
// ==================================================================================================

const POOL_PTX: &str = r#"
    .visible .entry pool2x2(
        .param .u64 p_in,
        .param .u64 p_max,
        .param .u64 p_avg,
        .param .u32 p_W,
        .param .u32 p_OH,
        .param .u32 p_OW
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rmax, [p_max];
        ld.param.u64 %ravg, [p_avg];
        ld.param.u32 %rW, [p_W];
        ld.param.u32 %rOH, [p_OH];
        ld.param.u32 %rOW, [p_OW];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gmax, %rmax;
        cvta.to.global.u64 %gavg, %ravg;
        mov.u32 %ox, %tid.x;
        mov.u32 %oy, %tid.y;
        setp.ge.s32 %pxo, %ox, %rOW;
        @%pxo bra DONE;
        setp.ge.s32 %pyo, %oy, %rOH;
        @%pyo bra DONE;
        shl.b32 %ix, %ox, 1;
        shl.b32 %iy, %oy, 1;
        // v0 = in[iy*W + ix]
        mad.lo.s32 %r0, %iy, %rW, %ix;
        mul.wide.s32 %o0, %r0, 4;
        add.s64 %p0, %gin, %o0;
        ld.global.u32 %v0, [%p0];
        // v1 = in[iy*W + ix + 1]
        ld.global.u32 %v1, [%p0+4];
        // v2 = in[(iy+1)*W + ix]
        add.s32 %iy1, %iy, 1;
        mad.lo.s32 %r2, %iy1, %rW, %ix;
        mul.wide.s32 %o2, %r2, 4;
        add.s64 %p2, %gin, %o2;
        ld.global.u32 %v2, [%p2];
        ld.global.u32 %v3, [%p2+4];
        // max = max(v0,v1,v2,v3) via a compare tree
        mov.u32 %mx, %v0;
        setp.gt.s32 %m1, %v1, %mx;
        @!%m1 bra M1;
        mov.u32 %mx, %v1;
    M1:
        setp.gt.s32 %m2, %v2, %mx;
        @!%m2 bra M2;
        mov.u32 %mx, %v2;
    M2:
        setp.gt.s32 %m3, %v3, %mx;
        @!%m3 bra M3;
        mov.u32 %mx, %v3;
    M3:
        // avg = (v0+v1+v2+v3) >> 2
        add.s32 %s, %v0, %v1;
        add.s32 %s, %s, %v2;
        add.s32 %s, %s, %v3;
        shr.s32 %av, %s, 2;
        mad.lo.s32 %oidx, %oy, %rOW, %ox;
        mul.wide.s32 %oo, %oidx, 4;
        add.s64 %pmo, %gmax, %oo;
        st.global.u32 [%pmo], %mx;
        add.s64 %pao, %gavg, %oo;
        st.global.u32 [%pao], %av;
    DONE:
        ret;
    }
"#;

#[test]
fn pool2x2_max_and_avg_exact() {
    let (h, w) = (8usize, 8usize);
    let (oh, ow) = (h / 2, w / 2);
    let img: Vec<i32> = (0..h * w).map(|i| (i as i32 * 13 + 7) % 97).collect(); // non-negative

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(POOL_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "pool2x2").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&img));
    let d_max = alloc_zeroed_i32(&mut sink, &mut ctx, oh * ow);
    let d_avg = alloc_zeroed_i32(&mut sink, &mut ctx, oh * ow);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_max),
        KernelArg::Ptr(d_avg),
        sc(w as i32),
        sc(oh as i32),
        sc(ow as i32),
    ];
    launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (4, 4, 1), &args).unwrap();

    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, oh * ow * 4));
    let got_avg = bytes_to_i32s(&readback(&mut sink, &ctx, d_avg, oh * ow * 4));

    let mut want_max = vec![0i32; oh * ow];
    let mut want_avg = vec![0i32; oh * ow];
    for oy in 0..oh {
        for ox in 0..ow {
            let (ix, iy) = (ox * 2, oy * 2);
            let v0 = img[iy * w + ix];
            let v1 = img[iy * w + ix + 1];
            let v2 = img[(iy + 1) * w + ix];
            let v3 = img[(iy + 1) * w + ix + 1];
            want_max[oy * ow + ox] = v0.max(v1).max(v2).max(v3);
            want_avg[oy * ow + ox] = (v0 + v1 + v2 + v3) >> 2;
        }
    }
    assert_eq!(got_max, want_max, "2×2 max-pool exact");
    assert_eq!(got_avg, want_avg, "2×2 avg-pool (floor mean) exact");
}
