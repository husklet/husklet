use super::*;

// ==================================================================================================
// 8. im2col + embedding — two exact index-remap front-ends.
//    (a) im2col: lower a [C,H,W] input into the column matrix [C·K·K, OH·OW] a GEMM-based convolution
//        multiplies against (valid pad, 3×3, stride 1). One thread per output spatial cell writes its
//        whole patch column — the canonical im2col gather.
//    (b) embedding: gather rows of a [V,D] table by a length-T index vector → [T,D] (one block per token,
//        `ctaid.x` = token, `tid.x` = feature) — the embedding-lookup front-end of every transformer.
// ==================================================================================================

const IM2COL_PTX: &str = r#"
    .visible .entry im2col(
        .param .u64 p_in,
        .param .u64 p_col,
        .param .u32 p_C,
        .param .u32 p_H,
        .param .u32 p_W,
        .param .u32 p_OH,
        .param .u32 p_OW,
        .param .u32 p_K
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rcol, [p_col];
        ld.param.u32 %rC, [p_C];
        ld.param.u32 %rH, [p_H];
        ld.param.u32 %rW, [p_W];
        ld.param.u32 %rOH, [p_OH];
        ld.param.u32 %rOW, [p_OW];
        ld.param.u32 %rK, [p_K];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gcol, %rcol;
        mov.u32 %ox, %tid.x;
        mov.u32 %oy, %tid.y;
        setp.ge.s32 %pxo, %ox, %rOW;
        @%pxo bra DONE;
        setp.ge.s32 %pyo, %oy, %rOH;
        @%pyo bra DONE;
        mul.lo.s32 %ncols, %rOH, %rOW;
        mad.lo.s32 %colIdx, %oy, %rOW, %ox;
        mov.u32 %c, 0;
    CLOOP:
        setp.ge.s32 %pc, %c, %rC;
        @%pc bra CEND;
        mov.u32 %ky, 0;
    KYLOOP:
        setp.ge.s32 %pky, %ky, %rK;
        @%pky bra KYEND;
        add.s32 %iy, %oy, %ky;
        mov.u32 %kx, 0;
    KXLOOP:
        setp.ge.s32 %pkx, %kx, %rK;
        @%pkx bra KXEND;
        add.s32 %ix, %ox, %kx;
        // in index = (c*H + iy)*W + ix
        mad.lo.s32 %t1, %c, %rH, %iy;
        mad.lo.s32 %iidx, %t1, %rW, %ix;
        mul.wide.s32 %io, %iidx, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %v, [%ip];
        // patchRow = (c*K + ky)*K + kx
        mad.lo.s32 %pr1, %c, %rK, %ky;
        mad.lo.s32 %patchRow, %pr1, %rK, %kx;
        // dst = patchRow*ncols + colIdx
        mad.lo.s32 %dst, %patchRow, %ncols, %colIdx;
        mul.wide.s32 %doff, %dst, 4;
        add.s64 %dp, %gcol, %doff;
        st.global.u32 [%dp], %v;
        add.s32 %kx, %kx, 1;
        bra KXLOOP;
    KXEND:
        add.s32 %ky, %ky, 1;
        bra KYLOOP;
    KYEND:
        add.s32 %c, %c, 1;
        bra CLOOP;
    CEND:
    DONE:
        ret;
    }

    .visible .entry embedding(
        .param .u64 p_tab,
        .param .u64 p_idx,
        .param .u64 p_out,
        .param .u32 p_D
    )
    {
        ld.param.u64 %rtab, [p_tab];
        ld.param.u64 %ridx, [p_idx];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rD, [p_D];
        cvta.to.global.u64 %gtab, %rtab;
        cvta.to.global.u64 %gidx, %ridx;
        cvta.to.global.u64 %gout, %rout;
        mov.u32 %d, %tid.x;
        mov.u32 %t, %ctaid.x;
        setp.ge.s32 %pd, %d, %rD;
        @%pd bra DONE;
        // ix = idx[t]
        mul.wide.s32 %to, %t, 4;
        add.s64 %ixp, %gidx, %to;
        ld.global.u32 %ix, [%ixp];
        // src = ix*D + d ; dst = t*D + d
        mad.lo.s32 %src, %ix, %rD, %d;
        mad.lo.s32 %dst, %t, %rD, %d;
        mul.wide.s32 %so, %src, 4;
        add.s64 %sp, %gtab, %so;
        ld.global.u32 %v, [%sp];
        mul.wide.s32 %dsto, %dst, 4;
        add.s64 %dp, %gout, %dsto;
        st.global.u32 [%dp], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn im2col_and_embedding_gather_exact() {
    // --- (a) im2col ---
    let (c, h, w, k) = (2usize, 5usize, 5usize, 3usize);
    let (oh, ow) = (h - k + 1, w - k + 1); // 3×3
    let ncols = oh * ow;
    let patch_rows = c * k * k;
    let input: Vec<i32> = (0..c * h * w).map(|i| (i as i32 * 3 + 1) % 100).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(IM2COL_PTX.as_bytes()).unwrap();
    let im2col_fn = load_module::module_get_function(&ctx, module, "im2col").unwrap();
    let embed_fn = load_module::module_get_function(&ctx, module, "embedding").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_col = alloc_zeroed_i32(&mut sink, &mut ctx, patch_rows * ncols);

    let args_im = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_col),
        sc(c as i32),
        sc(h as i32),
        sc(w as i32),
        sc(oh as i32),
        sc(ow as i32),
        sc(k as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        im2col_fn,
        (1, 1, 1),
        (ow as u32, oh as u32, 1),
        &args_im,
    )
    .unwrap();

    let got_col = bytes_to_i32s(&readback(&mut sink, &ctx, d_col, patch_rows * ncols * 4));

    let mut want_col = vec![0i32; patch_rows * ncols];
    for cc in 0..c {
        for ky in 0..k {
            for kx in 0..k {
                let patch_row = (cc * k + ky) * k + kx;
                for oy in 0..oh {
                    for ox in 0..ow {
                        let col_idx = oy * ow + ox;
                        want_col[patch_row * ncols + col_idx] =
                            input[(cc * h + (oy + ky)) * w + (ox + kx)];
                    }
                }
            }
        }
    }
    assert_eq!(got_col, want_col, "im2col column matrix, every entry exact");

    // --- (b) embedding gather ---
    let (vocab, dim, tokens) = (10usize, 4usize, 5usize);
    let table: Vec<i32> = (0..vocab * dim).map(|i| i as i32 * 2 - 3).collect();
    let idx: Vec<i32> = [7i32, 0, 3, 9, 3].to_vec(); // includes a repeat (token 2 and 4 → same row)

    let d_tab = upload(&mut sink, &mut ctx, &i32s_to_bytes(&table));
    let d_ix = upload(&mut sink, &mut ctx, &i32s_to_bytes(&idx));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, tokens * dim);

    let args_em = vec![
        KernelArg::Ptr(d_tab),
        KernelArg::Ptr(d_ix),
        KernelArg::Ptr(d_out),
        sc(dim as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        embed_fn,
        (tokens as u32, 1, 1),
        (dim as u32, 1, 1),
        &args_em,
    )
    .unwrap();

    let got_emb = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, tokens * dim * 4));

    let mut want_emb = vec![0i32; tokens * dim];
    for t in 0..tokens {
        for d in 0..dim {
            want_emb[t * dim + d] = table[(idx[t] as usize) * dim + d];
        }
    }
    assert_eq!(got_emb, want_emb, "embedding gather, every element exact");
    // The two tokens sharing index 3 must gather identical rows (guards a token/index swap).
    assert_eq!(
        &got_emb[2 * dim..3 * dim],
        &got_emb[4 * dim..5 * dim],
        "repeated index gathers the same row"
    );
}
