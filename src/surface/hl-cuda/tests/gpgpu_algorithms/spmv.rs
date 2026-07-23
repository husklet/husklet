use super::*;

const SPMV_PTX: &str = r#"
    .visible .entry spmv(
        .param .u64 p_roff,
        .param .u64 p_col,
        .param .u64 p_val,
        .param .u64 p_x,
        .param .u64 p_y,
        .param .u32 p_rows
    )
    {
        ld.param.u64 %rroff, [p_roff];
        ld.param.u64 %rcol, [p_col];
        ld.param.u64 %rval, [p_val];
        ld.param.u64 %rx, [p_x];
        ld.param.u64 %ry, [p_y];
        ld.param.u32 %rrows, [p_rows];
        mad.lo.s32 %row, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %row, %rrows;
        @%pdone bra DONE;
        cvta.to.global.u64 %groff, %rroff;
        cvta.to.global.u64 %gcol, %rcol;
        cvta.to.global.u64 %gval, %rval;
        cvta.to.global.u64 %gx, %rx;
        mul.wide.s32 %ro, %row, 4;
        add.s64 %rp, %groff, %ro;
        ld.global.u32 %start, [%rp];
        ld.global.u32 %end, [%rp+4];
        mov.u32 %acc, 0;
        mov.u32 %e, %start;
    LOOP:
        setp.ge.s32 %pl, %e, %end;
        @%pl bra STORE;
        mul.wide.s32 %eo, %e, 4;
        add.s64 %vp, %gval, %eo;
        ld.global.u32 %av, [%vp];
        add.s64 %cp, %gcol, %eo;
        ld.global.u32 %c, [%cp];
        mul.wide.s32 %xo, %c, 4;
        add.s64 %xp, %gx, %xo;
        ld.global.u32 %xv, [%xp];
        mad.lo.s32 %acc, %av, %xv, %acc;
        add.s32 %e, %e, 1;
        bra LOOP;
    STORE:
        cvta.to.global.u64 %gy, %ry;
        add.s64 %yp, %gy, %ro;
        st.global.u32 [%yp], %acc;
    DONE:
        ret;
    }
"#;

#[test]
fn spmv_csr_exact() {
    // 6×6 sparse matrix in CSR. Row 3 is empty (a real ragged-CSR edge case → y[3] == 0).
    let rows = 6usize;
    let cols = 6usize;
    // (col, val) per row
    let row_entries: Vec<Vec<(i32, i32)>> = vec![
        vec![(1, 3), (3, 2)],         // 0
        vec![(0, 5)],                 // 1
        vec![(2, 1), (4, 4), (5, 2)], // 2
        vec![],                       // 3  (empty)
        vec![(1, 7), (5, 1)],         // 4
        vec![(3, 6)],                 // 5
    ];
    let x: Vec<i32> = vec![2, -1, 4, 3, 5, -2];
    assert_eq!(x.len(), cols);

    let mut row_off = vec![0i32; rows + 1];
    let mut col = Vec::new();
    let mut val = Vec::new();
    for r in 0..rows {
        for &(c, vv) in &row_entries[r] {
            col.push(c);
            val.push(vv);
        }
        row_off[r + 1] = col.len() as i32;
    }

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SPMV_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "spmv").unwrap();

    let d_roff = upload(&mut sink, &mut ctx, &i32s_to_bytes(&row_off));
    let d_col = upload(&mut sink, &mut ctx, &i32s_to_bytes(&col));
    let d_val = upload(&mut sink, &mut ctx, &i32s_to_bytes(&val));
    let d_x = upload(&mut sink, &mut ctx, &i32s_to_bytes(&x));
    let d_y = alloc_zeroed_i32(&mut sink, &mut ctx, rows);

    let block = 4u32;
    let grid = (rows as u32 + block - 1) / block; // 2 blocks
    let args = vec![
        KernelArg::Ptr(d_roff),
        KernelArg::Ptr(d_col),
        KernelArg::Ptr(d_val),
        KernelArg::Ptr(d_x),
        KernelArg::Ptr(d_y),
        sc(rows as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (grid, 1, 1),
        (block, 1, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_y, rows * 4));

    // CPU reference.
    let mut want = vec![0i32; rows];
    for r in 0..rows {
        let mut acc = 0i32;
        for &(c, vv) in &row_entries[r] {
            acc += vv * x[c as usize];
        }
        want[r] = acc;
    }
    assert_eq!(got, want, "SpMV (CSR) y = A·x exact");
    assert_eq!(got[3], 0, "empty row yields 0");
    assert!(want.iter().any(|&v| v != 0), "reference is non-degenerate");
}

// ==================================================================================================
// 5. stream_compaction — compact the elements passing a predicate, in order. Three device stages plus a
//    host block-offset combine:
//      (a) `predicate`   — flag[i] = keep(in[i]) ? 1 : 0  (here: even values).
//      (b) multi-block inclusive prefix scan of flags → incl (block Hillis-Steele + host offsets +
//          add_offset — the same scan skeleton as gpgpu_patterns::prefix_scan).
//      (c) `scatter`     — if flag[i]: out[incl[i]-1] = in[i]   (incl[i]-1 == exclusive output position).
//    count = incl[n-1]. The compacted prefix out[0..count] must equal the CPU filter, exactly.
// ==================================================================================================
