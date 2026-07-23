use super::*;

const MERGE_PASS_PTX: &str = r#"
    .visible .entry merge_pass(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_n,
        .param .u32 p_runlen
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rrl, [p_runlen];
        mad.lo.s32 %gid, %ctaid.x, %ntid.x, %tid.x;
        shl.b32 %tworl, %rrl, 1;
        mul.lo.s32 %ls, %gid, %tworl;
        // if left_start >= n: nothing to do
        setp.ge.s32 %pdone, %ls, %rn;
        @%pdone bra DONE;
        // mid = min(ls + runlen, n)
        add.s32 %mid, %ls, %rrl;
        setp.le.s32 %pm, %mid, %rn;
        @%pm bra MIDOK;
        mov.u32 %mid, %rn;
    MIDOK:
        // rend = min(ls + 2*runlen, n)
        add.s32 %rend, %ls, %tworl;
        setp.le.s32 %pr, %rend, %rn;
        @%pr bra RENDOK;
        mov.u32 %rend, %rn;
    RENDOK:
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gout, %rout;
        mov.u32 %i, %ls;
        mov.u32 %j, %mid;
        mov.u32 %k, %ls;
    MERGE:
        setp.ge.s32 %pil, %i, %mid;
        @%pil bra DRAINR;
        setp.ge.s32 %pjr, %j, %rend;
        @%pjr bra DRAINL;
        // a = in[i], b = in[j]
        mul.wide.s32 %io, %i, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %a, [%ip];
        mul.wide.s32 %jo, %j, 4;
        add.s64 %jp, %gin, %jo;
        ld.global.u32 %b, [%jp];
        mul.wide.s32 %ko, %k, 4;
        add.s64 %kp, %gout, %ko;
        setp.le.u32 %ple, %a, %b;
        @%ple bra TAKEL;
        // take right
        st.global.u32 [%kp], %b;
        add.s32 %j, %j, 1;
        bra ADVK;
    TAKEL:
        st.global.u32 [%kp], %a;
        add.s32 %i, %i, 1;
    ADVK:
        add.s32 %k, %k, 1;
        bra MERGE;
    DRAINL:
        // copy remaining left [i, mid)
        setp.ge.s32 %pdl, %i, %mid;
        @%pdl bra DONE;
        mul.wide.s32 %io2, %i, 4;
        add.s64 %ip2, %gin, %io2;
        ld.global.u32 %lv, [%ip2];
        mul.wide.s32 %ko2, %k, 4;
        add.s64 %kp2, %gout, %ko2;
        st.global.u32 [%kp2], %lv;
        add.s32 %i, %i, 1;
        add.s32 %k, %k, 1;
        bra DRAINL;
    DRAINR:
        // copy remaining right [j, rend)
        setp.ge.s32 %pdr, %j, %rend;
        @%pdr bra DONE;
        mul.wide.s32 %jo2, %j, 4;
        add.s64 %jp2, %gin, %jo2;
        ld.global.u32 %rv, [%jp2];
        mul.wide.s32 %ko3, %k, 4;
        add.s64 %kp3, %gout, %ko3;
        st.global.u32 [%kp3], %rv;
        add.s32 %j, %j, 1;
        add.s32 %k, %k, 1;
        bra DRAINR;
    DONE:
        ret;
    }
"#;

#[test]
fn merge_sort_multiblock_u32_exact() {
    let n = 1000usize; // NOT a power of two → partial final runs exercise the min() clamps
    let block = 128u32;
    // A deterministic pseudo-shuffle including values ABOVE i32::MAX (so unsigned compare is load-bearing).
    let input: Vec<u32> = (0..n)
        .map(|i| {
            let base = ((i as u64 * 2654435761 + 1013904223) & 0xFFFF_FFFF) as u32;
            if i % 5 == 0 {
                base | 0x8000_0000 // force some keys into the high (unsigned-only) half
            } else {
                base & 0x7FFF_FFFF
            }
        })
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(MERGE_PASS_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "merge_pass").unwrap();

    let mut buf_a = upload(&mut sink, &mut ctx, &u32s_to_bytes(&input));
    let mut buf_b = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();

    let mut passes = 0u32;
    let mut runlen = 1u32;
    while (runlen as usize) < n {
        let tasks = (n as u32 + 2 * runlen - 1) / (2 * runlen);
        let grid = (tasks + block - 1) / block;
        let args = vec![
            KernelArg::Ptr(buf_a),
            KernelArg::Ptr(buf_b),
            sc(n as i32),
            sc(runlen as i32),
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
        std::mem::swap(&mut buf_a, &mut buf_b); // output becomes the input of the next pass
        runlen <<= 1;
        passes += 1;
    }

    let got = bytes_to_u32s(&readback(&mut sink, &ctx, buf_a, n * 4));

    let mut want = input.clone();
    want.sort_unstable();
    assert_eq!(
        got, want,
        "multi-block merge sort produces the exact ascending order"
    );
    // Anti-false-pass: input really was unsorted, and the result is a genuine permutation of the input.
    assert_ne!(input, want, "input must start unsorted");
    let mut got_sorted = got.clone();
    got_sorted.sort_unstable();
    assert_eq!(
        got_sorted, want,
        "output is a permutation of the input (no keys lost/duplicated)"
    );
    assert!(
        want.iter().any(|&k| k > i32::MAX as u32),
        "high-half keys present → unsigned compare tested"
    );
    assert_eq!(passes, 10, "ceil(log2(1000)) = 10 ping-pong passes");
    assert_eq!(
        sink.executor().dispatches,
        10,
        "one dispatch per merge pass"
    );
}

// ==================================================================================================
// 2. dft_fixed_point — fixed-point Discrete Fourier Transform of an N=16 real signal. Twiddles are a
//    precomputed INTEGER table `cosT[m] = round(cos(2π·m/N)·Q)`, `sinT[m] = round(sin(2π·m/N)·Q)` with a
//    fixed-point scale Q; each output bin k reads `cosT[(k·n) & (N−1)]` (power-of-two modular index — the
//    interpreter models no integer modulo). One thread per bin over a multi-block grid.
//        X_re[k] = Σ_n x[n]·cosT[(k·n)&(N−1)]        X_im[k] = −Σ_n x[n]·sinT[(k·n)&(N−1)]
// ==================================================================================================
