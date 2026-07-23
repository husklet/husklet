use super::*;

// ==================================================================================================
// 6. shared-memory + bar.sync — block-scoped tree reduction, one partial per block, multi-block grid.
// ==================================================================================================

/// Each block loads its `blockDim` slice into `.shared`, tree-reduces with `bar.sync` between halving
/// steps, and thread 0 writes the block partial to `out[blockIdx.x]`. Exercises workgroup memory +
/// the cooperative barrier model across a multi-block grid.
const BLOCKREDUCE_PTX: &str = r#"
    .visible .entry blockreduce(
        .param .u64 br_in,
        .param .u64 br_out,
        .param .u32 br_n
    )
    {
        .shared .align 4 .b32 sdata[256];
        ld.param.u64  %rin, [br_in];
        ld.param.u64  %rout, [br_out];
        ld.param.u32  %rn, [br_n];
        mov.u32 %tid, %tid.x;
        mov.u32 %bd, %ntid.x;
        mov.u32 %cx, %ctaid.x;
        mad.lo.s32 %gid, %cx, %bd, %tid;
        mov.u32 %val, 0;
        setp.ge.s32 %pge, %gid, %rn;
        @%pge bra STORE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %ioff, %gid, 4;
        add.s64 %ip, %gin, %ioff;
        ld.global.u32 %val, [%ip];
    STORE:
        mul.wide.s32 %sidx, %tid, 4;
        st.shared.u32 [%sidx], %val;
        bar.sync;
        shr.u32 %s, %bd, 1;
    RLOOP:
        setp.gt.s32 %pcont, %s, 0;
        @!%pcont bra DONE_REDUCE;
        setp.lt.s32 %plt, %tid, %s;
        @!%plt bra SKIP;
        mul.wide.s32 %ia, %tid, 4;
        add.s32 %tids, %tid, %s;
        mul.wide.s32 %ib, %tids, 4;
        ld.shared.u32 %va, [%ia];
        ld.shared.u32 %vb, [%ib];
        add.s32 %sum, %va, %vb;
        st.shared.u32 [%ia], %sum;
    SKIP:
        bar.sync;
        shr.u32 %s, %s, 1;
        bra RLOOP;
    DONE_REDUCE:
        setp.ne.s32 %pnz, %tid, 0;
        @%pnz bra DONE;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %ooff, %cx, 4;
        add.s64 %op, %gout, %ooff;
        ld.shared.u32 %res, [0];
        st.global.u32 [%op], %res;
    DONE: ret;
    }
"#;

#[test]
fn shared_memory_block_reduction_exact() {
    let block = 8usize;
    let grid = 4usize;
    let n = block * grid; // 32 elements, one partial per block
    let input: Vec<i32> = (0..n).map(|i| (i as i32 * 3 + 1) % 17).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(BLOCKREDUCE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "blockreduce").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_out = allocate::mem_alloc(&mut ctx, &mut sink, (grid * 4) as u64).unwrap();

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_out),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args,
    )
    .unwrap();

    let partials = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, grid * 4));

    // Reference: each block sums its own contiguous slice.
    let want_partials: Vec<i32> = (0..grid)
        .map(|b| input[b * block..(b + 1) * block].iter().sum())
        .collect();
    assert_eq!(
        partials, want_partials,
        "per-block shared-memory partials, each exact"
    );

    // And the host-summed total equals the whole-array sum.
    let total: i32 = partials.iter().sum();
    assert_eq!(
        total,
        input.iter().sum::<i32>(),
        "grid total from block partials"
    );
    assert_eq!(sink.executor().dispatches, 1);
}
