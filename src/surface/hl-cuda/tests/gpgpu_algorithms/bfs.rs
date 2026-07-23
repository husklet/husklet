use super::*;

const BFS_PTX: &str = r#"
    .visible .entry bfs_step(
        .param .u64 p_roff,
        .param .u64 p_col,
        .param .u64 p_dist,
        .param .u64 p_front,
        .param .u64 p_cnt,
        .param .u32 p_v,
        .param .u32 p_level
    )
    {
        ld.param.u64 %rroff, [p_roff];
        ld.param.u64 %rcol, [p_col];
        ld.param.u64 %rdist, [p_dist];
        ld.param.u64 %rfront, [p_front];
        ld.param.u64 %rcnt, [p_cnt];
        ld.param.u32 %rv, [p_v];
        ld.param.u32 %rlevel, [p_level];
        mad.lo.s32 %gid, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %gid, %rv;
        @%pdone bra DONE;
        cvta.to.global.u64 %gdist, %rdist;
        // d = dist[gid]; skip if already visited (d != -1)
        mul.wide.s32 %go, %gid, 4;
        add.s64 %dp, %gdist, %go;
        ld.global.u32 %d, [%dp];
        setp.ne.s32 %pvis, %d, -1;
        @%pvis bra DONE;
        // start = roff[gid], end = roff[gid+1]
        cvta.to.global.u64 %groff, %rroff;
        add.s64 %rp0, %groff, %go;
        ld.global.u32 %start, [%rp0];
        ld.global.u32 %end, [%rp0+4];
        cvta.to.global.u64 %gcol, %rcol;
        mov.u32 %e, %start;
    NLOOP:
        setp.ge.s32 %pn, %e, %end;
        @%pn bra DONE;
        // u = col[e]; du = dist[u]
        mul.wide.s32 %eo, %e, 4;
        add.s64 %ep, %gcol, %eo;
        ld.global.u32 %u, [%ep];
        mul.wide.s32 %uo, %u, 4;
        add.s64 %up, %gdist, %uo;
        ld.global.u32 %du, [%up];
        setp.eq.s32 %pf, %du, %rlevel;
        @%pf bra FOUND;
        add.s32 %e, %e, 1;
        bra NLOOP;
    FOUND:
        // dist[gid] = level+1 ; frontier[gid] = 1 ; atomic count++
        add.s32 %nl, %rlevel, 1;
        st.global.u32 [%dp], %nl;
        cvta.to.global.u64 %gfront, %rfront;
        add.s64 %fp, %gfront, %go;
        mov.u32 %one, 1;
        st.global.u32 [%fp], %one;
        cvta.to.global.u64 %gcnt, %rcnt;
        red.global.add.u32 [%gcnt], 1;
    DONE:
        ret;
    }
"#;

#[test]
fn bfs_frontier_expansion_step_exact() {
    // Symmetric (undirected) CSR graph, V=8.
    //   0-1 0-2 1-3 2-3 2-6 3-4 4-5 5-6 6-7
    let v = 8usize;
    let adj: Vec<Vec<i32>> = vec![
        vec![1, 2],    // 0
        vec![0, 3],    // 1
        vec![0, 3, 6], // 2
        vec![1, 2, 4], // 3
        vec![3, 5],    // 4
        vec![4, 6],    // 5
        vec![5, 2, 7], // 6
        vec![6],       // 7
    ];
    let mut row_off = vec![0i32; v + 1];
    let mut col = Vec::new();
    for u in 0..v {
        for &w in &adj[u] {
            col.push(w);
        }
        row_off[u + 1] = col.len() as i32;
    }

    let level = 0i32;
    let dist0: Vec<i32> = (0..v).map(|i| if i == 0 { 0 } else { -1 }).collect(); // BFS started at vertex 0

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(BFS_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "bfs_step").unwrap();

    let d_roff = upload(&mut sink, &mut ctx, &i32s_to_bytes(&row_off));
    let d_col = upload(&mut sink, &mut ctx, &i32s_to_bytes(&col));
    let d_dist = upload(&mut sink, &mut ctx, &i32s_to_bytes(&dist0));
    let d_front = alloc_zeroed_i32(&mut sink, &mut ctx, v);
    let d_cnt = alloc_zeroed_i32(&mut sink, &mut ctx, 1);

    let block = 4u32;
    let grid = (v as u32 + block - 1) / block; // 2 blocks
    let args = vec![
        KernelArg::Ptr(d_roff),
        KernelArg::Ptr(d_col),
        KernelArg::Ptr(d_dist),
        KernelArg::Ptr(d_front),
        KernelArg::Ptr(d_cnt),
        sc(v as i32),
        sc(level),
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

    let got_dist = bytes_to_i32s(&readback(&mut sink, &ctx, d_dist, v * 4));
    let got_front = bytes_to_i32s(&readback(&mut sink, &ctx, d_front, v * 4));
    let got_cnt = bytes_to_i32s(&readback(&mut sink, &ctx, d_cnt, 4))[0];

    // CPU reference: pull-model BFS expansion.
    let mut want_dist = dist0.clone();
    let mut want_front = vec![0i32; v];
    let mut want_cnt = 0i32;
    for x in 0..v {
        if dist0[x] != -1 {
            continue;
        }
        let mut touched = false;
        for &u in &adj[x] {
            if dist0[u as usize] == level {
                touched = true;
                break;
            }
        }
        if touched {
            want_dist[x] = level + 1;
            want_front[x] = 1;
            want_cnt += 1;
        }
    }
    assert_eq!(got_dist, want_dist, "BFS new dist array exact");
    assert_eq!(got_front, want_front, "BFS next-frontier mask exact");
    assert_eq!(
        got_cnt, want_cnt,
        "BFS frontier count exact (cross-block atomic)"
    );
    // Anti-false-pass: the step actually discovered vertices {1,2}, and a level-2 vertex (3) stays unvisited.
    assert_eq!(want_cnt, 2, "exactly vertices 1 and 2 join the frontier");
    assert_eq!(
        got_dist[3], -1,
        "vertex 3 is two hops away — still unvisited after one step"
    );
    assert_eq!(
        got_front,
        vec![0, 1, 1, 0, 0, 0, 0, 0],
        "frontier is exactly {{1,2}}"
    );
}

// ==================================================================================================
// 4. spmv_csr — sparse matrix–vector product y = A·x with A stored in CSR (`row_off`, `col`, `val`).
//    One thread per row accumulates `Σ val[e]·x[col[e]]` over its ragged row (including an EMPTY row →
//    y = 0). Irregular strided gather through `col`. Multi-block grid over rows.
// ==================================================================================================
