use super::*;

// ==================================================================================================
// 1. tiled_matmul — shared-memory blocked (TILE=16) integer matmul, C(N×N) = A(N×K) · B(K×N).
//    Each 16×16 block cooperatively stages a TILE of A and a TILE of B into `.shared`, `bar.sync`,
//    accumulates the tile product, `bar.sync`, and advances — the canonical CUDA tiled GEMM.
// ==================================================================================================

const MATMUL_TILED_PTX: &str = r#"
    .visible .entry mm_tiled(
        .param .u64 p_a,
        .param .u64 p_b,
        .param .u64 p_c,
        .param .u32 p_n,
        .param .u32 p_k,
        .param .u32 p_tiles
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
        cvta.to.global.u64 %gA, %ra;
        cvta.to.global.u64 %gB, %rb;
        cvta.to.global.u64 %gC, %rc;
        mov.u32 %tx, %tid.x;
        mov.u32 %ty, %tid.y;
        mov.u32 %bx, %ctaid.x;
        mov.u32 %by, %ctaid.y;
        mad.lo.s32 %row, %by, 16, %ty;
        mad.lo.s32 %col, %bx, 16, %tx;
        // per-thread shared slot byte offset: (ty*16 + tx) * 4
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
        // As[ty][tx] = A[row*K + (t*16 + tx)]
        mad.lo.s32 %acol, %t, 16, %tx;
        mad.lo.s32 %aidx, %row, %rk, %acol;
        mul.wide.s32 %aoff, %aidx, 4;
        add.s64 %aptr, %gA, %aoff;
        ld.global.u32 %av, [%aptr];
        st.shared.u32 [%asaddr], %av;
        // Bs[ty][tx] = B[(t*16 + ty)*N + col]
        mad.lo.s32 %brow, %t, 16, %ty;
        mad.lo.s32 %bidx, %brow, %rn, %col;
        mul.wide.s32 %boff, %bidx, 4;
        add.s64 %bptr, %gB, %boff;
        ld.global.u32 %bv, [%bptr];
        st.shared.u32 [%bsaddr], %bv;
        bar.sync;
        // inner: acc += As[ty][k] * Bs[k][tx], k = 0..16
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
        mul.wide.s32 %coff, %cidx, 4;
        add.s64 %cptr, %gC, %coff;
        st.global.u32 [%cptr], %acc;
        ret;
    }
"#;

#[test]
fn tiled_matmul_shared_memory_exact() {
    const TILE: usize = 16;
    let (n, k) = (64usize, 64usize); // square 64×64, K=64
                                     // Bounded signed values so the i32 accumulation cannot overflow (|a|,|b| ≤ 9 ⇒ |Σ| ≤ 64·81 = 5184).
    let a: Vec<i32> = (0..n * k)
        .map(|i| (i as i32 * 7 + 3).rem_euclid(19) - 9)
        .collect();
    let b: Vec<i32> = (0..k * n)
        .map(|i| (i as i32 * 5 + 1).rem_euclid(19) - 9)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(MATMUL_TILED_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "mm_tiled").unwrap();

    let da = upload(&mut sink, &mut ctx, &i32s_to_bytes(&a));
    let db = upload(&mut sink, &mut ctx, &i32s_to_bytes(&b));
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, (n * n * 4) as u64).unwrap();

    let tiles = k / TILE; // 4
    let args = vec![
        KernelArg::Ptr(da),
        KernelArg::Ptr(db),
        KernelArg::Ptr(dc),
        sc(n as i32),
        sc(k as i32),
        sc(tiles as i32),
    ];
    // grid (4,4) blocks × block (16,16) threads = exactly the 64×64 output.
    launch::launch(&mut ctx, &mut sink, func, (4, 4, 1), (16, 16, 1), &args).unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, dc, n * n * 4));

    // CPU reference: wrapping i32 triple loop (matches the interpreter's 32-bit mad semantics exactly).
    let mut want = vec![0i32; n * n];
    for row in 0..n {
        for col in 0..n {
            let mut acc = 0i32;
            for kk in 0..k {
                acc = acc.wrapping_add(a[row * k + kk].wrapping_mul(b[kk * n + col]));
            }
            want[row * n + col] = acc;
        }
    }
    assert_eq!(got, want, "tiled matmul C = A·B, every element exact");
    // Spot-check one non-trivial element is actually non-zero (guards against an all-zero fake pass).
    assert!(
        want.iter().any(|&v| v != 0),
        "reference must be non-degenerate"
    );
    assert_eq!(sink.executor().dispatches, 1);
}
