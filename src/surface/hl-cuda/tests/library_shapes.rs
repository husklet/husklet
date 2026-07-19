//! CUDA **real-library-shape** demo battery — the exact kernel shapes production ML/HPC libraries
//! (cuBLAS strided-batched GEMM, cuDNN conv/pool/softmax/normalization, embedding/im2col front-ends)
//! actually dispatch, each driven through the REAL hl-cuda PTX front-end → the reference
//! [`CpuExecutor`] kernel-IR interpreter → device readback → asserted **bit-exact, element by element**
//! against an independent CPU reference computed in the test.
//!
//! Every kernel exercises the genuine structure of its library counterpart — batch strides, NCHW /
//! im2col indexing with valid-padding remainders, `.shared` tiles + `bar.sync`, cross-block `red.*`
//! atomics (max / min / add), multi-block grids — and every value is integer / fixed-point, so the
//! assertions are bit-exact with NO float tolerance. A mis-strided batch, a dropped channel in the
//! accumulation, an off-by-one in the pooling window, a barrier that failed to fence the row reductions,
//! or an atomic that lost an update would each fail these assertions.
//!
//! Wiring is identical to `tests/gpgpu_patterns.rs` / `tests/compute_demo.rs`: the same in-process
//! [`InProcessCommandSink`] over the [`CpuExecutor`] with the PTX compiler injected.
//!
//! Batteries:
//!   1. `batched_strided_gemm` — B independent M×K·K×N GEMMs at a batch stride (the cuBLAS
//!      `cublasGemmStridedBatched` shape), shared-memory TILE=16 blocked. Exact per batch.
//!   2. `conv2d_nchw`          — a cuDNN conv layer (N=1, C_in=3, C_out=4, 3×3, NCHW, valid pad), exact
//!      multi-channel accumulation vs a CPU conv.
//!   3. `pool2x2`              — max-pool AND avg-pool, 2×2 stride-2 (the cuDNN pooling shape), exact.
//!   4. `softmax_rowwise`      — numerically-stable per-row softmax in fixed point: subtract the row max,
//!      base-2 fixed-point exponential (`(1<<Q) >> (max−x)`), row-sum denominator — the two exact stages
//!      of a stable softmax (the final normalize-divide is the sole float step, omitted). Exact per row.
//!   5. `layernorm_stats`      — per-row LayerNorm statistics in fixed point: row mean (N a power of two →
//!      exact arithmetic-shift), centered residual `x−mean`, and variance `Σ(x−mean)² / N`. Exact.
//!   6. `relu_gelu`            — ReLU and a fixed-point GELU-style cubic, elementwise over a multi-block
//!      grid, each exact vs the identical integer polynomial on CPU.
//!   7. `gemv_argmax`          — matrix–vector `y = A·x` then a large cross-block reduction: the max of y
//!      (`red.global.max`) and its arg-max index (`red.global.min` over the indices that hit the max).
//!      Exact y, exact max, exact lowest-index arg-max.
//!   8. `im2col` + `embedding` — the im2col lowering front-end (valid-pad patch gather) and an embedding
//!      table gather, each a pure exact index remap vs CPU.

use hl_cuda::adapter::ptx;
use hl_cuda::service::{allocate, launch, load_module, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::capability::{
    command_bits, format_bits, shader_payload, ALL_COMMANDS, COLOR_FORMATS,
};
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{
    BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION,
};

// --------------------------------------------------------------------------------------------------
// shared harness — identical wiring to tests/gpgpu_patterns.rs.
// --------------------------------------------------------------------------------------------------

fn harness() -> InProcessCommandSink<CpuExecutor> {
    let mut exec = CpuExecutor::new();
    exec.set_kernel_compiler(|desc: &KernelDescriptor| {
        ptx::compile(&desc.ptx, &desc.entry, desc.block)
    });
    let mut sink = InProcessCommandSink::new(exec);
    let req = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: command_bits(ALL_COMMANDS),
        texture_formats: format_bits(COLOR_FORMATS),
    };
    sink.negotiate(&req).expect("negotiate against CpuExecutor");
    sink
}

