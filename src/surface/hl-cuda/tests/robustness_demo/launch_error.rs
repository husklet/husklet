use super::*;

// ==================================================================================================
// 1. error_bad_launch — an invalid launch config returns CUDA_ERROR_INVALID_VALUE, not cudaSuccess.
//    (Regression guard for the driver faking success on a config real hardware could never dispatch.)
// ==================================================================================================

#[test]
fn error_bad_launch_returns_honest_error() {
    let n = 256usize;
    let x: Vec<f32> = (0..n).map(|i| i as f32 * 0.5).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(AFFINE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "affine").unwrap();

    let dx = upload(&mut sink, &mut ctx, &f32s_to_bytes(&x));
    let dy = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let good_args = vec![
        KernelArg::Ptr(dx),
        KernelArg::Ptr(dy),
        sc_f32(3.0),
        sc_f32(1.0),
        sc_i32(n as i32),
    ];

    // A legal launch first: proves the kernel + harness are healthy and computes a real result.
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, 1),
        (256, 1, 1),
        &good_args,
    )
    .unwrap();
    assert_eq!(sink.executor().dispatches, 1);
    let got = bytes_to_f32s(&readback(&mut sink, &ctx, dy, n * 4));
    let want: Vec<f32> = x.iter().map(|v| 3.0f32.mul_add(*v, 1.0)).collect();
    assert_eq!(
        got, want,
        "the valid launch computed the real affine result"
    );

    // maxThreadsPerBlock is 1024 on the modeled device; 32*32*2 = 2048 threads is over the limit → a real
    // driver returns CUDA_ERROR_INVALID_VALUE. It must NOT silently run (which the software oracle would
    // otherwise happily do — a fake success).
    let err = launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, 1),
        (32, 32, 2),
        &good_args,
    )
    .unwrap_err();
    assert_eq!(
        result::DriverStatus::from(&err).code(),
        result::CUDA_ERROR_INVALID_VALUE,
        "over-maxThreadsPerBlock launch → CUDA_ERROR_INVALID_VALUE"
    );
    assert_eq!(
        result::RuntimeStatus::from(&err).code(),
        result::CUDART_ERROR_INVALID_VALUE
    );
    assert_ne!(
        result::DriverStatus::from(&err).code(),
        result::CUDA_SUCCESS,
        "NOT a faked success"
    );

    // A zero block dimension is equally invalid.
    let zerr = launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, 1),
        (256, 0, 1),
        &good_args,
    )
    .unwrap_err();
    assert_eq!(
        result::DriverStatus::from(&zerr).code(),
        result::CUDA_ERROR_INVALID_VALUE
    );
    // …and so is a zero grid dimension.
    let gerr = launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (0, 1, 1),
        (256, 1, 1),
        &good_args,
    )
    .unwrap_err();
    assert_eq!(
        result::DriverStatus::from(&gerr).code(),
        result::CUDA_ERROR_INVALID_VALUE
    );

    // Crucially: none of the three rejected launches reached the sink — the dispatch count is still 1, so
    // the bad launches emitted NOTHING and computed NOTHING (no partial/garbage write into `dy`).
    assert_eq!(
        sink.executor().dispatches,
        1,
        "rejected launches emit no dispatch"
    );
    let after = bytes_to_f32s(&readback(&mut sink, &ctx, dy, n * 4));
    assert_eq!(
        after, want,
        "the output buffer is untouched by the rejected launches"
    );
}
