use super::*;

// ==================================================================================================
// 1. saxpy — y[i] = a*x[i] + y[i] over N elements across a multi-block grid.
// ==================================================================================================

/// nvcc-style saxpy with the natural param order `(x*, y*, a, n)` → offsets `u64@0, u64@8, f32@16,
/// u32@20`. Global index `ctaid*ntid+tid` + an `i >= n` guard, one `fma.rn.f32`.
const SAXPY_PTX: &str = r#"
    .visible .entry saxpy(
        .param .u64 saxpy_x,
        .param .u64 saxpy_y,
        .param .f32 saxpy_a,
        .param .u32 saxpy_n
    )
    {
        ld.param.u64  %rdx, [saxpy_x];
        ld.param.u64  %rdy, [saxpy_y];
        ld.param.f32  %fa,  [saxpy_a];
        ld.param.u32  %rn,  [saxpy_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %gx, %rdx;
        cvta.to.global.u64 %gy, %rdy;
        mul.wide.s32  %off, %ri, 4;
        add.s64       %px, %gx, %off;
        add.s64       %py, %gy, %off;
        ld.global.f32 %vx, [%px];
        ld.global.f32 %vy, [%py];
        fma.rn.f32    %vr, %fa, %vx, %vy;
        st.global.f32 [%py], %vr;
    DONE:
        ret;
    }
"#;

#[test]
fn saxpy_multiblock_exact() {
    let n = 1024usize;
    let alpha = 2.5f32;
    let x: Vec<f32> = (0..n).map(|i| (i as f32) * 0.5 - 3.0).collect();
    let y: Vec<f32> = (0..n).map(|i| (i as f32) * 0.25 + 1.0).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SAXPY_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "saxpy").unwrap();

    let dx = upload(&mut sink, &mut ctx, &f32s_to_bytes(&x));
    let dy = upload(&mut sink, &mut ctx, &f32s_to_bytes(&y));

    // grid = 4 blocks × 256 threads = exactly 1024 lanes.
    let args = vec![
        KernelArg::Ptr(dx),
        KernelArg::Ptr(dy),
        KernelArg::Scalar(alpha.to_le_bytes().to_vec()),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, (4, 1, 1), (256, 1, 1), &args).unwrap();

    let got = bytes_to_f32s(&readback(&mut sink, &ctx, dy, n * 4));
    let want: Vec<f32> = x
        .iter()
        .zip(&y)
        .map(|(xi, yi)| alpha.mul_add(*xi, *yi))
        .collect();
    assert_eq!(
        got, want,
        "saxpy y = a*x + y, all {n} elements across 4 blocks"
    );
    assert_eq!(sink.executor().dispatches, 1);
}