fn i32s_to_bytes(v: &[i32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn bytes_to_i32s(raw: &[u8]) -> Vec<i32> {
    raw.chunks_exact(4)
        .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn readback(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &CudaContext,
    p: DevicePtr,
    len: usize,
) -> Vec<u8> {
    let (buf, off): (BufferId, u64) = ctx.device_location(p).unwrap();
    sink.read_buffer(buf, off, len).unwrap()
}

fn upload(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &mut CudaContext,
    bytes: &[u8],
) -> DevicePtr {
    let p = allocate::mem_alloc(ctx, sink, bytes.len() as u64).unwrap();
    transfer::memcpy_htod(ctx, sink, p, bytes).unwrap();
    p
}

fn alloc_zeroed_i32(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &mut CudaContext,
    n: usize,
) -> DevicePtr {
    let p = allocate::mem_alloc(ctx, sink, (n * 4) as u64).unwrap();
    transfer::memset(ctx, sink, p, &vec![0u8; n * 4]).unwrap();
    p
}

fn sc(v: i32) -> KernelArg {
    KernelArg::Scalar(v.to_le_bytes().to_vec())
}

// ==================================================================================================
// 1. batched_strided_gemm — the cuBLAS `cublasGemmStridedBatched` shape: B independent GEMMs
//    C_b(M×N) = A_b(M×K) · B_b(K×N), the b-th operand at element base `b*stride`. Each 16×16 block
//    cooperatively stages a TILE of A_b and B_b into `.shared`, `bar.sync`s, accumulates, advances the
//    K tiles — the canonical tiled GEMM, replicated across `ctaid.z` = batch. M=N=K=32, TILE=16, B=3.
// ==================================================================================================

const GEMM_BATCHED_PTX: &str = r#"
    .visible .entry mm_batched(
        .param .u64 p_a,
        .param .u64 p_b,
        .param .u64 p_c,
        .param .u32 p_n,
        .param .u32 p_k,
        .param .u32 p_tiles,
        .param .u32 p_sa,
        .param .u32 p_sb,
        .param .u32 p_sc
    )
    {
        .shared .align 4 .b32 As[256];
        .shared .align 4 .b32 Bs[256];
        ld.param.u64 %ra, [p_a];
        ld.param.u64 %rb, [p_b];
        ld.param.u64 %rc, [p_c];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rk, [p_k];
        ld.param.u32 %rtiles, [p_tiles];
        ld.param.u32 %rsa, [p_sa];
        ld.param.u32 %rsb, [p_sb];
        ld.param.u32 %rsc, [p_sc];
        cvta.to.global.u64 %gA, %ra;
        cvta.to.global.u64 %gB, %rb;
        cvta.to.global.u64 %gC, %rc;
        mov.u32 %tx, %tid.x;
        mov.u32 %ty, %tid.y;
        mov.u32 %bx, %ctaid.x;
        mov.u32 %by, %ctaid.y;
        mov.u32 %bz, %ctaid.z;
        mad.lo.s32 %row, %by, 16, %ty;
        mad.lo.s32 %col, %bx, 16, %tx;
        mul.lo.s32 %baseA, %bz, %rsa;
        mul.lo.s32 %baseB, %bz, %rsb;
        mul.lo.s32 %baseC, %bz, %rsc;
        mad.lo.s32 %sidx, %ty, 16, %tx;
        mul.lo.s32 %soff, %sidx, 4;
        mov.u32 %asb, As;
        mov.u32 %bsb, Bs;
        add.s32 %asaddr, %asb, %soff;
        add.s32 %bsaddr, %bsb, %soff;
        mov.u32 %acc, 0;
        mov.u32 %t, 0;
    TLOOP:
        setp.ge.s32 %pdone, %t, %rtiles;
        @%pdone bra ENDT;
        mad.lo.s32 %acol, %t, 16, %tx;
        mad.lo.s32 %aidx, %row, %rk, %acol;
        add.s32 %aidx, %aidx, %baseA;
        mul.wide.s32 %aoff, %aidx, 4;
        add.s64 %aptr, %gA, %aoff;
        ld.global.u32 %av, [%aptr];
        st.shared.u32 [%asaddr], %av;
        mad.lo.s32 %brow, %t, 16, %ty;
        mad.lo.s32 %bidx, %brow, %rn, %col;
        add.s32 %bidx, %bidx, %baseB;
        mul.wide.s32 %boff, %bidx, 4;
        add.s64 %bptr, %gB, %boff;
        ld.global.u32 %bv, [%bptr];
        st.shared.u32 [%bsaddr], %bv;
        bar.sync;
        mov.u32 %kk, 0;
    KLOOP:
        setp.ge.s32 %pk, %kk, 16;
        @%pk bra ENDK;
        mad.lo.s32 %aik, %ty, 16, %kk;
        mul.lo.s32 %aiko, %aik, 4;
        add.s32 %aikaddr, %asb, %aiko;
        ld.shared.u32 %sa, [%aikaddr];
        mad.lo.s32 %bik, %kk, 16, %tx;
        mul.lo.s32 %biko, %bik, 4;
        add.s32 %bikaddr, %bsb, %biko;
        ld.shared.u32 %sbv, [%bikaddr];
        mad.lo.s32 %acc, %sa, %sbv, %acc;
        add.s32 %kk, %kk, 1;
        bra KLOOP;
    ENDK:
        bar.sync;
        add.s32 %t, %t, 1;
        bra TLOOP;
    ENDT:
        mad.lo.s32 %cidx, %row, %rn, %col;
        add.s32 %cidx, %cidx, %baseC;
        mul.wide.s32 %coff, %cidx, 4;
        add.s64 %cptr, %gC, %coff;
        st.global.u32 [%cptr], %acc;
        ret;
    }
"#;

#[test]
fn batched_strided_gemm_exact() {
    const TILE: usize = 16;
    let (m, n, k) = (32usize, 32usize, 32usize);
    let batch = 3usize;
    let (sa, sb, sc_stride) = (m * k, k * n, m * n);

    // Bounded signed operands so the i32 accumulation cannot overflow (|a|,|b| ≤ 9 ⇒ |Σ| ≤ 32·81 = 2592).
    let a: Vec<i32> = (0..batch * sa)
        .map(|i| (i as i32 * 7 + 3).rem_euclid(19) - 9)
        .collect();
    let b: Vec<i32> = (0..batch * sb)
        .map(|i| (i as i32 * 5 + 1).rem_euclid(19) - 9)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(GEMM_BATCHED_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "mm_batched").unwrap();

    let da = upload(&mut sink, &mut ctx, &i32s_to_bytes(&a));
    let db = upload(&mut sink, &mut ctx, &i32s_to_bytes(&b));
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, (batch * sc_stride * 4) as u64).unwrap();

    let tiles = k / TILE; // 2
    let args = vec![
        KernelArg::Ptr(da),
        KernelArg::Ptr(db),
        KernelArg::Ptr(dc),
        sc(n as i32),
        sc(k as i32),
        sc(tiles as i32),
        sc(sa as i32),
        sc(sb as i32),
        sc(sc_stride as i32),
    ];
    // grid (2,2,batch) blocks × block (16,16) threads = the full 32×32 output for every batch.
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        ((n / TILE) as u32, (m / TILE) as u32, batch as u32),
        (16, 16, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, dc, batch * sc_stride * 4));

    // CPU reference: an independent per-batch triple loop, i32-wrapping to match the interpreter.
    let mut want = vec![0i32; batch * sc_stride];
    for bt in 0..batch {
        for row in 0..m {
            for col in 0..n {
                let mut acc = 0i32;
                for kk in 0..k {
                    acc = acc.wrapping_add(
                        a[bt * sa + row * k + kk].wrapping_mul(b[bt * sb + kk * n + col]),
                    );
                }
                want[bt * sc_stride + row * n + col] = acc;
            }
        }
    }
    assert_eq!(
        got, want,
        "strided-batched GEMM: every element of every batch exact"
    );
    // Batches must actually differ (guards against a stride bug that reruns batch 0 three times).
    assert_ne!(
        &want[0..sc_stride],
        &want[sc_stride..2 * sc_stride],
        "distinct batches must produce distinct C (stride is real)"
    );
    assert_eq!(sink.executor().dispatches, 1);
}

// ==================================================================================================
// 2. conv2d_nchw — a cuDNN convolution layer: input [N=1, C_in=3, H, W], weights [C_out=4, C_in=3, 3, 3],
//    NCHW layout, valid padding → output [1, C_out=4, H−2, W−2]. Each thread owns one output pixel of one
//    output channel (`ctaid.z` = out channel); it accumulates the full C_in·3·3 = 27-tap dot product with
//    real NCHW strides. A dropped channel or a transposed weight index would fail element-exact.
// ==================================================================================================

const CONV2D_NCHW_PTX: &str = r#"
    .visible .entry conv2d_nchw(
        .param .u64 p_in,
        .param .u64 p_w,
        .param .u64 p_out,
        .param .u32 p_H,
        .param .u32 p_W,
        .param .u32 p_OH,
        .param .u32 p_OW,
        .param .u32 p_Cin
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rw, [p_w];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rH, [p_H];
        ld.param.u32 %rW, [p_W];
        ld.param.u32 %rOH, [p_OH];
        ld.param.u32 %rOW, [p_OW];
        ld.param.u32 %rCin, [p_Cin];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gw, %rw;
        cvta.to.global.u64 %gout, %rout;
        mov.u32 %ox, %tid.x;
        mov.u32 %oy, %tid.y;
        mov.u32 %oc, %ctaid.z;
        setp.ge.s32 %px, %ox, %rOW;
        @%px bra DONE;
        setp.ge.s32 %py, %oy, %rOH;
        @%py bra DONE;
        mov.u32 %acc, 0;
        mov.u32 %ic, 0;
    ICLOOP:
        setp.ge.s32 %pic, %ic, %rCin;
        @%pic bra ICEND;
        mov.u32 %ky, 0;
    KYLOOP:
        setp.gt.s32 %pky, %ky, 2;
        @%pky bra KYEND;
        add.s32 %iy, %oy, %ky;
        mov.u32 %kx, 0;
    KXLOOP:
        setp.gt.s32 %pkx, %kx, 2;
        @%pkx bra KXEND;
        add.s32 %ix, %ox, %kx;
        // in index = (ic*H + iy)*W + ix
        mad.lo.s32 %tmp, %ic, %rH, %iy;
        mad.lo.s32 %iidx, %tmp, %rW, %ix;
        mul.wide.s32 %ioff, %iidx, 4;
        add.s64 %ip, %gin, %ioff;
        ld.global.u32 %pv, [%ip];
        // w index = ((oc*Cin + ic)*3 + ky)*3 + kx
        mad.lo.s32 %w1, %oc, %rCin, %ic;
        mad.lo.s32 %w2, %w1, 3, %ky;
        mad.lo.s32 %w3, %w2, 3, %kx;
        mul.wide.s32 %woff, %w3, 4;
        add.s64 %wp, %gw, %woff;
        ld.global.u32 %wv, [%wp];
        mad.lo.s32 %acc, %pv, %wv, %acc;
        add.s32 %kx, %kx, 1;
        bra KXLOOP;
    KXEND:
        add.s32 %ky, %ky, 1;
        bra KYLOOP;
    KYEND:
        add.s32 %ic, %ic, 1;
        bra ICLOOP;
    ICEND:
        // out index = (oc*OH + oy)*OW + ox
        mad.lo.s32 %o1, %oc, %rOH, %oy;
        mad.lo.s32 %oidx, %o1, %rOW, %ox;
        mul.wide.s32 %oo, %oidx, 4;
        add.s64 %op, %gout, %oo;
        st.global.u32 [%op], %acc;
    DONE:
        ret;
    }
"#;

#[test]
fn conv2d_nchw_multichannel_exact() {
    let (cin, cout) = (3usize, 4usize);
    let (h, w) = (8usize, 8usize);
    let (oh, ow) = (h - 2, w - 2); // valid padding, 3×3 → 6×6
    let input: Vec<i32> = (0..cin * h * w).map(|i| (i as i32 * 3 + 1) % 10).collect();
    let weight: Vec<i32> = (0..cout * cin * 9)
        .map(|i| (i as i32 * 7 + 2) % 9 - 4)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(CONV2D_NCHW_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "conv2d_nchw").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_w = upload(&mut sink, &mut ctx, &i32s_to_bytes(&weight));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, cout * oh * ow);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_w),
        KernelArg::Ptr(d_out),
        sc(h as i32),
        sc(w as i32),
        sc(oh as i32),
        sc(ow as i32),
        sc(cin as i32),
    ];
    // block (8,8) covers the 6×6 output (guarded); grid.z = C_out.
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, cout as u32),
        (8, 8, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, cout * oh * ow * 4));

    // CPU reference: NCHW valid convolution, exact multi-channel accumulation.
    let mut want = vec![0i32; cout * oh * ow];
    for oc in 0..cout {
        for oy in 0..oh {
            for ox in 0..ow {
                let mut acc = 0i32;
                for ic in 0..cin {
                    for ky in 0..3 {
                        for kx in 0..3 {
                            let iv = input[(ic * h + (oy + ky)) * w + (ox + kx)];
                            let wv = weight[((oc * cin + ic) * 3 + ky) * 3 + kx];
                            acc = acc.wrapping_add(iv.wrapping_mul(wv));
                        }
                    }
                }
                want[(oc * oh + oy) * ow + ox] = acc;
            }
        }
    }
    assert_eq!(got, want, "NCHW 3×3 valid conv, every output element exact");
    assert!(
        want.iter().any(|&v| v != 0),
        "reference must be non-degenerate"
    );
}

