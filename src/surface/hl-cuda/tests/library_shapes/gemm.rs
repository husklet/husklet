use super::*;

// ==================================================================================================
// 1. batched_strided_gemm — the cuBLAS `cublasGemmStridedBatched` shape: B independent GEMMs
//    C_b(M×N) = A_b(M×K) · B_b(K×N), the b-th operand at element base `b*stride`. Each 16×16 block
//    cooperatively stages a TILE of A_b and B_b into `.shared`, `bar.sync`s, accumulates, advances the
//    K tiles — the canonical tiled GEMM, replicated across `ctaid.z` = batch. M=N=K=32, TILE=16, B=3.
// ==================================================================================================

const GEMM_BATCHED_PTX: &str = r#"
    .visible .entry mm_batched(
        .param .u64 p_a,
        .param .u64 p_b,
        .param .u64 p_c,
        .param .u32 p_n,
        .param .u32 p_k,
        .param .u32 p_tiles,
        .param .u32 p_sa,
        .param .u32 p_sb,
        .param .u32 p_sc
    )
    {
        .shared .align 4 .b32 As[256];
        .shared .align 4 .b32 Bs[256];
        ld.param.u64 %ra, [p_a];
        ld.param.u64 %rb, [p_b];
        ld.param.u64 %rc, [p_c];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rk, [p_k];
        ld.param.u32 %rtiles, [p_tiles];
        ld.param.u32 %rsa, [p_sa];
        ld.param.u32 %rsb, [p_sb];
        ld.param.u32 %rsc, [p_sc];
        cvta.to.global.u64 %gA, %ra;
        cvta.to.global.u64 %gB, %rb;
        cvta.to.global.u64 %gC, %rc;
        mov.u32 %tx, %tid.x;
        mov.u32 %ty, %tid.y;
        mov.u32 %bx, %ctaid.x;
        mov.u32 %by, %ctaid.y;
        mov.u32 %bz, %ctaid.z;
        mad.lo.s32 %row, %by, 16, %ty;
        mad.lo.s32 %col, %bx, 16, %tx;
        mul.lo.s32 %baseA, %bz, %rsa;
        mul.lo.s32 %baseB, %bz, %rsb;
        mul.lo.s32 %baseC, %bz, %rsc;
        mad.lo.s32 %sidx, %ty, 16, %tx;
        mul.lo.s32 %soff, %sidx, 4;
        mov.u32 %asb, As;
        mov.u32 %bsb, Bs;
        add.s32 %asaddr, %asb, %soff;
        add.s32 %bsaddr, %bsb, %soff;
        mov.u32 %acc, 0;
        mov.u32 %t, 0;
    TLOOP:
        setp.ge.s32 %pdone, %t, %rtiles;
        @%pdone bra ENDT;
        mad.lo.s32 %acol, %t, 16, %tx;
        mad.lo.s32 %aidx, %row, %rk, %acol;
        add.s32 %aidx, %aidx, %baseA;
        mul.wide.s32 %aoff, %aidx, 4;
        add.s64 %aptr, %gA, %aoff;
        ld.global.u32 %av, [%aptr];
        st.shared.u32 [%asaddr], %av;
        mad.lo.s32 %brow, %t, 16, %ty;
        mad.lo.s32 %bidx, %brow, %rn, %col;
        add.s32 %bidx, %bidx, %baseB;
        mul.wide.s32 %boff, %bidx, 4;
        add.s64 %bptr, %gB, %boff;
        ld.global.u32 %bv, [%bptr];
        st.shared.u32 [%bsaddr], %bv;
        bar.sync;
        mov.u32 %kk, 0;
    KLOOP:
        setp.ge.s32 %pk, %kk, 16;
        @%pk bra ENDK;
        mad.lo.s32 %aik, %ty, 16, %kk;
        mul.lo.s32 %aiko, %aik, 4;
        add.s32 %aikaddr, %asb, %aiko;
        ld.shared.u32 %sa, [%aikaddr];
        mad.lo.s32 %bik, %kk, 16, %tx;
        mul.lo.s32 %biko, %bik, 4;
        add.s32 %bikaddr, %bsb, %biko;
        ld.shared.u32 %sbv, [%bikaddr];
        mad.lo.s32 %acc, %sa, %sbv, %acc;
        add.s32 %kk, %kk, 1;
        bra KLOOP;
    ENDK:
        bar.sync;
        add.s32 %t, %t, 1;
        bra TLOOP;
    ENDT:
        mad.lo.s32 %cidx, %row, %rn, %col;
        add.s32 %cidx, %cidx, %baseC;
        mul.wide.s32 %coff, %cidx, 4;
        add.s64 %cptr, %gC, %coff;
        st.global.u32 [%cptr], %acc;
        ret;
    }
"#;

#[test]
fn batched_strided_gemm_exact() {
    const TILE: usize = 16;
    let (m, n, k) = (32usize, 32usize, 32usize);
    let batch = 3usize;
    let (sa, sb, sc_stride) = (m * k, k * n, m * n);

    // Bounded signed operands so the i32 accumulation cannot overflow (|a|,|b| ≤ 9 ⇒ |Σ| ≤ 32·81 = 2592).
    let a: Vec<i32> = (0..batch * sa)
        .map(|i| (i as i32 * 7 + 3).rem_euclid(19) - 9)
        .collect();
    let b: Vec<i32> = (0..batch * sb)
        .map(|i| (i as i32 * 5 + 1).rem_euclid(19) - 9)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(GEMM_BATCHED_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "mm_batched").unwrap();

    let da = upload(&mut sink, &mut ctx, &i32s_to_bytes(&a));
    let db = upload(&mut sink, &mut ctx, &i32s_to_bytes(&b));
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, (batch * sc_stride * 4) as u64).unwrap();

    let tiles = k / TILE; // 2
    let args = vec![
        KernelArg::Ptr(da),
        KernelArg::Ptr(db),
        KernelArg::Ptr(dc),
        sc(n as i32),
        sc(k as i32),
        sc(tiles as i32),
        sc(sa as i32),
        sc(sb as i32),
        sc(sc_stride as i32),
    ];
    // grid (2,2,batch) blocks × block (16,16) threads = the full 32×32 output for every batch.
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        ((n / TILE) as u32, (m / TILE) as u32, batch as u32),
        (16, 16, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, dc, batch * sc_stride * 4));

    // CPU reference: an independent per-batch triple loop, i32-wrapping to match the interpreter.
    let mut want = vec![0i32; batch * sc_stride];
    for bt in 0..batch {
        for row in 0..m {
            for col in 0..n {
                let mut acc = 0i32;
                for kk in 0..k {
                    acc = acc.wrapping_add(
                        a[bt * sa + row * k + kk].wrapping_mul(b[bt * sb + kk * n + col]),
                    );
                }
                want[bt * sc_stride + row * n + col] = acc;
            }
        }
    }
    assert_eq!(
        got, want,
        "strided-batched GEMM: every element of every batch exact"
    );
    // Batches must actually differ (guards against a stride bug that reruns batch 0 three times).
    assert_ne!(
        &want[0..sc_stride],
        &want[sc_stride..2 * sc_stride],
        "distinct batches must produce distinct C (stride is real)"
    );
    assert_eq!(sink.executor().dispatches, 1);
}
