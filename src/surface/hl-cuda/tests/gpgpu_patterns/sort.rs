use super::*;

// ==================================================================================================
// 7. bitonic_sort — a bitonic sorting network over a power-of-2 N in `.shared`, one `bar.sync` per
//    compare-exchange substep. Only the lower index of each pair performs the swap, so writes never
//    race; every thread executes the identical (k,j) loop nest, hitting every barrier. Exact vs sorted.
// ==================================================================================================

const BITONIC_PTX: &str = r#"
    .visible .entry bitonic(
        .param .u64 p_data,
        .param .u32 p_n
    )
    {
        .shared .align 4 .b32 s[256];
        ld.param.u64 %rd, [p_data];
        ld.param.u32 %rn, [p_n];
        mov.u32 %tid, %tid.x;
        cvta.to.global.u64 %gd, %rd;
        mul.lo.s32 %toff, %tid, 4;
        mul.wide.s32 %goff, %tid, 4;
        add.s64 %gp, %gd, %goff;
        ld.global.u32 %mine, [%gp];
        st.shared.u32 [%toff], %mine;
        bar.sync;
        mov.u32 %k, 2;
    KLOOP:
        setp.gt.s32 %pkend, %k, %rn;
        @%pkend bra KEND;
        shr.u32 %j, %k, 1;
    JLOOP:
        setp.le.s32 %pjend, %j, 0;
        @%pjend bra JEND;
        xor.b32 %ixj, %tid, %j;
        // act only if ixj > tid (lower index owns the compare-exchange)
        setp.le.s32 %pskip, %ixj, %tid;
        @%pskip bra AFTER;
        mul.lo.s32 %ioff, %ixj, 4;
        ld.shared.u32 %a, [%toff];
        ld.shared.u32 %b, [%ioff];
        // direction: ascending when (tid & k) == 0, else descending
        and.b32 %tk, %tid, %k;
        setp.eq.s32 %pasc, %tk, 0;
        @%pasc bra ASC;
        // descending: swap if a < b
        setp.lt.s32 %psw, %a, %b;
        @%psw bra SWAP;
        bra AFTER;
    ASC:
        // ascending: swap if a > b
        setp.gt.s32 %psw2, %a, %b;
        @%psw2 bra SWAP;
        bra AFTER;
    SWAP:
        st.shared.u32 [%toff], %b;
        st.shared.u32 [%ioff], %a;
    AFTER:
        bar.sync;
        shr.u32 %j, %j, 1;
        bra JLOOP;
    JEND:
        shl.b32 %k, %k, 1;
        bra KLOOP;
    KEND:
        ld.shared.u32 %res, [%toff];
        st.global.u32 [%gp], %res;
        ret;
    }
"#;

#[test]
fn bitonic_sort_power_of_two_exact() {
    let n = 256usize; // power of two, single block
                      // A deterministic pseudo-shuffle with duplicates (stability irrelevant for value-equality of a sort).
    let input: Vec<i32> = (0..n)
        .map(|i| ((i * 1103515245 + 12345) % 1000) as i32)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(BITONIC_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "bitonic").unwrap();

    let d_data = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let args = vec![KernelArg::Ptr(d_data), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, 1),
        (n as u32, 1, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_data, n * 4));

    let mut want = input.clone();
    want.sort();
    assert_eq!(got, want, "bitonic sort produces the exact ascending order");
    // Sanity: it really was unsorted to begin with (guards against a no-op fake pass).
    assert_ne!(input, want, "input must start unsorted");
    assert_eq!(sink.executor().dispatches, 1);
}