// ==================================================================================================
// 3. pool2x2 — the cuDNN 2×2 stride-2 pooling shape, computing BOTH max-pool and avg-pool in one pass.
//    Max is a register compare tree (no min/max ALU in the subset); avg is `Σ >> 2` (exact floor of the
//    4-tap mean over the non-negative inputs). One thread per output cell.
// ==================================================================================================

const POOL_PTX: &str = r#"
    .visible .entry pool2x2(
        .param .u64 p_in,
        .param .u64 p_max,
        .param .u64 p_avg,
        .param .u32 p_W,
        .param .u32 p_OH,
        .param .u32 p_OW
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rmax, [p_max];
        ld.param.u64 %ravg, [p_avg];
        ld.param.u32 %rW, [p_W];
        ld.param.u32 %rOH, [p_OH];
        ld.param.u32 %rOW, [p_OW];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gmax, %rmax;
        cvta.to.global.u64 %gavg, %ravg;
        mov.u32 %ox, %tid.x;
        mov.u32 %oy, %tid.y;
        setp.ge.s32 %pxo, %ox, %rOW;
        @%pxo bra DONE;
        setp.ge.s32 %pyo, %oy, %rOH;
        @%pyo bra DONE;
        shl.b32 %ix, %ox, 1;
        shl.b32 %iy, %oy, 1;
        // v0 = in[iy*W + ix]
        mad.lo.s32 %r0, %iy, %rW, %ix;
        mul.wide.s32 %o0, %r0, 4;
        add.s64 %p0, %gin, %o0;
        ld.global.u32 %v0, [%p0];
        // v1 = in[iy*W + ix + 1]
        ld.global.u32 %v1, [%p0+4];
        // v2 = in[(iy+1)*W + ix]
        add.s32 %iy1, %iy, 1;
        mad.lo.s32 %r2, %iy1, %rW, %ix;
        mul.wide.s32 %o2, %r2, 4;
        add.s64 %p2, %gin, %o2;
        ld.global.u32 %v2, [%p2];
        ld.global.u32 %v3, [%p2+4];
        // max = max(v0,v1,v2,v3) via a compare tree
        mov.u32 %mx, %v0;
        setp.gt.s32 %m1, %v1, %mx;
        @!%m1 bra M1;
        mov.u32 %mx, %v1;
    M1:
        setp.gt.s32 %m2, %v2, %mx;
        @!%m2 bra M2;
        mov.u32 %mx, %v2;
    M2:
        setp.gt.s32 %m3, %v3, %mx;
        @!%m3 bra M3;
        mov.u32 %mx, %v3;
    M3:
        // avg = (v0+v1+v2+v3) >> 2
        add.s32 %s, %v0, %v1;
        add.s32 %s, %s, %v2;
        add.s32 %s, %s, %v3;
        shr.s32 %av, %s, 2;
        mad.lo.s32 %oidx, %oy, %rOW, %ox;
        mul.wide.s32 %oo, %oidx, 4;
        add.s64 %pmo, %gmax, %oo;
        st.global.u32 [%pmo], %mx;
        add.s64 %pao, %gavg, %oo;
        st.global.u32 [%pao], %av;
    DONE:
        ret;
    }
