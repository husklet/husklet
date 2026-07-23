use super::*;

// ==================================================================================================
// 4. convolution — stencils with exact integer arithmetic.
//    (a) conv1d_box: radius-2 box SUM with a shared-memory HALO tile (block loads BLOCK + 2·R elements
//        into `.shared`, bar.sync, each lane sums its 5-wide window from shared), zero-padded boundary.
//    (b) conv2d_box: 3×3 box blur SUM, zero-padded boundary, direct 2D global gather.
//    (c) sobel3x3: |Gx| + |Gy| with the standard Sobel kernels, zero-padded boundary.
// ==================================================================================================

const CONV1D_PTX: &str = r#"
    .visible .entry conv1d_box(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        .shared .align 4 .b32 tile[68];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rn, [p_n];
        mov.u32 %tid, %tid.x;
        mov.u32 %bd, %ntid.x;
        mov.u32 %cx, %ctaid.x;
        mad.lo.s32 %gid, %cx, %bd, %tid;
        cvta.to.global.u64 %gin, %rin;
        // center: tile[tid+2] = load(gid)
        add.s32 %cslot, %tid, 2;
        mul.lo.s32 %csb, %cslot, 4;
        mov.u32 %cv, 0;
        setp.lt.s32 %cneg, %gid, 0;
        @%cneg bra CSTORE;
        setp.ge.s32 %coob, %gid, %rn;
        @%coob bra CSTORE;
        mul.wide.s32 %co, %gid, 4;
        add.s64 %cp, %gin, %co;
        ld.global.u32 %cv, [%cp];
    CSTORE:
        st.shared.u32 [%csb], %cv;
        // left halo: lanes 0..2 load gid-2 into tile[tid]
        setp.ge.s32 %pnl, %tid, 2;
        @%pnl bra RIGHT;
        sub.s32 %lg, %gid, 2;
        mul.lo.s32 %lsb, %tid, 4;
        mov.u32 %lv, 0;
        setp.lt.s32 %lneg, %lg, 0;
        @%lneg bra LSTORE;
        setp.ge.s32 %loob, %lg, %rn;
        @%loob bra LSTORE;
        mul.wide.s32 %lo, %lg, 4;
        add.s64 %lp, %gin, %lo;
        ld.global.u32 %lv, [%lp];
    LSTORE:
        st.shared.u32 [%lsb], %lv;
    RIGHT:
        // right halo: lanes bd-2..bd load gid+2 into tile[tid+4]
        sub.s32 %rthresh, %bd, 2;
        setp.lt.s32 %pnr, %tid, %rthresh;
        @%pnr bra SYNC;
        add.s32 %rg, %gid, 2;
        add.s32 %rslot, %tid, 4;
        mul.lo.s32 %rsb, %rslot, 4;
        mov.u32 %rv, 0;
        setp.lt.s32 %rneg, %rg, 0;
        @%rneg bra RSTORE;
        setp.ge.s32 %roob, %rg, %rn;
        @%roob bra RSTORE;
        mul.wide.s32 %ro, %rg, 4;
        add.s64 %rp, %gin, %ro;
        ld.global.u32 %rv, [%rp];
    RSTORE:
        st.shared.u32 [%rsb], %rv;
    SYNC:
        bar.sync;
        // out[gid] = sum tile[tid .. tid+4]  (5-wide window centered at tile[tid+2])
        setp.ge.s32 %poob, %gid, %rn;
        @%poob bra DONE;
        mov.u32 %acc, 0;
        mul.lo.s32 %b0, %tid, 4;
        ld.shared.u32 %w0, [%b0];
        add.s32 %acc, %acc, %w0;
        ld.shared.u32 %w1, [%b0+4];
        add.s32 %acc, %acc, %w1;
        ld.shared.u32 %w2, [%b0+8];
        add.s32 %acc, %acc, %w2;
        ld.shared.u32 %w3, [%b0+12];
        add.s32 %acc, %acc, %w3;
        ld.shared.u32 %w4, [%b0+16];
        add.s32 %acc, %acc, %w4;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %oo, %gid, 4;
        add.s64 %op, %gout, %oo;
        st.global.u32 [%op], %acc;
    DONE:
        ret;
    }
"#;

#[test]
fn conv1d_box_shared_halo_exact() {
    let block = 64usize;
    let n = 200usize; // remainder: 200 is not a multiple of 64 → last block partial
    let radius = 2i32;
    let input: Vec<i32> = (0..n).map(|i| (i as i32 * 3 + 1) % 37).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(CONV1D_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "conv1d_box").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let grid = ((n + block - 1) / block) as u32; // 4 blocks

    let args = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_out), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (grid, 1, 1),
        (block as u32, 1, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, n * 4));

    // Reference: zero-padded radius-2 box sum.
    let mut want = vec![0i32; n];
    for i in 0..n as i32 {
        let mut acc = 0i32;
        for d in -radius..=radius {
            let j = i + d;
            if j >= 0 && j < n as i32 {
                acc += input[j as usize];
            }
        }
        want[i as usize] = acc;
    }
    assert_eq!(
        got, want,
        "1D box convolution (shared-halo), every element exact"
    );
}

