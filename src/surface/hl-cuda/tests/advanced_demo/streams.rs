use super::*;

// ==================================================================================================
// 5. multi_stream — overlapping async copies + kernels across four streams; deterministic + exact.
// ==================================================================================================

/// `out[i] = k*in[i] + c` (integer), one lane per element.
const ISCALE_PTX: &str = r#"
    .visible .entry iscale(
        .param .u64 is_in,
        .param .u64 is_out,
        .param .u32 is_k,
        .param .u32 is_c,
        .param .u32 is_n
    )
    {
        ld.param.u64  %rin, [is_in];
        ld.param.u64  %rout, [is_out];
        ld.param.u32  %rk, [is_k];
        ld.param.u32  %rc, [is_c];
        ld.param.u32  %rn, [is_n];
        mov.u32 %rnt, %ntid.x; mov.u32 %rct, %ctaid.x; mov.u32 %rtt, %tid.x;
        mad.lo.s32 %i, %rct, %rnt, %rtt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        add.s64 %pout, %gout, %off;
        ld.global.u32 %v, [%pin];
        mad.lo.s32 %r, %v, %rk, %rc;
        st.global.u32 [%pout], %r;
    DONE: ret;
    }
"#;

/// Run four streams, each doing async H2D + kernel + (implicit) into its own output buffer, issuing the
/// four in the order `order`. Returns the four output arrays concatenated head-to-tail.
fn run_four_streams(input: &[i32], coeffs: [(i32, i32); 4], order: [usize; 4]) -> Vec<Vec<i32>> {
    let n = input.len();
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(ISCALE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "iscale").unwrap();

    let streams: Vec<_> = (0..4).map(|_| ctx.streams.create()).collect();
    let in_bytes = i32s_to_bytes(input);
    let d_in: Vec<DevicePtr> = (0..4)
        .map(|_| allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap())
        .collect();
    let d_out: Vec<DevicePtr> = (0..4)
        .map(|_| allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap())
        .collect();

    // Overlapping issue: for each stream (in the given interleaving) an async H2D copy then a kernel.
    let sc = |v: i32| KernelArg::Scalar(v.to_le_bytes().to_vec());
    for &s in &order {
        transfer::memcpy_htod_async(&mut ctx, &mut sink, streams[s], d_in[s], &in_bytes).unwrap();
        let (k, c) = coeffs[s];
        let args = vec![
            KernelArg::Ptr(d_in[s]),
            KernelArg::Ptr(d_out[s]),
            sc(k),
            sc(c),
            sc(n as i32),
        ];
        launch::launch(&mut ctx, &mut sink, func, (16, 1, 1), (128, 1, 1), &args).unwrap();
    }
    for &s in &streams {
        ctx.synchronize_stream(&mut sink, s).unwrap();
    }

    (0..4)
        .map(|s| bytes_to_i32s(&readback(&mut sink, &ctx, d_out[s], n * 4)))
        .collect()
}

#[test]
fn multi_stream_overlap_is_deterministic_and_exact() {
    let n = 1500usize;
    let input: Vec<i32> = (0..n).map(|i| i as i32 - 750).collect();
    let coeffs = [(2, 1), (3, -5), (7, 100), (-4, 9)];

    // Two different issue interleavings of the identical work.
    let forward = run_four_streams(&input, coeffs, [0, 1, 2, 3]);
    let shuffled = run_four_streams(&input, coeffs, [3, 1, 0, 2]);

    // Independent CPU reference per stream.
    let cpu: Vec<Vec<i32>> = (0..4)
        .map(|s| {
            let (k, c) = coeffs[s];
            input
                .iter()
                .map(|v| v.wrapping_mul(k).wrapping_add(c))
                .collect()
        })
        .collect();

    for s in 0..4 {
        assert_eq!(
            forward[s], cpu[s],
            "stream {s} forward-issue output matches CPU reference"
        );
        assert_eq!(
            shuffled[s], cpu[s],
            "stream {s} shuffled-issue output matches CPU reference"
        );
        assert_eq!(
            forward[s], shuffled[s],
            "stream {s} output is identical regardless of issue order"
        );
    }
}