"#;

#[test]
fn pool2x2_max_and_avg_exact() {
    let (h, w) = (8usize, 8usize);
    let (oh, ow) = (h / 2, w / 2);
    let img: Vec<i32> = (0..h * w).map(|i| (i as i32 * 13 + 7) % 97).collect(); // non-negative

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(POOL_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "pool2x2").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&img));
    let d_max = alloc_zeroed_i32(&mut sink, &mut ctx, oh * ow);
    let d_avg = alloc_zeroed_i32(&mut sink, &mut ctx, oh * ow);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_max),
        KernelArg::Ptr(d_avg),
        sc(w as i32),
        sc(oh as i32),
        sc(ow as i32),
    ];
    launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (4, 4, 1), &args).unwrap();

    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, oh * ow * 4));
    let got_avg = bytes_to_i32s(&readback(&mut sink, &ctx, d_avg, oh * ow * 4));

    let mut want_max = vec![0i32; oh * ow];
    let mut want_avg = vec![0i32; oh * ow];
    for oy in 0..oh {
        for ox in 0..ow {
            let (ix, iy) = (ox * 2, oy * 2);
            let v0 = img[iy * w + ix];
            let v1 = img[iy * w + ix + 1];
            let v2 = img[(iy + 1) * w + ix];
            let v3 = img[(iy + 1) * w + ix + 1];
            want_max[oy * ow + ox] = v0.max(v1).max(v2).max(v3);
            want_avg[oy * ow + ox] = (v0 + v1 + v2 + v3) >> 2;
        }
    }
    assert_eq!(got_max, want_max, "2×2 max-pool exact");
    assert_eq!(got_avg, want_avg, "2×2 avg-pool (floor mean) exact");
}

// ==================================================================================================
// 4. softmax_rowwise — a numerically-stable per-row softmax in fixed point. One block per row (block dim
//    = number of columns, a power of two); the row is staged into `.shared`, a barrier-fenced compare
//    tree finds the row max, each lane forms the base-2 fixed-point weight `w = (1<<Q) >> (max − x)`
//    (the max-subtraction is the stability trick, an exact right shift), and a second barrier-fenced
//    tree sums the weights into the row denominator. Weights AND denominators are asserted exact; the
//    final normalize-divide (`w / Σ`) is the sole floating-point step and is intentionally omitted.
// ==================================================================================================

const SOFTMAX_PTX: &str = r#"
    .visible .entry softmax_rows(
        .param .u64 p_in,
        .param .u64 p_w,
        .param .u64 p_sum,
        .param .u32 p_cols
    )
    {
        .shared .align 4 .b32 sh[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rw, [p_w];
        ld.param.u64 %rsum, [p_sum];
        ld.param.u32 %rcols, [p_cols];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gw, %rw;
        cvta.to.global.u64 %gsum, %rsum;
        mov.u32 %tid, %tid.x;
        mov.u32 %row, %ctaid.x;
        mad.lo.s32 %gid, %row, %rcols, %tid;
        mul.lo.s32 %toff, %tid, 4;
        // stage x into shared
        mul.wide.s32 %io, %gid, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %x, [%ip];
        mov.u32 %shb, sh;
        add.s32 %saddr, %shb, %toff;
        st.shared.u32 [%saddr], %x;
        bar.sync;
        // max reduction over shared
        shr.s32 %stride, %rcols, 1;
    MAXLOOP:
        setp.le.s32 %pend, %stride, 0;
        @%pend bra MAXDONE;
        setp.lt.s32 %pact, %tid, %stride;
        @!%pact bra MAXAFTER;
        ld.shared.u32 %a, [%saddr];
        add.s32 %jidx, %tid, %stride;
        mul.lo.s32 %joff, %jidx, 4;
        add.s32 %jaddr, %shb, %joff;
        ld.shared.u32 %bv, [%jaddr];
        setp.gt.s32 %pg, %bv, %a;
        @!%pg bra MAXAFTER;
        st.shared.u32 [%saddr], %bv;
    MAXAFTER:
        bar.sync;
        shr.s32 %stride, %stride, 1;
        bra MAXLOOP;
    MAXDONE:
        ld.shared.u32 %m, [%shb];
        bar.sync;
        // w = (1<<16) >> (m - x)
        sub.s32 %shift, %m, %x;
        mov.u32 %one, 65536;
        shr.u32 %w, %one, %shift;
        add.s64 %wp, %gw, %io;
        st.global.u32 [%wp], %w;
        // stage w for the sum reduction
        st.shared.u32 [%saddr], %w;
        bar.sync;
        shr.s32 %sstride, %rcols, 1;
    SUMLOOP:
        setp.le.s32 %psend, %sstride, 0;
        @%psend bra SUMDONE;
        setp.lt.s32 %psact, %tid, %sstride;
        @!%psact bra SUMAFTER;
        ld.shared.u32 %sa, [%saddr];
        add.s32 %sj, %tid, %sstride;
        mul.lo.s32 %sjoff, %sj, 4;
        add.s32 %sjaddr, %shb, %sjoff;
        ld.shared.u32 %sb, [%sjaddr];
        add.s32 %sa, %sa, %sb;
        st.shared.u32 [%saddr], %sa;
    SUMAFTER:
        bar.sync;
        shr.s32 %sstride, %sstride, 1;
        bra SUMLOOP;
    SUMDONE:
        setp.ne.s32 %pnl, %tid, 0;
        @%pnl bra DONE;
        ld.shared.u32 %total, [%shb];
        mul.wide.s32 %so, %row, 4;
        add.s64 %sp, %gsum, %so;
        st.global.u32 [%sp], %total;
    DONE:
        ret;
    }
"#;

#[test]
fn softmax_rowwise_fixedpoint_exact() {
    let rows = 6usize;
    let cols = 8usize; // power of two for the reduction tree
    const Q: u32 = 16;
    // Bounded so (max − x) stays well under 32 (fixed-point base-2 exp shift domain).
    let input: Vec<i32> = (0..rows * cols).map(|i| (i as i32 * 5 + 3) % 19).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SOFTMAX_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "softmax_rows").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_w = alloc_zeroed_i32(&mut sink, &mut ctx, rows * cols);
    let d_sum = alloc_zeroed_i32(&mut sink, &mut ctx, rows);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_w),
        KernelArg::Ptr(d_sum),
        sc(cols as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (rows as u32, 1, 1),
        (cols as u32, 1, 1),
        &args,
    )
    .unwrap();

    let got_w = bytes_to_i32s(&readback(&mut sink, &ctx, d_w, rows * cols * 4));
    let got_sum = bytes_to_i32s(&readback(&mut sink, &ctx, d_sum, rows * 4));

    // CPU reference: stable base-2 fixed-point softmax weights + row denominators.
    let mut want_w = vec![0i32; rows * cols];
    let mut want_sum = vec![0i32; rows];
    for r in 0..rows {
        let m = (0..cols).map(|c| input[r * cols + c]).max().unwrap();
        let mut s = 0i32;
        for c in 0..cols {
            let shift = (m - input[r * cols + c]) as u32; // >= 0
            let w = (1u32 << Q).wrapping_shr(shift) as i32; // wrapping_shr masks to &31, matching the interpreter
            want_w[r * cols + c] = w;
            s += w;
        }
        want_sum[r] = s;
    }
    assert_eq!(got_w, want_w, "row-softmax fixed-point weights exact");
    assert_eq!(got_sum, want_sum, "row-softmax denominators exact");
    // The lane holding the row max must weight exactly 1<<Q (its shift is 0) — the stability anchor.
    for r in 0..rows {
        assert!(
            want_w[r * cols..(r + 1) * cols].contains(&(1i32 << Q)),
            "row max lane weights 1<<Q"
        );
    }
}

