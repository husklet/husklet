use super::*;

// ==================================================================================================
// 2. reduction — sum AND max of an N-element s32 array across a MULTI-BLOCK grid via global atomics.
//    (f32 atomics are intentionally unsupported by the model, so integer atomics carry the reduction.)
// ==================================================================================================

/// `out[0] += in[i]` for every in-bounds lane, accumulated across the whole grid with `red.global.add`.
/// Because device regions persist across blocks in the executor, the single accumulator sums cross-block.
const REDUCE_SUM_PTX: &str = r#"
    .visible .entry reduce_sum(
        .param .u64 rs_in,
        .param .u64 rs_out,
        .param .u32 rs_n
    )
    {
        ld.param.u64  %rin, [rs_in];
        ld.param.u64  %rout, [rs_out];
        ld.param.u32  %rn, [rs_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32  %off, %ri, 4;
        add.s64       %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        cvta.to.global.u64 %gout, %rout;
        red.global.add.u32 [%gout], %v;
    DONE:
        ret;
    }
"#;

/// `out[0] = max(out[0], in[i])` across the grid with signed `red.global.max`.
const REDUCE_MAX_PTX: &str = r#"
    .visible .entry reduce_max(
        .param .u64 rm_in,
        .param .u64 rm_out,
        .param .u32 rm_n
    )
    {
        ld.param.u64  %rin, [rm_in];
        ld.param.u64  %rout, [rm_out];
        ld.param.u32  %rn, [rm_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32  %off, %ri, 4;
        add.s64       %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        cvta.to.global.u64 %gout, %rout;
        red.global.max.s32 [%gout], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn reduction_sum_and_max_multiblock_exact() {
    let n = 1000usize;
    // signed inputs spanning negatives → positives: exercises signed max + wrapping-add sum.
    let input: Vec<i32> = (0..n).map(|i| i as i32 - 500).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    // ---- sum ----
    let sum_mod = ctx.load_module(REDUCE_SUM_PTX.as_bytes()).unwrap();
    let sum_fn = load_module::module_get_function(&ctx, sum_mod, "reduce_sum").unwrap();
    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_sum = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_sum, &0i32.to_le_bytes()).unwrap(); // accumulator = 0

    // 8 blocks × 128 = 1024 lanes over N=1000 (24 guarded off) → grid.x = 8 > 1.
    let sum_args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_sum),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        sum_fn,
        (8, 1, 1),
        (128, 1, 1),
        &sum_args,
    )
    .unwrap();

    let got_sum = bytes_to_i32s(&readback(&mut sink, &ctx, d_sum, 4))[0];
    let want_sum = input.iter().fold(0i32, |acc, v| acc.wrapping_add(*v));
    assert_eq!(got_sum, want_sum, "cross-block sum reduction");
    assert_eq!(
        got_sum, -500,
        "closed-form: sum_{{i=0}}^{{999}}(i-500) = -500"
    );

    // ---- max ----
    let max_mod = ctx.load_module(REDUCE_MAX_PTX.as_bytes()).unwrap();
    let max_fn = load_module::module_get_function(&ctx, max_mod, "reduce_max").unwrap();
    let d_max = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_max, &i32::MIN.to_le_bytes()).unwrap(); // -inf seed
    let max_args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_max),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        max_fn,
        (8, 1, 1),
        (128, 1, 1),
        &max_args,
    )
    .unwrap();

    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, 4))[0];
    let want_max = *input.iter().max().unwrap();
    assert_eq!(got_max, want_max, "cross-block max reduction");
    assert_eq!(got_max, 499);
    assert_eq!(
        sink.executor().dispatches,
        2,
        "two dispatches: one sum, one max"
    );
}