const CONV2D_PTX: &str = r#"
    .visible .entry conv2d_box(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_w,
        .param .u32 p_h
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rw, [p_w];
        ld.param.u32 %rh, [p_h];
        mov.u32 %tx, %tid.x;
        mov.u32 %ty, %tid.y;
        mov.u32 %bx, %ctaid.x;
        mov.u32 %by, %ctaid.y;
        mov.u32 %nx, %ntid.x;
        mov.u32 %ny, %ntid.y;
        mad.lo.s32 %x, %bx, %nx, %tx;
        mad.lo.s32 %y, %by, %ny, %ty;
        setp.ge.s32 %pxx, %x, %rw;
        @%pxx bra DONE;
        setp.ge.s32 %pyy, %y, %rh;
        @%pyy bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mov.u32 %acc, 0;
        // dy = -1..1
        mov.u32 %dy, -1;
    YLOOP:
        setp.gt.s32 %pdy, %dy, 1;
        @%pdy bra YEND;
        add.s32 %yy, %y, %dy;
        // dx = -1..1
        mov.u32 %dx, -1;
    XLOOP:
        setp.gt.s32 %pdx, %dx, 1;
        @%pdx bra XEND;
        add.s32 %xx, %x, %dx;
        // bounds check (zero pad): 0<=xx<w && 0<=yy<h
        setp.lt.s32 %bxn, %xx, 0;
        @%bxn bra XNEXT;
        setp.ge.s32 %bxo, %xx, %rw;
        @%bxo bra XNEXT;
        setp.lt.s32 %byn, %yy, 0;
        @%byn bra XNEXT;
        setp.ge.s32 %byo, %yy, %rh;
        @%byo bra XNEXT;
        mad.lo.s32 %idx, %yy, %rw, %xx;
        mul.wide.s32 %io, %idx, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %pv, [%ip];
        add.s32 %acc, %acc, %pv;
    XNEXT:
        add.s32 %dx, %dx, 1;
        bra XLOOP;
    XEND:
        add.s32 %dy, %dy, 1;
        bra YLOOP;
    YEND:
        cvta.to.global.u64 %gout, %rout;
        mad.lo.s32 %oidx, %y, %rw, %x;
        mul.wide.s32 %oo, %oidx, 4;
        add.s64 %op, %gout, %oo;
        st.global.u32 [%op], %acc;
    DONE:
        ret;
    }
"#;

#[test]
fn conv2d_box_blur_exact() {
    let (w, h) = (20usize, 12usize); // remainder on both axes vs 16×16 blocks
    let img: Vec<i32> = (0..w * h).map(|i| (i as i32 * 7 + 5) % 53).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(CONV2D_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "conv2d_box").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&img));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, w * h);
    let gx = ((w + 15) / 16) as u32;
    let gy = ((h + 15) / 16) as u32;

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_out),
        sc(w as i32),
        sc(h as i32),
    ];
    launch::launch(&mut ctx, &mut sink, func, (gx, gy, 1), (16, 16, 1), &args).unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, w * h * 4));

    // Reference: zero-padded 3×3 box sum.
    let mut want = vec![0i32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut acc = 0i32;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (xx, yy) = (x + dx, y + dy);
                    if xx >= 0 && xx < w as i32 && yy >= 0 && yy < h as i32 {
                        acc += img[(yy * w as i32 + xx) as usize];
                    }
                }
            }
            want[(y * w as i32 + x) as usize] = acc;
        }
    }
    assert_eq!(got, want, "2D 3×3 box blur, every pixel exact");
}