// ==================================================================================================
// 5. layernorm_stats — the per-row LayerNorm statistics a normalization layer computes, in fixed point.
//    One block per row (block dim = feature count N, a power of two): stage the row, barrier-fenced sum
//    tree → mean = Σ/N (exact arithmetic shift, N a power of two), each lane writes the centered residual
//    x − mean, then a second barrier-fenced tree over the squared residuals → variance = Σ(x−mean)²/N.
//    Centered residuals, per-row mean, and per-row variance are all asserted exact. (The final divide by
//    the standard deviation is the sole float step and is omitted.)
// ==================================================================================================

const LAYERNORM_PTX: &str = r#"
    .visible .entry layernorm_stats(
        .param .u64 p_in,
        .param .u64 p_cent,
        .param .u64 p_mean,
        .param .u64 p_var,
        .param .u32 p_n,
        .param .u32 p_log2n
    )
    {
        .shared .align 4 .b32 sh[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rcent, [p_cent];
        ld.param.u64 %rmean, [p_mean];
        ld.param.u64 %rvar, [p_var];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rlog, [p_log2n];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gcent, %rcent;
        cvta.to.global.u64 %gmean, %rmean;
        cvta.to.global.u64 %gvar, %rvar;
        mov.u32 %tid, %tid.x;
        mov.u32 %row, %ctaid.x;
        mad.lo.s32 %gid, %row, %rn, %tid;
        mul.lo.s32 %toff, %tid, 4;
        mul.wide.s32 %io, %gid, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %x, [%ip];
        mov.u32 %shb, sh;
        add.s32 %saddr, %shb, %toff;
        st.shared.u32 [%saddr], %x;
        bar.sync;
        // sum reduction → mean
        shr.s32 %stride, %rn, 1;
    SUMLOOP:
        setp.le.s32 %pend, %stride, 0;
        @%pend bra SUMDONE;
        setp.lt.s32 %pact, %tid, %stride;
        @!%pact bra SUMAFTER;
        ld.shared.u32 %a, [%saddr];
        add.s32 %j, %tid, %stride;
        mul.lo.s32 %joff, %j, 4;
        add.s32 %jaddr, %shb, %joff;
        ld.shared.u32 %b, [%jaddr];
        add.s32 %a, %a, %b;
        st.shared.u32 [%saddr], %a;
    SUMAFTER:
        bar.sync;
        shr.s32 %stride, %stride, 1;
        bra SUMLOOP;
    SUMDONE:
        ld.shared.u32 %total, [%shb];
        bar.sync;
        shr.s32 %mean, %total, %rlog;
        // centered residual
        sub.s32 %c, %x, %mean;
        add.s64 %cp, %gcent, %io;
        st.global.u32 [%cp], %c;
        // lane 0 records the mean
        setp.ne.s32 %pn0, %tid, 0;
        @%pn0 bra AFTERMEAN;
        mul.wide.s32 %mo, %row, 4;
        add.s64 %mp, %gmean, %mo;
        st.global.u32 [%mp], %mean;
    AFTERMEAN:
        // stage squared residual for the variance reduction
        mul.lo.s32 %sq, %c, %c;
        st.shared.u32 [%saddr], %sq;
        bar.sync;
        shr.s32 %vstride, %rn, 1;
    VLOOP:
        setp.le.s32 %pvend, %vstride, 0;
        @%pvend bra VDONE;
        setp.lt.s32 %pvact, %tid, %vstride;
        @!%pvact bra VAFTER;
        ld.shared.u32 %va, [%saddr];
        add.s32 %vj, %tid, %vstride;
        mul.lo.s32 %vjoff, %vj, 4;
        add.s32 %vjaddr, %shb, %vjoff;
        ld.shared.u32 %vb, [%vjaddr];
        add.s32 %va, %va, %vb;
        st.shared.u32 [%saddr], %va;
    VAFTER:
        bar.sync;
        shr.s32 %vstride, %vstride, 1;
        bra VLOOP;
    VDONE:
        setp.ne.s32 %pn1, %tid, 0;
        @%pn1 bra DONE;
        ld.shared.u32 %sqtotal, [%shb];
        shr.s32 %var, %sqtotal, %rlog;
        mul.wide.s32 %vo, %row, 4;
        add.s64 %vp, %gvar, %vo;
        st.global.u32 [%vp], %var;
    DONE:
        ret;
    }
"#;

#[test]
fn layernorm_stats_fixedpoint_exact() {
    let rows = 5usize;
    let n = 8usize; // power of two
    let log2n = 3u32;
    let input: Vec<i32> = (0..rows * n).map(|i| (i as i32 * 9 + 4) % 50).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(LAYERNORM_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "layernorm_stats").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_cent = alloc_zeroed_i32(&mut sink, &mut ctx, rows * n);
    let d_mean = alloc_zeroed_i32(&mut sink, &mut ctx, rows);
    let d_var = alloc_zeroed_i32(&mut sink, &mut ctx, rows);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_cent),
        KernelArg::Ptr(d_mean),
        KernelArg::Ptr(d_var),
        sc(n as i32),
        sc(log2n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (rows as u32, 1, 1),
        (n as u32, 1, 1),
        &args,
    )
    .unwrap();

    let got_cent = bytes_to_i32s(&readback(&mut sink, &ctx, d_cent, rows * n * 4));
    let got_mean = bytes_to_i32s(&readback(&mut sink, &ctx, d_mean, rows * 4));
    let got_var = bytes_to_i32s(&readback(&mut sink, &ctx, d_var, rows * 4));

    let mut want_cent = vec![0i32; rows * n];
    let mut want_mean = vec![0i32; rows];
    let mut want_var = vec![0i32; rows];
    for r in 0..rows {
        let sum: i32 = (0..n).map(|c| input[r * n + c]).sum();
        let mean = sum >> log2n; // exact arithmetic-shift mean (N a power of two)
        want_mean[r] = mean;
        let mut sq = 0i32;
        for c in 0..n {
            let cent = input[r * n + c] - mean;
            want_cent[r * n + c] = cent;
            sq += cent * cent;
        }
        want_var[r] = sq >> log2n;
    }
    assert_eq!(got_cent, want_cent, "LayerNorm centered residuals exact");
    assert_eq!(got_mean, want_mean, "LayerNorm per-row mean exact");
    assert_eq!(got_var, want_var, "LayerNorm per-row variance exact");
}

