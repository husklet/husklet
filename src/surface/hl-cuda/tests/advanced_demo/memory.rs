use super::*;

// ==================================================================================================
// 2. managed_memory — cudaMallocManaged: host writes, kernel transforms in place, host reads back.
// ==================================================================================================

/// In-place affine transform on a managed buffer: `data[i] = a*data[i] + b`.
const TRANSFORM_PTX: &str = r#"
    .visible .entry transform(
        .param .u64 tf_data,
        .param .f32 tf_a,
        .param .f32 tf_b,
        .param .u32 tf_n
    )
    {
        ld.param.u64  %rd, [tf_data];
        ld.param.f32  %fa, [tf_a];
        ld.param.f32  %fb, [tf_b];
        ld.param.u32  %rn, [tf_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %gd, %rd;
        mul.wide.s32  %off, %ri, 4;
        add.s64       %pd, %gd, %off;
        ld.global.f32 %v, [%pd];
        fma.rn.f32    %r, %fa, %v, %fb;
        st.global.f32 [%pd], %r;
    DONE:
        ret;
    }
"#;

#[test]
fn managed_memory_unified_pointer_round_trips_exact() {
    let n = 640usize;
    let (a, b) = (3.0f32, 7.0f32);
    let pattern: Vec<f32> = (0..n).map(|i| (i as f32) * 0.5 - 100.0).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    // cudaMallocManaged — a unified, host-addressable device pointer.
    let managed = allocate::mem_alloc_managed(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    assert!(
        ctx.mem.is_managed(managed),
        "the pointer is flagged managed/unified"
    );
    // A plain cudaMalloc is NOT managed — the model distinguishes the two truthfully.
    let plain = allocate::mem_alloc(&mut ctx, &mut sink, 16).unwrap();
    assert!(
        !ctx.mem.is_managed(plain),
        "a plain device allocation is not managed"
    );

    // Host writes the pattern through the unified pointer (cudaMemcpy / direct host store analogue).
    transfer::memcpy_htod(&mut ctx, &mut sink, managed, &f32s_to_bytes(&pattern)).unwrap();

    // A kernel reads + transforms the SAME managed allocation in place.
    let module = ctx.load_module(TRANSFORM_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "transform").unwrap();
    let args = vec![
        KernelArg::Ptr(managed),
        KernelArg::Scalar(a.to_le_bytes().to_vec()),
        KernelArg::Scalar(b.to_le_bytes().to_vec()),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, (5, 1, 1), (128, 1, 1), &args).unwrap();

    // Host reads back through the unified pointer — coherence: it observes the kernel's writes exactly.
    let got = bytes_to_f32s(&readback(&mut sink, &ctx, managed, n * 4));
    let want: Vec<f32> = pattern.iter().map(|x| a.mul_add(*x, b)).collect();
    assert_eq!(
        got, want,
        "managed pointer round-trips host→kernel→host, all {n} elements exact"
    );
}
