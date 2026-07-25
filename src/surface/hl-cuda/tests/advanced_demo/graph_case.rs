use super::*;

// ==================================================================================================
// 3. cuda_graph — build a kernel sequence into a graph, instantiate + launch; replay == eager, exact.
// ==================================================================================================

/// saxpy `y[i] = a*x[i] + y[i]` — the kernel node the graph replays.
const SAXPY_PTX: &str = r#"
    .visible .entry saxpy(
        .param .u64 saxpy_x,
        .param .u64 saxpy_y,
        .param .f32 saxpy_a,
        .param .u32 saxpy_n
    )
    {
        ld.param.u64  %rdx, [saxpy_x];
        ld.param.u64  %rdy, [saxpy_y];
        ld.param.f32  %fa,  [saxpy_a];
        ld.param.u32  %rn,  [saxpy_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %gx, %rdx;
        cvta.to.global.u64 %gy, %rdy;
        mul.wide.s32  %off, %ri, 4;
        add.s64       %px, %gx, %off;
        add.s64       %py, %gy, %off;
        ld.global.f32 %vx, [%px];
        ld.global.f32 %vy, [%py];
        fma.rn.f32    %vr, %fa, %vx, %vy;
        st.global.f32 [%py], %vr;
    DONE:
        ret;
    }
"#;

#[test]
fn cuda_graph_replay_equals_eager_sequence_exact() {
    let n = 768usize;
    let alpha = 1.75f32;
    let x: Vec<f32> = (0..n).map(|i| (i as f32) * 0.25 - 40.0).collect();
    let y0: Vec<f32> = (0..n).map(|i| (i as f32) * -0.5 + 3.0).collect();
    let x_bytes = f32s_to_bytes(&x);
    let y_bytes = f32s_to_bytes(&y0);

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SAXPY_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "saxpy").unwrap();
    let grid = (6u32, 1, 1);
    let block = (128u32, 1, 1);

    // ---- eager reference: upload x, upload y, saxpy — run directly through the services ----
    let ex = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let ey = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, ex, &x_bytes).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, ey, &y_bytes).unwrap();
    let eager_args = vec![
        KernelArg::Ptr(ex),
        KernelArg::Ptr(ey),
        KernelArg::Scalar(alpha.to_le_bytes().to_vec()),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, grid, block, &eager_args).unwrap();
    let eager = bytes_to_f32s(&readback(&mut sink, &ctx, ey, n * 4));

    // ---- graph: identical sequence built as nodes, instantiated, launched ----
    let gx = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let gy = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let mut g = graph::graph_create();
    graph::add_memcpy_htod_node(&mut g, gx, &x_bytes);
    graph::add_memcpy_htod_node(&mut g, gy, &y_bytes);
    let graph_args = vec![
        KernelArg::Ptr(gx),
        KernelArg::Ptr(gy),
        KernelArg::Scalar(alpha.to_le_bytes().to_vec()),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    graph::add_kernel_node(&mut g, func, grid, block, graph_args);
    assert_eq!(
        g.nodes.len(),
        3,
        "graph has 3 nodes: two H2D copies + one kernel"
    );

    let exec = g.instantiate();
    graph::launch_graph(&mut ctx, &mut sink, &exec).unwrap();
    let graphed = bytes_to_f32s(&readback(&mut sink, &ctx, gy, n * 4));

    // Independent CPU reference (same fma order).
    let want: Vec<f32> = x
        .iter()
        .zip(&y0)
        .map(|(xi, yi)| alpha.mul_add(*xi, *yi))
        .collect();
    assert_eq!(eager, want, "eager saxpy sequence matches CPU reference");
    assert_eq!(graphed, want, "graph-replayed saxpy matches CPU reference");
    assert_eq!(
        graphed, eager,
        "graph replay is bit-identical to the eager sequence"
    );

    // Re-launching the graph re-runs its H2D nodes (resetting y) then saxpy → idempotent bit-exact.
    graph::launch_graph(&mut ctx, &mut sink, &exec).unwrap();
    let relaunched = bytes_to_f32s(&readback(&mut sink, &ctx, gy, n * 4));
    assert_eq!(
        relaunched, want,
        "second graph launch reproduces the same result (idempotent replay)"
    );
}