// ==================================================================================================
// 6. relu_gelu — the two most common activation layers, elementwise over a multi-block grid.
//    ReLU is a branch-selected `max(x,0)`; the GELU-style activation is a fixed-point cubic
//    `((x³ + 20·x) >> 4)` (a monotone smooth-ish approximation), each asserted bit-exact against the
//    identical integer polynomial on CPU (no float tolerance).
// ==================================================================================================

const ACT_PTX: &str = r#"
    .visible .entry relu(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %ip, %gin, %off;
        ld.global.u32 %x, [%ip];
        mov.u32 %y, 0;
        setp.gt.s32 %pp, %x, 0;
        @!%pp bra ST;
        mov.u32 %y, %x;
    ST:
        cvta.to.global.u64 %gout, %rout;
        add.s64 %op, %gout, %off;
        st.global.u32 [%op], %y;
    DONE:
        ret;
    }

    .visible .entry gelu_cubic(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %ip, %gin, %off;
        ld.global.u32 %x, [%ip];
        mul.lo.s32 %x2, %x, %x;
        mul.lo.s32 %x3, %x2, %x;
        // num = x*20 + x3  (== x^3 + 20x)
        mad.lo.s32 %num, %x, 20, %x3;
        shr.s32 %y, %num, 4;
        cvta.to.global.u64 %gout, %rout;
        add.s64 %op, %gout, %off;
        st.global.u32 [%op], %y;
    DONE:
        ret;
    }
"#;

#[test]
fn relu_and_gelu_cubic_exact() {
    let n = 500usize; // multi-block: > one 128-wide block
    let input: Vec<i32> = (0..n).map(|i| (i as i32 % 13) - 6).collect(); // spans [-6, 6]

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(ACT_PTX.as_bytes()).unwrap();
    let relu_fn = load_module::module_get_function(&ctx, module, "relu").unwrap();
    let gelu_fn = load_module::module_get_function(&ctx, module, "gelu_cubic").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_relu = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_gelu = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let grid = ((n + 127) / 128) as u32; // 4 blocks × 128

    let args_r = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_relu), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        relu_fn,
        (grid, 1, 1),
        (128, 1, 1),
        &args_r,
    )
    .unwrap();
    let args_g = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_gelu), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        gelu_fn,
        (grid, 1, 1),
        (128, 1, 1),
        &args_g,
    )
    .unwrap();

    let got_relu = bytes_to_i32s(&readback(&mut sink, &ctx, d_relu, n * 4));
    let got_gelu = bytes_to_i32s(&readback(&mut sink, &ctx, d_gelu, n * 4));

    let want_relu: Vec<i32> = input.iter().map(|&x| x.max(0)).collect();
    let want_gelu: Vec<i32> = input
        .iter()
        .map(|&x| {
            let x3 = x.wrapping_mul(x).wrapping_mul(x);
            (x.wrapping_mul(20).wrapping_add(x3)) >> 4
        })
        .collect();
    assert_eq!(got_relu, want_relu, "ReLU elementwise exact");
    assert_eq!(
        got_gelu, want_gelu,
        "fixed-point GELU-cubic elementwise exact"
    );
    assert!(
        want_relu.iter().any(|&v| v == 0) && want_relu.iter().any(|&v| v > 0),
        "ReLU exercises both sides"
    );
    assert!(
        want_gelu.iter().any(|&v| v < 0),
        "GELU-cubic produces negative outputs too"
    );
}

// ==================================================================================================
// 7. gemv_argmax — matrix–vector `y = A·x` (A is M×N, one thread per output row over a multi-block grid,
//    an `mad`-accumulated N-tap dot product) followed by a large cross-block reduction over y: the max
//    (`red.global.max.s32` into one slot) and the arg-max index (`red.global.min.s32` over exactly the
//    indices whose y equals the max → the LOWEST index, matching the standard tie rule). Exact y, exact
//    max, exact arg-max.
// ==================================================================================================

