use super::*;

// ==================================================================================================
// 6. transpose — shared-memory coalesced tiled transpose. A 16×16 tile is staged into `.shared` with a
//    coalesced read, `bar.sync`, then written to the output at the transposed block offset (the classic
//    NVIDIA transpose). Non-square with a remainder tile on both axes; bounds-guarded.
// ==================================================================================================

const TRANSPOSE_PTX: &str = r#"
    .visible .entry transpose(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_w,
        .param .u32 p_h
    )
    {
        .shared .align 4 .b32 tile[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rw, [p_w];
        ld.param.u32 %rh, [p_h];
        mov.u32 %tx, %tid.x;
        mov.u32 %ty, %tid.y;
        mov.u32 %bx, %ctaid.x;
        mov.u32 %by, %ctaid.y;
        // load phase: x = bx*16+tx, y = by*16+ty ; tile[ty*16+tx] = in[y*w + x]
        mad.lo.s32 %x, %bx, 16, %tx;
        mad.lo.s32 %y, %by, 16, %ty;
        setp.ge.s32 %pxo, %x, %rw;
        @%pxo bra SYNC;
        setp.ge.s32 %pyo, %y, %rh;
        @%pyo bra SYNC;
        cvta.to.global.u64 %gin, %rin;
        mad.lo.s32 %iidx, %y, %rw, %x;
        mul.wide.s32 %io, %iidx, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %v, [%ip];
        mad.lo.s32 %tsi, %ty, 16, %tx;
        mul.lo.s32 %tsb, %tsi, 4;
        st.shared.u32 [%tsb], %v;
    SYNC:
        bar.sync;
        // store phase: xo = by*16+tx, yo = bx*16+ty ; out[yo*h + xo] = tile[tx*16+ty]
        mad.lo.s32 %xo, %by, 16, %tx;
        mad.lo.s32 %yo, %bx, 16, %ty;
        setp.ge.s32 %pxo2, %xo, %rh;
        @%pxo2 bra DONE;
        setp.ge.s32 %pyo2, %yo, %rw;
        @%pyo2 bra DONE;
        mad.lo.s32 %tri, %tx, 16, %ty;
        mul.lo.s32 %trb, %tri, 4;
        ld.shared.u32 %tv, [%trb];
        cvta.to.global.u64 %gout, %rout;
        mad.lo.s32 %oidx, %yo, %rh, %xo;
        mul.wide.s32 %oo, %oidx, 4;
        add.s64 %op, %gout, %oo;
        st.global.u32 [%op], %tv;
    DONE:
        ret;
    }
"#;

#[test]
fn transpose_shared_memory_coalesced_exact() {
    let (w, h) = (48usize, 34usize); // non-square, remainder tiles on both axes
    let input: Vec<i32> = (0..w * h).map(|i| i as i32).collect(); // distinct per cell → any misindex caught

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(TRANSPOSE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "transpose").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, w * h);
    let gx = w.div_ceil(16) as u32; // 3
    let gy = h.div_ceil(16) as u32; // 3

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_out),
        sc(w as i32),
        sc(h as i32),
    ];
    launch::launch(&mut ctx, &mut sink, func, (gx, gy, 1), (16, 16, 1), &args).unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, w * h * 4));

    // Reference: out[c][r] = in[r][c], out is (w rows × h cols).
    let mut want = vec![0i32; w * h];
    for r in 0..h {
        for c in 0..w {
            want[c * h + r] = input[r * w + c];
        }
    }
    assert_eq!(
        got, want,
        "shared-memory tiled transpose, every element exact"
    );
}