const SOBEL_PTX: &str = r#"
    .visible .entry sobel3x3(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_w,
        .param .u32 p_h
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rw, [p_w];
        ld.param.u32 %rh, [p_h];
        mov.u32 %tx, %tid.x;
        mov.u32 %ty, %tid.y;
        mov.u32 %bx, %ctaid.x;
        mov.u32 %by, %ctaid.y;
        mov.u32 %nx, %ntid.x;
        mov.u32 %ny, %ntid.y;
        mad.lo.s32 %x, %bx, %nx, %tx;
        mad.lo.s32 %y, %by, %ny, %ty;
        setp.ge.s32 %pxx, %x, %rw;
        @%pxx bra DONE;
        setp.ge.s32 %pyy, %y, %rh;
        @%pyy bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mov.u32 %gx, 0;
        mov.u32 %gy, 0;
        mov.u32 %dy, -1;
    YLOOP:
        setp.gt.s32 %pdy, %dy, 1;
        @%pdy bra YEND;
        add.s32 %yy, %y, %dy;
        mov.u32 %dx, -1;
    XLOOP:
        setp.gt.s32 %pdx, %dx, 1;
        @%pdx bra XEND;
        add.s32 %xx, %x, %dx;
        // fetch pixel (zero pad)
        mov.u32 %pv, 0;
        setp.lt.s32 %bxn, %xx, 0;
        @%bxn bra HAVE;
        setp.ge.s32 %bxo, %xx, %rw;
        @%bxo bra HAVE;
        setp.lt.s32 %byn, %yy, 0;
        @%byn bra HAVE;
        setp.ge.s32 %byo, %yy, %rh;
        @%byo bra HAVE;
        mad.lo.s32 %idx, %yy, %rw, %xx;
        mul.wide.s32 %io, %idx, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %pv, [%ip];
    HAVE:
        // wx = dx * (2 - |dy|) ;  wy = dy * (2 - |dx|)   (separable Sobel weights)
        // |dy|
        mov.u32 %ady, %dy;
        setp.ge.s32 %pdyp, %dy, 0;
        @%pdyp bra ADYOK;
        sub.s32 %ady, 0, %dy;
    ADYOK:
        sub.s32 %wcoefx, 2, %ady;
        mul.lo.s32 %wx, %dx, %wcoefx;
        mov.u32 %adx, %dx;
        setp.ge.s32 %pdxp, %dx, 0;
        @%pdxp bra ADXOK;
        sub.s32 %adx, 0, %dx;
    ADXOK:
        sub.s32 %wcoefy, 2, %adx;
        mul.lo.s32 %wy, %dy, %wcoefy;
        mad.lo.s32 %gx, %pv, %wx, %gx;
        mad.lo.s32 %gy, %pv, %wy, %gy;
        add.s32 %dx, %dx, 1;
        bra XLOOP;
    XEND:
        add.s32 %dy, %dy, 1;
        bra YLOOP;
    YEND:
        // |gx| + |gy|
        setp.ge.s32 %pgxp, %gx, 0;
        @%pgxp bra GXOK;
        sub.s32 %gx, 0, %gx;
    GXOK:
        setp.ge.s32 %pgyp, %gy, 0;
        @%pgyp bra GYOK;
        sub.s32 %gy, 0, %gy;
    GYOK:
        add.s32 %mag, %gx, %gy;
        cvta.to.global.u64 %gout, %rout;
        mad.lo.s32 %oidx, %y, %rw, %x;
        mul.wide.s32 %oo, %oidx, 4;
        add.s64 %op, %gout, %oo;
        st.global.u32 [%op], %mag;
    DONE:
        ret;
    }
"#;

#[test]
fn sobel3x3_gradient_magnitude_exact() {
    let (w, h) = (18usize, 14usize);
    let img: Vec<i32> = (0..w * h).map(|i| (i as i32 * 11 + 3) % 41).collect();

    // Separable Sobel weights: wx[dy][dx] = dx*(2-|dy|), wy[dy][dx] = dy*(2-|dx|).
    let sobel_x = [[-1, 0, 1], [-2, 0, 2], [-1, 0, 1]];
    let sobel_y = [[-1, -2, -1], [0, 0, 0], [1, 2, 1]];

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SOBEL_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "sobel3x3").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&img));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, w * h);
    let gxg = ((w + 15) / 16) as u32;
    let gyg = ((h + 15) / 16) as u32;

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_out),
        sc(w as i32),
        sc(h as i32),
    ];
    launch::launch(&mut ctx, &mut sink, func, (gxg, gyg, 1), (16, 16, 1), &args).unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, w * h * 4));

    // Reference: |Gx| + |Gy| with zero-padded boundary and the canonical Sobel kernels.
    let mut want = vec![0i32; w * h];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let (mut sx, mut sy) = (0i32, 0i32);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (xx, yy) = (x + dx, y + dy);
                    let p = if xx >= 0 && xx < w as i32 && yy >= 0 && yy < h as i32 {
                        img[(yy * w as i32 + xx) as usize]
                    } else {
                        0
                    };
                    sx += p * sobel_x[(dy + 1) as usize][(dx + 1) as usize];
                    sy += p * sobel_y[(dy + 1) as usize][(dx + 1) as usize];
                }
            }
            want[(y * w as i32 + x) as usize] = sx.abs() + sy.abs();
        }
    }
    assert_eq!(got, want, "3×3 Sobel |Gx|+|Gy|, every pixel exact");
}