const GEMV_PTX: &str = r#"
    .visible .entry gemv(
        .param .u64 p_a,
        .param .u64 p_x,
        .param .u64 p_y,
        .param .u32 p_m,
        .param .u32 p_n
    )
    {
        ld.param.u64 %ra, [p_a];
        ld.param.u64 %rx, [p_x];
        ld.param.u64 %ry, [p_y];
        ld.param.u32 %rm, [p_m];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %row, %ct, %nt, %tt;
        setp.ge.s32 %pg, %row, %rm;
        @%pg bra DONE;
        cvta.to.global.u64 %gA, %ra;
        cvta.to.global.u64 %gx, %rx;
        cvta.to.global.u64 %gy, %ry;
        mul.lo.s32 %rowbase, %row, %rn;
        mov.u32 %acc, 0;
        mov.u32 %j, 0;
    JLOOP:
        setp.ge.s32 %pj, %j, %rn;
        @%pj bra JEND;
        add.s32 %aidx, %rowbase, %j;
        mul.wide.s32 %ao, %aidx, 4;
        add.s64 %ap, %gA, %ao;
        ld.global.u32 %av, [%ap];
        mul.wide.s32 %xo, %j, 4;
        add.s64 %xp, %gx, %xo;
        ld.global.u32 %xv, [%xp];
        mad.lo.s32 %acc, %av, %xv, %acc;
        add.s32 %j, %j, 1;
        bra JLOOP;
    JEND:
        mul.wide.s32 %yo, %row, 4;
        add.s64 %yp, %gy, %yo;
        st.global.u32 [%yp], %acc;
    DONE:
        ret;
    }

    .visible .entry reduce_max(
        .param .u64 p_y,
        .param .u64 p_max,
        .param .u32 p_m
    )
    {
        ld.param.u64 %ry, [p_y];
        ld.param.u64 %rmax, [p_max];
        ld.param.u32 %rm, [p_m];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rm;
        @%pg bra DONE;
        cvta.to.global.u64 %gy, %ry;
        mul.wide.s32 %yo, %i, 4;
        add.s64 %yp, %gy, %yo;
        ld.global.u32 %v, [%yp];
        cvta.to.global.u64 %gmax, %rmax;
        red.global.max.s32 [%gmax], %v;
    DONE:
        ret;
    }

    .visible .entry arg_of_max(
        .param .u64 p_y,
        .param .u64 p_max,
        .param .u64 p_idx,
        .param .u32 p_m
    )
    {
        ld.param.u64 %ry, [p_y];
        ld.param.u64 %rmax, [p_max];
        ld.param.u64 %ridx, [p_idx];
        ld.param.u32 %rm, [p_m];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rm;
        @%pg bra DONE;
        cvta.to.global.u64 %gy, %ry;
        mul.wide.s32 %yo, %i, 4;
        add.s64 %yp, %gy, %yo;
        ld.global.u32 %v, [%yp];
        cvta.to.global.u64 %gmax, %rmax;
        ld.global.u32 %mv, [%gmax];
        setp.ne.s32 %pne, %v, %mv;
        @%pne bra DONE;
        cvta.to.global.u64 %gidx, %ridx;
        red.global.min.s32 [%gidx], %i;
    DONE:
        ret;
    }
"#;

#[test]
fn gemv_and_argmax_exact() {
    let (m, n) = (1000usize, 64usize);
    let a: Vec<i32> = (0..m * n)
        .map(|i| (i as i32 * 7 + 1).rem_euclid(11) - 5)
        .collect();
    let x: Vec<i32> = (0..n)
        .map(|i| (i as i32 * 3 + 2).rem_euclid(11) - 5)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(GEMV_PTX.as_bytes()).unwrap();
    let gemv_fn = load_module::module_get_function(&ctx, module, "gemv").unwrap();
    let max_fn = load_module::module_get_function(&ctx, module, "reduce_max").unwrap();
    let arg_fn = load_module::module_get_function(&ctx, module, "arg_of_max").unwrap();

    let d_a = upload(&mut sink, &mut ctx, &i32s_to_bytes(&a));
    let d_x = upload(&mut sink, &mut ctx, &i32s_to_bytes(&x));
    let d_y = alloc_zeroed_i32(&mut sink, &mut ctx, m);
    // max slot seeded to i32::MIN, arg-index slot seeded to M (a sentinel above any real index).
    let d_max = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_max, &i32s_to_bytes(&[i32::MIN])).unwrap();
    let d_idx = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_idx, &i32s_to_bytes(&[m as i32])).unwrap();

    let grid = ((m + 255) / 256) as u32; // 4 blocks × 256

    let args_gemv = vec![
        KernelArg::Ptr(d_a),
        KernelArg::Ptr(d_x),
        KernelArg::Ptr(d_y),
        sc(m as i32),
        sc(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        gemv_fn,
        (grid, 1, 1),
        (256, 1, 1),
        &args_gemv,
    )
    .unwrap();

    let args_max = vec![KernelArg::Ptr(d_y), KernelArg::Ptr(d_max), sc(m as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        max_fn,
        (grid, 1, 1),
        (256, 1, 1),
        &args_max,
    )
    .unwrap();

    let args_arg = vec![
        KernelArg::Ptr(d_y),
        KernelArg::Ptr(d_max),
        KernelArg::Ptr(d_idx),
        sc(m as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        arg_fn,
        (grid, 1, 1),
        (256, 1, 1),
        &args_arg,
    )
    .unwrap();

    let got_y = bytes_to_i32s(&readback(&mut sink, &ctx, d_y, m * 4));
    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, 4))[0];
    let got_idx = bytes_to_i32s(&readback(&mut sink, &ctx, d_idx, 4))[0];

    // CPU reference: gemv, then max + lowest-index arg-max.
    let mut want_y = vec![0i32; m];
    for row in 0..m {
        let mut acc = 0i32;
        for j in 0..n {
            acc = acc.wrapping_add(a[row * n + j].wrapping_mul(x[j]));
        }
        want_y[row] = acc;
    }
    let want_max = *want_y.iter().max().unwrap();
    let want_idx = want_y.iter().position(|&v| v == want_max).unwrap() as i32;

    assert_eq!(got_y, want_y, "gemv y = A·x, every row exact");
    assert_eq!(got_max, want_max, "cross-block max reduction exact");
    assert_eq!(got_idx, want_idx, "arg-max (lowest tie index) exact");
    assert_eq!(
        got_y[got_idx as usize], got_max,
        "arg-max index indeed points at the max value"
    );
}

// ==================================================================================================
// 8. im2col + embedding — two exact index-remap front-ends.
//    (a) im2col: lower a [C,H,W] input into the column matrix [C·K·K, OH·OW] a GEMM-based convolution
//        multiplies against (valid pad, 3×3, stride 1). One thread per output spatial cell writes its
//        whole patch column — the canonical im2col gather.
//    (b) embedding: gather rows of a [V,D] table by a length-T index vector → [T,D] (one block per token,
//        `ctaid.x` = token, `tid.x` = feature) — the embedding-lookup front-end of every transformer.
// ==================================================================================================

