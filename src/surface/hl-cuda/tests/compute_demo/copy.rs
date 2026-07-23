use super::*;

// ==================================================================================================
// 5. strided / 2D copy — extract a sub-rectangle from a wider row-major source into a packed dst.
// ==================================================================================================

/// `dst[y*rw + x] = src[(r0+y)*W + (c0+x)]` for `(x,y)` in the sub-rect, over a 2D block/grid.
const SUBRECT_PTX: &str = r#"
    .visible .entry subrect(
        .param .u64 sr_src,
        .param .u64 sr_dst,
        .param .u32 sr_w,
        .param .u32 sr_r0,
        .param .u32 sr_c0,
        .param .u32 sr_rw,
        .param .u32 sr_rh
    )
    {
        ld.param.u64  %rsrc, [sr_src];
        ld.param.u64  %rdst, [sr_dst];
        ld.param.u32  %rw, [sr_w];
        ld.param.u32  %rr0, [sr_r0];
        ld.param.u32  %rc0, [sr_c0];
        ld.param.u32  %rrw, [sr_rw];
        ld.param.u32  %rrh, [sr_rh];
        mov.u32 %rnx, %ntid.x; mov.u32 %rcx, %ctaid.x; mov.u32 %rtx, %tid.x;
        mad.lo.s32 %x, %rcx, %rnx, %rtx;
        mov.u32 %rny, %ntid.y; mov.u32 %rcy, %ctaid.y; mov.u32 %rty, %tid.y;
        mad.lo.s32 %y, %rcy, %rny, %rty;
        setp.ge.s32 %px, %x, %rrw;
        @%px bra DONE;
        setp.ge.s32 %py, %y, %rrh;
        @%py bra DONE;
        add.s32 %srow, %rr0, %y;
        add.s32 %scol, %rc0, %x;
        mad.lo.s32 %sidx, %srow, %rw, %scol;
        mad.lo.s32 %didx, %y, %rrw, %x;
        cvta.to.global.u64 %gsrc, %rsrc;
        cvta.to.global.u64 %gdst, %rdst;
        mul.wide.s32 %soff, %sidx, 4;
        mul.wide.s32 %doff, %didx, 4;
        add.s64 %sp, %gsrc, %soff;
        add.s64 %dp, %gdst, %doff;
        ld.global.u32 %v, [%sp];
        st.global.u32 [%dp], %v;
    DONE: ret;
    }
"#;

#[test]
fn strided_subrect_copy_exact() {
    let (w, h) = (8usize, 6usize);
    let (r0, c0, rw, rh) = (1usize, 2usize, 4usize, 3usize);
    // Distinct value per source cell = row*W + col, so any mis-index is caught.
    let src: Vec<i32> = (0..w * h).map(|i| i as i32).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SUBRECT_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "subrect").unwrap();

    let d_src = upload(&mut sink, &mut ctx, &i32s_to_bytes(&src));
    let d_dst = allocate::mem_alloc(&mut ctx, &mut sink, (rw * rh * 4) as u64).unwrap();

    let sc = |v: usize| KernelArg::Scalar((v as i32).to_le_bytes().to_vec());
    let args = vec![
        KernelArg::Ptr(d_src),
        KernelArg::Ptr(d_dst),
        sc(w),
        sc(r0),
        sc(c0),
        sc(rw),
        sc(rh),
    ];
    // block (4,4) covers rw=4, rh=3 (the y=3 lane is guarded off) in one block.
    launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (4, 4, 1), &args).unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_dst, rw * rh * 4));
    let mut want = vec![0i32; rw * rh];
    for y in 0..rh {
        for x in 0..rw {
            want[y * rw + x] = src[(r0 + y) * w + (c0 + x)];
        }
    }
    assert_eq!(got, want, "strided sub-rectangle copy layout");
    // Spot-check the closed form: dst[0] = src[(1)*8 + 2] = 10.
    assert_eq!(got[0], 10);
    assert_eq!(sink.executor().dispatches, 1);
}
