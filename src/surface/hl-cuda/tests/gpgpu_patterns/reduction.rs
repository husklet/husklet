use super::*;

// ==================================================================================================
// 5. reduce_segmented — per-segment sum AND signed max. Segments have a fixed width (a power of two so
//    seg = i >> log2(width), the interpreter modeling no integer division), so segment boundaries do NOT
//    align with block boundaries — atomics from many blocks land in the same segment bin, and the
//    cross-block totals must still be exact.
// ==================================================================================================

const SEG_REDUCE_PTX: &str = r#"
    .visible .entry seg_reduce(
        .param .u64 p_in,
        .param .u64 p_sum,
        .param .u64 p_max,
        .param .u32 p_n,
        .param .u32 p_shift
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rsum, [p_sum];
        ld.param.u64 %rmax, [p_max];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rshift, [p_shift];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        // seg = i >> shift
        shr.u32 %seg, %i, %rshift;
        mul.wide.s32 %soff, %seg, 4;
        cvta.to.global.u64 %gsum, %rsum;
        add.s64 %psum, %gsum, %soff;
        red.global.add.u32 [%psum], %v;
        cvta.to.global.u64 %gmax, %rmax;
        add.s64 %pmax, %gmax, %soff;
        red.global.max.s32 [%pmax], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn reduce_segmented_sum_and_max_exact() {
    let n = 1000usize;
    let seg_shift = 6u32; // segment width = 64
    let seg_w = 1usize << seg_shift;
    let nseg = n.div_ceil(seg_w); // 16 segments (last partial: 960..999)
    let input: Vec<i32> = (0..n).map(|i| (i as i32 * 37 + 11) % 200 - 60).collect(); // signed, spans ±

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SEG_REDUCE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "seg_reduce").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_sum = alloc_zeroed_i32(&mut sink, &mut ctx, nseg);
    // seed the max bins with i32::MIN.
    let d_max = allocate::mem_alloc(&mut ctx, &mut sink, (nseg * 4) as u64).unwrap();
    transfer::memset(
        &mut ctx,
        &mut sink,
        d_max,
        &i32s_to_bytes(&vec![i32::MIN; nseg]),
    )
    .unwrap();

    let block = 128u32;
    let grid = (n as u32).div_ceil(block); // 8 blocks; boundaries cross segments
    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_sum),
        KernelArg::Ptr(d_max),
        sc(n as i32),
        sc(seg_shift as i32),
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

    let got_sum = bytes_to_i32s(&readback(&mut sink, &ctx, d_sum, nseg * 4));
    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, nseg * 4));

    // Reference per-segment sum + max.
    let mut want_sum = vec![0i32; nseg];
    let mut want_max = vec![i32::MIN; nseg];
    for (i, value) in input.iter().enumerate().take(n) {
        let s = i >> seg_shift;
        want_sum[s] += value;
        want_max[s] = want_max[s].max(*value);
    }
    assert_eq!(
        got_sum, want_sum,
        "per-segment sum exact (cross-block atomic accumulation)"
    );
    assert_eq!(got_max, want_max, "per-segment signed max exact");
    assert_eq!(
        got_sum.iter().sum::<i32>(),
        input.iter().sum::<i32>(),
        "segment sums cover every element"
    );
}