const IM2COL_PTX: &str = r#"
    .visible .entry im2col(
        .param .u64 p_in,
        .param .u64 p_col,
        .param .u32 p_C,
        .param .u32 p_H,
        .param .u32 p_W,
        .param .u32 p_OH,
        .param .u32 p_OW,
        .param .u32 p_K
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rcol, [p_col];
        ld.param.u32 %rC, [p_C];
        ld.param.u32 %rH, [p_H];
        ld.param.u32 %rW, [p_W];
        ld.param.u32 %rOH, [p_OH];
        ld.param.u32 %rOW, [p_OW];
        ld.param.u32 %rK, [p_K];
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gcol, %rcol;
        mov.u32 %ox, %tid.x;
        mov.u32 %oy, %tid.y;
        setp.ge.s32 %pxo, %ox, %rOW;
        @%pxo bra DONE;
        setp.ge.s32 %pyo, %oy, %rOH;
        @%pyo bra DONE;
        mul.lo.s32 %ncols, %rOH, %rOW;
        mad.lo.s32 %colIdx, %oy, %rOW, %ox;
        mov.u32 %c, 0;
    CLOOP:
        setp.ge.s32 %pc, %c, %rC;
        @%pc bra CEND;
        mov.u32 %ky, 0;
    KYLOOP:
        setp.ge.s32 %pky, %ky, %rK;
        @%pky bra KYEND;
        add.s32 %iy, %oy, %ky;
        mov.u32 %kx, 0;
    KXLOOP:
        setp.ge.s32 %pkx, %kx, %rK;
        @%pkx bra KXEND;
        add.s32 %ix, %ox, %kx;
        // in index = (c*H + iy)*W + ix
        mad.lo.s32 %t1, %c, %rH, %iy;
        mad.lo.s32 %iidx, %t1, %rW, %ix;
        mul.wide.s32 %io, %iidx, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %v, [%ip];
        // patchRow = (c*K + ky)*K + kx
        mad.lo.s32 %pr1, %c, %rK, %ky;
        mad.lo.s32 %patchRow, %pr1, %rK, %kx;
        // dst = patchRow*ncols + colIdx
        mad.lo.s32 %dst, %patchRow, %ncols, %colIdx;
        mul.wide.s32 %doff, %dst, 4;
        add.s64 %dp, %gcol, %doff;
        st.global.u32 [%dp], %v;
        add.s32 %kx, %kx, 1;
        bra KXLOOP;
    KXEND:
        add.s32 %ky, %ky, 1;
        bra KYLOOP;
    KYEND:
        add.s32 %c, %c, 1;
        bra CLOOP;
    CEND:
    DONE:
        ret;
    }

    .visible .entry embedding(
        .param .u64 p_tab,
        .param .u64 p_idx,
        .param .u64 p_out,
        .param .u32 p_D
    )
    {
        ld.param.u64 %rtab, [p_tab];
        ld.param.u64 %ridx, [p_idx];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rD, [p_D];
        cvta.to.global.u64 %gtab, %rtab;
        cvta.to.global.u64 %gidx, %ridx;
        cvta.to.global.u64 %gout, %rout;
        mov.u32 %d, %tid.x;
        mov.u32 %t, %ctaid.x;
        setp.ge.s32 %pd, %d, %rD;
        @%pd bra DONE;
        // ix = idx[t]
        mul.wide.s32 %to, %t, 4;
        add.s64 %ixp, %gidx, %to;
        ld.global.u32 %ix, [%ixp];
        // src = ix*D + d ; dst = t*D + d
        mad.lo.s32 %src, %ix, %rD, %d;
        mad.lo.s32 %dst, %t, %rD, %d;
        mul.wide.s32 %so, %src, 4;
        add.s64 %sp, %gtab, %so;
        ld.global.u32 %v, [%sp];
        mul.wide.s32 %dsto, %dst, 4;
        add.s64 %dp, %gout, %dsto;
        st.global.u32 [%dp], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn im2col_and_embedding_gather_exact() {
    // --- (a) im2col ---
    let (c, h, w, k) = (2usize, 5usize, 5usize, 3usize);
    let (oh, ow) = (h - k + 1, w - k + 1); // 3×3
    let ncols = oh * ow;
    let patch_rows = c * k * k;
    let input: Vec<i32> = (0..c * h * w).map(|i| (i as i32 * 3 + 1) % 100).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(IM2COL_PTX.as_bytes()).unwrap();
    let im2col_fn = load_module::module_get_function(&ctx, module, "im2col").unwrap();
    let embed_fn = load_module::module_get_function(&ctx, module, "embedding").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_col = alloc_zeroed_i32(&mut sink, &mut ctx, patch_rows * ncols);

    let args_im = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_col),
        sc(c as i32),
        sc(h as i32),
        sc(w as i32),
        sc(oh as i32),
        sc(ow as i32),
        sc(k as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        im2col_fn,
        (1, 1, 1),
        (ow as u32, oh as u32, 1),
        &args_im,
    )
    .unwrap();

    let got_col = bytes_to_i32s(&readback(&mut sink, &ctx, d_col, patch_rows * ncols * 4));

    let mut want_col = vec![0i32; patch_rows * ncols];
    for cc in 0..c {
        for ky in 0..k {
            for kx in 0..k {
                let patch_row = (cc * k + ky) * k + kx;
                for oy in 0..oh {
                    for ox in 0..ow {
                        let col_idx = oy * ow + ox;
                        want_col[patch_row * ncols + col_idx] =
                            input[(cc * h + (oy + ky)) * w + (ox + kx)];
                    }
                }
            }
        }
    }
    assert_eq!(got_col, want_col, "im2col column matrix, every entry exact");

    // --- (b) embedding gather ---
    let (vocab, dim, tokens) = (10usize, 4usize, 5usize);
    let table: Vec<i32> = (0..vocab * dim).map(|i| i as i32 * 2 - 3).collect();
    let idx: Vec<i32> = [7i32, 0, 3, 9, 3].to_vec(); // includes a repeat (token 2 and 4 → same row)

    let d_tab = upload(&mut sink, &mut ctx, &i32s_to_bytes(&table));
    let d_ix = upload(&mut sink, &mut ctx, &i32s_to_bytes(&idx));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, tokens * dim);

    let args_em = vec![
        KernelArg::Ptr(d_tab),
        KernelArg::Ptr(d_ix),
        KernelArg::Ptr(d_out),
        sc(dim as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        embed_fn,
        (tokens as u32, 1, 1),
        (dim as u32, 1, 1),
        &args_em,
    )
    .unwrap();

    let got_emb = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, tokens * dim * 4));

    let mut want_emb = vec![0i32; tokens * dim];
    for t in 0..tokens {
        for d in 0..dim {
            want_emb[t * dim + d] = table[(idx[t] as usize) * dim + d];
        }
    }
    assert_eq!(got_emb, want_emb, "embedding gather, every element exact");
    // The two tokens sharing index 3 must gather identical rows (guards a token/index swap).
    assert_eq!(
        &got_emb[2 * dim..3 * dim],
        &got_emb[4 * dim..5 * dim],
        "repeated index gathers the same row"
    );
}
