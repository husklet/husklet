use super::*;

// ==================================================================================================
// 4. elementwise — mul / add (f32) and min (s32, branch-selected) over two arrays. Three entries in
//    one PTX module; each launch checks every element.
// ==================================================================================================

const ELEMENTWISE_PTX: &str = r#"
    .visible .entry emul(
        .param .u64 em_a, .param .u64 em_b, .param .u64 em_c, .param .u32 em_n
    ) {
        ld.param.u64  %ra, [em_a];
        ld.param.u64  %rb, [em_b];
        ld.param.u64  %rc, [em_c];
        ld.param.u32  %rn, [em_n];
        mov.u32 %rnt, %ntid.x; mov.u32 %rct, %ctaid.x; mov.u32 %rtt, %tid.x;
        mad.lo.s32 %i, %rct, %rnt, %rtt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %ga, %ra;
        cvta.to.global.u64 %gb, %rb;
        cvta.to.global.u64 %gc, %rc;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pa, %ga, %off; add.s64 %pb, %gb, %off; add.s64 %pc, %gc, %off;
        ld.global.f32 %va, [%pa];
        ld.global.f32 %vb, [%pb];
        mul.f32 %vr, %va, %vb;
        st.global.f32 [%pc], %vr;
    DONE: ret;
    }

    .visible .entry eadd(
        .param .u64 ea_a, .param .u64 ea_b, .param .u64 ea_c, .param .u32 ea_n
    ) {
        ld.param.u64  %ra, [ea_a];
        ld.param.u64  %rb, [ea_b];
        ld.param.u64  %rc, [ea_c];
        ld.param.u32  %rn, [ea_n];
        mov.u32 %rnt, %ntid.x; mov.u32 %rct, %ctaid.x; mov.u32 %rtt, %tid.x;
        mad.lo.s32 %i, %rct, %rnt, %rtt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %ga, %ra;
        cvta.to.global.u64 %gb, %rb;
        cvta.to.global.u64 %gc, %rc;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pa, %ga, %off; add.s64 %pb, %gb, %off; add.s64 %pc, %gc, %off;
        ld.global.f32 %va, [%pa];
        ld.global.f32 %vb, [%pb];
        add.f32 %vr, %va, %vb;
        st.global.f32 [%pc], %vr;
    DONE: ret;
    }

    .visible .entry emin(
        .param .u64 en_a, .param .u64 en_b, .param .u64 en_c, .param .u32 en_n
    ) {
        ld.param.u64  %ra, [en_a];
        ld.param.u64  %rb, [en_b];
        ld.param.u64  %rc, [en_c];
        ld.param.u32  %rn, [en_n];
        mov.u32 %rnt, %ntid.x; mov.u32 %rct, %ctaid.x; mov.u32 %rtt, %tid.x;
        mad.lo.s32 %i, %rct, %rnt, %rtt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %ga, %ra;
        cvta.to.global.u64 %gb, %rb;
        cvta.to.global.u64 %gc, %rc;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pa, %ga, %off; add.s64 %pb, %gb, %off; add.s64 %pc, %gc, %off;
        ld.global.u32 %va, [%pa];
        ld.global.u32 %vb, [%pb];
        setp.lt.s32 %plt, %va, %vb;
        @%plt bra USEA;
        st.global.u32 [%pc], %vb;
        bra DONE;
    USEA:
        st.global.u32 [%pc], %va;
    DONE: ret;
    }
"#;

#[test]
fn elementwise_mul_add_min_exact() {
    let n = 512usize;
    let af: Vec<f32> = (0..n).map(|i| (i as f32) * 0.3 - 2.0).collect();
    let bf: Vec<f32> = (0..n).map(|i| (i as f32) * -0.1 + 4.0).collect();
    let ai: Vec<i32> = (0..n).map(|i| (i as i32 * 7) % 101 - 50).collect();
    let bi: Vec<i32> = (0..n).map(|i| (i as i32 * 13) % 97 - 40).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(ELEMENTWISE_PTX.as_bytes()).unwrap();

    let grid = (2u32, 1, 1);
    let block = (256u32, 1, 1); // 512 lanes total

    // mul (f32)
    let da = upload(&mut sink, &mut ctx, &f32s_to_bytes(&af));
    let db = upload(&mut sink, &mut ctx, &f32s_to_bytes(&bf));
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let mul_fn = load_module::module_get_function(&ctx, module, "emul").unwrap();
    let args = |x: DevicePtr, y: DevicePtr, z: DevicePtr| {
        vec![
            KernelArg::Ptr(x),
            KernelArg::Ptr(y),
            KernelArg::Ptr(z),
            KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
        ]
    };
    launch::launch(&mut ctx, &mut sink, mul_fn, grid, block, &args(da, db, dc)).unwrap();
    let got_mul = bytes_to_f32s(&readback(&mut sink, &ctx, dc, n * 4));
    let want_mul: Vec<f32> = af.iter().zip(&bf).map(|(x, y)| x * y).collect();
    assert_eq!(got_mul, want_mul, "elementwise mul");

    // add (f32) — reuse the same buffers, distinct entry.
    let add_fn = load_module::module_get_function(&ctx, module, "eadd").unwrap();
    launch::launch(&mut ctx, &mut sink, add_fn, grid, block, &args(da, db, dc)).unwrap();
    let got_add = bytes_to_f32s(&readback(&mut sink, &ctx, dc, n * 4));
    let want_add: Vec<f32> = af.iter().zip(&bf).map(|(x, y)| x + y).collect();
    assert_eq!(got_add, want_add, "elementwise add");

    // min (s32)
    let dai = upload(&mut sink, &mut ctx, &i32s_to_bytes(&ai));
    let dbi = upload(&mut sink, &mut ctx, &i32s_to_bytes(&bi));
    let dci = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let min_fn = load_module::module_get_function(&ctx, module, "emin").unwrap();
    launch::launch(
        &mut ctx,
        &mut sink,
        min_fn,
        grid,
        block,
        &args(dai, dbi, dci),
    )
    .unwrap();
    let got_min = bytes_to_i32s(&readback(&mut sink, &ctx, dci, n * 4));
    let want_min: Vec<i32> = ai.iter().zip(&bi).map(|(x, y)| *x.min(y)).collect();
    assert_eq!(got_min, want_min, "elementwise signed min");

    assert_eq!(sink.executor().dispatches, 3);
}
