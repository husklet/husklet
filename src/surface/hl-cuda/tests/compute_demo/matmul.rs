use super::*;

// ==================================================================================================
// 3. matmul — C(MxN) = A(MxK) · B(KxN), one output element per thread over a 2D block/grid + a k-loop.
// ==================================================================================================

/// Row-major matmul. `row = ctaid.y*ntid.y + tid.y`, `col = ctaid.x*ntid.x + tid.x`; each thread walks
/// `k = 0..K` accumulating `A[row*K+k] * B[k*N+col]` with `fma.rn.f32`. Bounds-guarded on both axes.
const MATMUL_PTX: &str = r#"
    .visible .entry matmul(
        .param .u64 mm_a,
        .param .u64 mm_b,
        .param .u64 mm_c,
        .param .u32 mm_m,
        .param .u32 mm_n,
        .param .u32 mm_k
    )
    {
        ld.param.u64  %ra, [mm_a];
        ld.param.u64  %rb, [mm_b];
        ld.param.u64  %rc, [mm_c];
        ld.param.u32  %rm, [mm_m];
        ld.param.u32  %rn, [mm_n];
        ld.param.u32  %rk, [mm_k];
        mov.u32       %rnx, %ntid.x;
        mov.u32       %rcx, %ctaid.x;
        mov.u32       %rtx, %tid.x;
        mad.lo.s32    %col, %rcx, %rnx, %rtx;
        mov.u32       %rny, %ntid.y;
        mov.u32       %rcy, %ctaid.y;
        mov.u32       %rty, %tid.y;
        mad.lo.s32    %row, %rcy, %rny, %rty;
        setp.ge.s32   %prm, %row, %rm;
        @%prm bra     DONE;
        setp.ge.s32   %pcn, %col, %rn;
        @%pcn bra     DONE;
        cvta.to.global.u64 %ga, %ra;
        cvta.to.global.u64 %gb, %rb;
        cvta.to.global.u64 %gc, %rc;
        mov.f32       %acc, 0f00000000;
        mov.u32       %k, 0;
    LOOP:
        setp.ge.s32   %pk, %k, %rk;
        @%pk bra      ENDLOOP;
        mad.lo.s32    %aidx, %row, %rk, %k;
        mul.wide.s32  %aoff, %aidx, 4;
        add.s64       %pa, %ga, %aoff;
        ld.global.f32 %av, [%pa];
        mad.lo.s32    %bidx, %k, %rn, %col;
        mul.wide.s32  %boff, %bidx, 4;
        add.s64       %pb, %gb, %boff;
        ld.global.f32 %bv, [%pb];
        fma.rn.f32    %acc, %av, %bv, %acc;
        add.s32       %k, %k, 1;
        bra           LOOP;
    ENDLOOP:
        mad.lo.s32    %cidx, %row, %rn, %col;
        mul.wide.s32  %coff, %cidx, 4;
        add.s64       %pc, %gc, %coff;
        st.global.f32 [%pc], %acc;
    DONE:
        ret;
    }
"#;

#[test]
fn matmul_tiled_exact() {
    let (m, k, n) = (4usize, 4usize, 4usize);
    // Fractional, non-trivial values so this is a genuine floating-point matmul.
    let a: Vec<f32> = (0..m * k).map(|i| (i as f32) * 0.5 - 1.0).collect();
    let b: Vec<f32> = (0..k * n).map(|i| (i as f32) * 0.25 + 0.5).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(MATMUL_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "matmul").unwrap();

    let da = upload(&mut sink, &mut ctx, &f32s_to_bytes(&a));
    let db = upload(&mut sink, &mut ctx, &f32s_to_bytes(&b));
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, (m * n * 4) as u64).unwrap();

    // 2D grid: block (2,2) × grid (2,2) covers the 4×4 output exactly.
    let args = vec![
        KernelArg::Ptr(da),
        KernelArg::Ptr(db),
        KernelArg::Ptr(dc),
        KernelArg::Scalar((m as i32).to_le_bytes().to_vec()),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
        KernelArg::Scalar((k as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, (2, 2, 1), (2, 2, 1), &args).unwrap();

    let got = bytes_to_f32s(&readback(&mut sink, &ctx, dc, m * n * 4));

    // CPU triple-loop reference with the SAME fma accumulation order.
    let mut want = vec![0f32; m * n];
    for row in 0..m {
        for col in 0..n {
            let mut acc = 0f32;
            for kk in 0..k {
                acc = a[row * k + kk].mul_add(b[kk * n + col], acc);
            }
            want[row * n + col] = acc;
        }
    }
    assert_eq!(got, want, "matmul C = A·B, exact per element");
    assert_eq!(sink.executor().dispatches, 1);
}
