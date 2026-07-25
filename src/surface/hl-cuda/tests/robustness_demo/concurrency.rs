use super::*;

// ==================================================================================================
// 6. concurrent_kernels_determinism — kernels across multiple streams produce identical bit-exact
//    output regardless of the order they are issued in.
// ==================================================================================================

/// Run three independent `iscale` kernels (one per stream/buffer) with the per-stream coefficients
/// `coeffs[s] = (k, c)`, issuing them in the order given by `order`. Returns the three output arrays.
fn run_three_streams(input: &[i32], coeffs: [(i32, i32); 3], order: [usize; 3]) -> [Vec<i32>; 3] {
    let n = input.len();
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(ISCALE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "iscale").unwrap();

    let streams: Vec<Stream> = (0..3).map(|_| ctx.streams.create()).collect();
    let d_in: Vec<DevicePtr> = (0..3)
        .map(|_| upload(&mut sink, &mut ctx, &i32s_to_bytes(input)))
        .collect();
    let d_out: Vec<DevicePtr> = (0..3)
        .map(|_| allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap())
        .collect();

    // Issue the three kernels in the requested interleaving. A deterministic engine must not care.
    for &s in &order {
        let (k, c) = coeffs[s];
        transfer::memcpy_htod_async(
            &mut ctx,
            &mut sink,
            streams[s],
            d_in[s],
            &i32s_to_bytes(input),
        )
        .unwrap();
        let args = vec![
            KernelArg::Ptr(d_in[s]),
            KernelArg::Ptr(d_out[s]),
            sc_i32(k),
            sc_i32(c),
            sc_i32(n as i32),
        ];
        launch::launch(&mut ctx, &mut sink, func, (16, 1, 1), (128, 1, 1), &args).unwrap();
    }
    for &s in &streams {
        ctx.synchronize_stream(&mut sink, s).unwrap();
    }

    [
        bytes_to_i32s(&readback(&mut sink, &ctx, d_out[0], n * 4)),
        bytes_to_i32s(&readback(&mut sink, &ctx, d_out[1], n * 4)),
        bytes_to_i32s(&readback(&mut sink, &ctx, d_out[2], n * 4)),
    ]
}

#[test]
fn concurrent_kernels_are_order_independent_and_deterministic() {
    let n = 2048usize;
    let input: Vec<i32> = (0..n).map(|i| i as i32 - 1024).collect();
    let coeffs = [(2, 1), (3, -5), (7, 100)];

    // Same work, two different issue interleavings.
    let forward = run_three_streams(&input, coeffs, [0, 1, 2]);
    let reverse = run_three_streams(&input, coeffs, [2, 1, 0]);

    // Independent CPU reference per stream.
    let cpu: [Vec<i32>; 3] = std::array::from_fn(|s| {
        let (k, c) = coeffs[s];
        input
            .iter()
            .map(|v| v.wrapping_mul(k).wrapping_add(c))
            .collect()
    });

    for s in 0..3 {
        assert_eq!(
            forward[s], cpu[s],
            "stream {s} forward-issue output matches CPU reference"
        );
        assert_eq!(
            reverse[s], cpu[s],
            "stream {s} reverse-issue output matches CPU reference"
        );
        assert_eq!(
            forward[s], reverse[s],
            "stream {s} output is identical regardless of issue order"
        );
    }
}
