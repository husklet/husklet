use super::*;

// ==================================================================================================
// 3. async_overlap_correctness — async H2D + kernel + async D2H on a stream, synchronized; the final
//    host data is bit-exact. Overlap of the queued ops must not corrupt the result.
// ==================================================================================================

#[test]
fn async_overlap_produces_bit_exact_result() {
    let n = 4096usize;
    let a: Vec<i32> = (0..n).map(|i| (i as i32 * 3) - 1000).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(ISCALE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "iscale").unwrap();

    let stream: Stream = ctx.streams.create();
    let d_in = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let d_out = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();

    // async H2D → kernel (out = 5*in + 7) → async D2H, all on one stream, then a single stream barrier.
    transfer::memcpy_htod_async(&mut ctx, &mut sink, stream, d_in, &i32s_to_bytes(&a)).unwrap();
    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_out),
        sc_i32(5),
        sc_i32(7),
        sc_i32(n as i32),
    ];
    launch::launch(&mut ctx, &mut sink, func, (32, 1, 1), (128, 1, 1), &args).unwrap();
    let raw = transfer::read_dtoh_async(&ctx, &mut sink, stream, d_out, n * 4).unwrap();
    ctx.synchronize_stream(&mut sink, stream).unwrap();

    let got = bytes_to_i32s(&raw);
    let want: Vec<i32> = a
        .iter()
        .map(|v| v.wrapping_mul(5).wrapping_add(7))
        .collect();
    assert_eq!(
        got, want,
        "async H2D/kernel/D2H overlap produced the exact result, uncorrupted"
    );
    assert_eq!(sink.executor().dispatches, 1);
}
