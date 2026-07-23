use super::*;

const DFT_PTX: &str = r#"
    .visible .entry dft(
        .param .u64 p_x,
        .param .u64 p_cos,
        .param .u64 p_sin,
        .param .u64 p_re,
        .param .u64 p_im,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rx, [p_x];
        ld.param.u64 %rc, [p_cos];
        ld.param.u64 %rs, [p_sin];
        ld.param.u64 %rre, [p_re];
        ld.param.u64 %rim, [p_im];
        ld.param.u32 %rn, [p_n];
        mad.lo.s32 %k, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %k, %rn;
        @%pdone bra DONE;
        cvta.to.global.u64 %gx, %rx;
        cvta.to.global.u64 %gc, %rc;
        cvta.to.global.u64 %gs, %rs;
        sub.s32 %nm1, %rn, 1;
        mov.u32 %re, 0;
        mov.u32 %im, 0;
        mov.u32 %n, 0;
    LOOP:
        setp.ge.s32 %pl, %n, %rn;
        @%pl bra STORE;
        // idx = (k*n) & (N-1)
        mul.lo.s32 %kn, %k, %n;
        and.b32 %idx, %kn, %nm1;
        // xv = x[n]
        mul.wide.s32 %xo, %n, 4;
        add.s64 %xp, %gx, %xo;
        ld.global.u32 %xv, [%xp];
        // cosv = cosT[idx], sinv = sinT[idx]
        mul.wide.s32 %co, %idx, 4;
        add.s64 %cp, %gc, %co;
        ld.global.u32 %cosv, [%cp];
        add.s64 %sp, %gs, %co;
        ld.global.u32 %sinv, [%sp];
        mad.lo.s32 %re, %xv, %cosv, %re;
        mad.lo.s32 %im, %xv, %sinv, %im;
        add.s32 %n, %n, 1;
        bra LOOP;
    STORE:
        cvta.to.global.u64 %gre, %rre;
        cvta.to.global.u64 %gim, %rim;
        mul.wide.s32 %ko, %k, 4;
        add.s64 %rep, %gre, %ko;
        st.global.u32 [%rep], %re;
        // imag bin = -Σ x·sin
        sub.s32 %nim, 0, %im;
        add.s64 %imp, %gim, %ko;
        st.global.u32 [%imp], %nim;
    DONE:
        ret;
    }
"#;

#[test]
fn dft_fixed_point_integer_twiddle_exact() {
    let n = 16usize;
    let q = 256i64; // fixed-point scale
                    // Integer twiddle tables (bit-exact: the CPU reference reads the SAME rounded integers).
    let cos_t: Vec<i32> = (0..n)
        .map(|m| {
            ((2.0 * std::f64::consts::PI * m as f64 / n as f64).cos() * q as f64).round() as i32
        })
        .collect();
    let sin_t: Vec<i32> = (0..n)
        .map(|m| {
            ((2.0 * std::f64::consts::PI * m as f64 / n as f64).sin() * q as f64).round() as i32
        })
        .collect();
    // A non-trivial small integer signal (a couple of tones + DC), values in a modest range.
    let x: Vec<i32> = (0..n)
        .map(|i| 3 + (i as i32 % 4) - ((i as i32 / 4) % 3) + 2 * ((i as i32) % 2))
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(DFT_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "dft").unwrap();

    let d_x = upload(&mut sink, &mut ctx, &i32s_to_bytes(&x));
    let d_cos = upload(&mut sink, &mut ctx, &i32s_to_bytes(&cos_t));
    let d_sin = upload(&mut sink, &mut ctx, &i32s_to_bytes(&sin_t));
    let d_re = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_im = alloc_zeroed_i32(&mut sink, &mut ctx, n);

    let block = 8u32;
    let grid = (n as u32 + block - 1) / block; // 2 blocks
    let args = vec![
        KernelArg::Ptr(d_x),
        KernelArg::Ptr(d_cos),
        KernelArg::Ptr(d_sin),
        KernelArg::Ptr(d_re),
        KernelArg::Ptr(d_im),
        sc(n as i32),
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

    let got_re = bytes_to_i32s(&readback(&mut sink, &ctx, d_re, n * 4));
    let got_im = bytes_to_i32s(&readback(&mut sink, &ctx, d_im, n * 4));

    // CPU reference: the identical integer DFT with the identical rounded twiddle table.
    let mut want_re = vec![0i32; n];
    let mut want_im = vec![0i32; n];
    for k in 0..n {
        let mut re = 0i32;
        let mut im = 0i32;
        for nn in 0..n {
            let idx = (k * nn) & (n - 1);
            re = re.wrapping_add(x[nn].wrapping_mul(cos_t[idx]));
            im = im.wrapping_add(x[nn].wrapping_mul(sin_t[idx]));
        }
        want_re[k] = re;
        want_im[k] = -im;
    }
    assert_eq!(got_re, want_re, "fixed-point DFT real bins exact");
    assert_eq!(got_im, want_im, "fixed-point DFT imag bins exact");
    // Anti-false-pass: DC bin (k=0) is Q·Σx (all twiddles cos=Q, sin=0), and the spectrum is non-degenerate.
    assert_eq!(
        want_re[0],
        q as i32 * x.iter().sum::<i32>(),
        "k=0 real bin = Q·Σx"
    );
    assert_eq!(want_im[0], 0, "k=0 imag bin = 0");
    assert!(
        want_im.iter().any(|&v| v != 0),
        "spectrum has non-zero imaginary content"
    );
}

// ==================================================================================================
// 3. bfs_frontier — one BFS level-expansion step over a SYMMETRIC CSR graph, pull model. One thread per
//    vertex v: if v is unvisited (`dist[v] == -1`) and any neighbor u is on the current level
//    (`dist[u] == level`), then v joins the next frontier: `dist[v] = level+1`, `frontier[v] = 1`, and a
//    cross-block `red.global.add` bumps the frontier count. Each thread owns its own v (no write race on
//    `dist`); the count atomic is the only cross-block contention. Irregular CSR neighbor gather.
// ==================================================================================================
