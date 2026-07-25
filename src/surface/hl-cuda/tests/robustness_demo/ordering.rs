use super::*;

// ==================================================================================================
// 2. stream_event_ordering — record an event on stream A, make stream B wait; B's dependent kernel
//    observes A's result, bit-exact. Bad event/stream handles surface honest errors.
// ==================================================================================================

#[test]
fn stream_event_ordering_dependent_work_observes_producer() {
    let n = 1024usize;
    let x: Vec<f32> = (0..n).map(|i| i as f32 * 0.25 - 7.0).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(AFFINE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "affine").unwrap();

    let stream_a: Stream = ctx.streams.create();
    let stream_b: Stream = ctx.streams.create();
    let ev: Event = ctx.event_create();

    // Producer on stream A: async-upload x, then r = 2*x + 0.
    let d_in = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    transfer::memcpy_htod_async(&mut ctx, &mut sink, stream_a, d_in, &f32s_to_bytes(&x)).unwrap();
    let d_r = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let prod_args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_r),
        sc_f32(2.0),
        sc_f32(0.0),
        sc_i32(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (4, 1, 1),
        (256, 1, 1),
        &prod_args,
    )
    .unwrap();

    // Record the producer's completion on stream A; stream B must wait for it before its consumer runs.
    ctx.event_record(ev, stream_a).unwrap();
    assert!(
        ctx.event_query(ev).unwrap(),
        "event is complete after record (synchronous model)"
    );
    ctx.stream_wait_event(stream_b, ev).unwrap();

    // Consumer on stream B: o = 1*r + 1 = r + 1. If ordering were NOT honored, r would still be zero here.
    let d_out = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let cons_args = vec![
        KernelArg::Ptr(d_r),
        KernelArg::Ptr(d_out),
        sc_f32(1.0),
        sc_f32(1.0),
        sc_i32(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (4, 1, 1),
        (256, 1, 1),
        &cons_args,
    )
    .unwrap();

    ctx.event_synchronize(ev).unwrap();
    ctx.synchronize_stream(&mut sink, stream_b).unwrap();

    let got =
        bytes_to_f32s(&transfer::read_dtoh_async(&ctx, &mut sink, stream_b, d_out, n * 4).unwrap());
    let want: Vec<f32> = x.iter().map(|v| (2.0f32 * v) + 1.0).collect();
    assert_eq!(
        got, want,
        "consumer on B observed producer-on-A's result: o = 2*x + 1"
    );

    // Honest handle validation: a bogus event / stream handle is a hard error, never a silent success.
    assert!(
        ctx.event_record(Event(9999), stream_a).is_err(),
        "record on bad event errors"
    );
    assert!(
        ctx.event_record(ev, Stream(9999)).is_err(),
        "record on bad stream errors"
    );
    assert!(
        ctx.stream_wait_event(stream_b, Event(9999)).is_err(),
        "wait on bad event errors"
    );
    assert!(
        ctx.event_query(Event(9999)).is_err(),
        "query on bad event errors"
    );

    // Clean teardown validates the destroy path too.
    ctx.event_destroy(ev).unwrap();
    assert!(ctx.event_destroy(ev).is_err(), "double-destroy is rejected");
}
