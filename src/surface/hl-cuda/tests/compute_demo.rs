//! CUDA **compute-correctness** demo battery: real CUDA kernels driven through the REAL hl-cuda driver
//! lowering → an in-process [`InProcessCommandSink`] over the reference [`CpuExecutor`] → the device
//! buffer read back → asserted **EXACTLY, element by element** against an independent CPU reference.
//!
//! Every test here follows the exact seam `tests/e2e.rs` established (module-load / mem-alloc /
//! memcpy-HtoD / launch / memcpy-DtoH), but each demo COMPUTES a non-trivial result and checks every
//! output element — never "did not crash". If the software path had a lowering / param-marshalling /
//! grid-mapping / shared-memory bug, these assertions would catch it.
//!
//! Batteries:
//!   1. `saxpy`        — `y[i] = a*x[i] + y[i]`, multi-block grid, every element vs an `fma` reference.
//!   2. `reduction`    — sum AND max of an N-element array across a MULTI-BLOCK grid via global atomics
//!                       (`red.global.add` / `red.global.max`), asserting the exact cross-block total.
//!   3. `matmul`       — MxK · KxN → MxN over a 2D block/grid, vs a CPU triple-loop (`fma`), per element.
//!   4. `elementwise`  — `mul` / `add` (f32) and `min` (s32, branch-selected) over two arrays, exact.
//!   5. `strided/2D`   — copy a sub-rectangle out of a wider row-major source, exact resulting layout.
//!   6. `shared+sync`  — block-scoped tree reduction in `.shared` memory with `bar.sync`, one partial per
//!                       block over a multi-block grid; each partial AND the host-summed total exact.

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
// shared harness — identical wiring to tests/e2e.rs: the reference CpuExecutor with the PTX front-end
// injected + the capability handshake a socketed driver would negotiate before its first submit.
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

fn f32s_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn i32s_to_bytes(v: &[i32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn bytes_to_f32s(raw: &[u8]) -> Vec<f32> {
    raw.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
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
    let (buf, off): (BufferId, u64) = transfer::memcpy_dtoh(ctx, p).unwrap();
    sink.read_buffer(buf, off, len).unwrap()
}

/// Allocate a device buffer and upload `bytes` to it (cuMemAlloc + cuMemcpyHtoD).
fn upload(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &mut CudaContext,
    bytes: &[u8],
) -> DevicePtr {
    let p = allocate::mem_alloc(ctx, sink, bytes.len() as u64).unwrap();
    transfer::memcpy_htod(ctx, sink, p, bytes).unwrap();
    p
}

// ==================================================================================================
// 1. saxpy — y[i] = a*x[i] + y[i] over N elements across a multi-block grid.
// ==================================================================================================

/// nvcc-style saxpy with the natural param order `(x*, y*, a, n)` → offsets `u64@0, u64@8, f32@16,
/// u32@20`. Global index `ctaid*ntid+tid` + an `i >= n` guard, one `fma.rn.f32`.
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
fn saxpy_multiblock_exact() {
    let n = 1024usize;
    let alpha = 2.5f32;
    let x: Vec<f32> = (0..n).map(|i| (i as f32) * 0.5 - 3.0).collect();
    let y: Vec<f32> = (0..n).map(|i| (i as f32) * 0.25 + 1.0).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, SAXPY_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "saxpy").unwrap();

    let dx = upload(&mut sink, &mut ctx, &f32s_to_bytes(&x));
    let dy = upload(&mut sink, &mut ctx, &f32s_to_bytes(&y));

    // grid = 4 blocks × 256 threads = exactly 1024 lanes.
    let args = vec![
        KernelArg::Ptr(dx),
        KernelArg::Ptr(dy),
        KernelArg::Scalar(alpha.to_le_bytes().to_vec()),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, (4, 1, 1), (256, 1, 1), &args).unwrap();

    let got = bytes_to_f32s(&readback(&mut sink, &ctx, dy, n * 4));
    let want: Vec<f32> = x
        .iter()
        .zip(&y)
        .map(|(xi, yi)| alpha.mul_add(*xi, *yi))
        .collect();
    assert_eq!(
        got, want,
        "saxpy y = a*x + y, all {n} elements across 4 blocks"
    );
    assert_eq!(sink.executor().dispatches, 1);
}

// ==================================================================================================
// 2. reduction — sum AND max of an N-element s32 array across a MULTI-BLOCK grid via global atomics.
//    (f32 atomics are intentionally unsupported by the model, so integer atomics carry the reduction.)
// ==================================================================================================

/// `out[0] += in[i]` for every in-bounds lane, accumulated across the whole grid with `red.global.add`.
/// Because device regions persist across blocks in the executor, the single accumulator sums cross-block.
const REDUCE_SUM_PTX: &str = r#"
    .visible .entry reduce_sum(
        .param .u64 rs_in,
        .param .u64 rs_out,
        .param .u32 rs_n
    )
    {
        ld.param.u64  %rin, [rs_in];
        ld.param.u64  %rout, [rs_out];
        ld.param.u32  %rn, [rs_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32  %off, %ri, 4;
        add.s64       %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        cvta.to.global.u64 %gout, %rout;
        red.global.add.u32 [%gout], %v;
    DONE:
        ret;
    }
"#;

/// `out[0] = max(out[0], in[i])` across the grid with signed `red.global.max`.
const REDUCE_MAX_PTX: &str = r#"
    .visible .entry reduce_max(
        .param .u64 rm_in,
        .param .u64 rm_out,
        .param .u32 rm_n
    )
    {
        ld.param.u64  %rin, [rm_in];
        ld.param.u64  %rout, [rm_out];
        ld.param.u32  %rn, [rm_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32  %off, %ri, 4;
        add.s64       %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        cvta.to.global.u64 %gout, %rout;
        red.global.max.s32 [%gout], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn reduction_sum_and_max_multiblock_exact() {
    let n = 1000usize;
    // signed inputs spanning negatives → positives: exercises signed max + wrapping-add sum.
    let input: Vec<i32> = (0..n).map(|i| i as i32 - 500).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    // ---- sum ----
    let sum_mod = load_module::module_load_data(&mut ctx, REDUCE_SUM_PTX.as_bytes()).unwrap();
    let sum_fn = load_module::module_get_function(&ctx, sum_mod, "reduce_sum").unwrap();
    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_sum = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_sum, &0i32.to_le_bytes()).unwrap(); // accumulator = 0

    // 8 blocks × 128 = 1024 lanes over N=1000 (24 guarded off) → grid.x = 8 > 1.
    let sum_args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_sum),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        sum_fn,
        (8, 1, 1),
        (128, 1, 1),
        &sum_args,
    )
    .unwrap();

    let got_sum = bytes_to_i32s(&readback(&mut sink, &ctx, d_sum, 4))[0];
    let want_sum = input.iter().fold(0i32, |acc, v| acc.wrapping_add(*v));
    assert_eq!(got_sum, want_sum, "cross-block sum reduction");
    assert_eq!(
        got_sum, -500,
        "closed-form: sum_{{i=0}}^{{999}}(i-500) = -500"
    );

    // ---- max ----
    let max_mod = load_module::module_load_data(&mut ctx, REDUCE_MAX_PTX.as_bytes()).unwrap();
    let max_fn = load_module::module_get_function(&ctx, max_mod, "reduce_max").unwrap();
    let d_max = allocate::mem_alloc(&mut ctx, &mut sink, 4).unwrap();
    transfer::memset(&mut ctx, &mut sink, d_max, &i32::MIN.to_le_bytes()).unwrap(); // -inf seed
    let max_args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_max),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        max_fn,
        (8, 1, 1),
        (128, 1, 1),
        &max_args,
    )
    .unwrap();

    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, 4))[0];
    let want_max = *input.iter().max().unwrap();
    assert_eq!(got_max, want_max, "cross-block max reduction");
    assert_eq!(got_max, 499);
    assert_eq!(
        sink.executor().dispatches,
        2,
        "two dispatches: one sum, one max"
    );
}

// ==================================================================================================
// 3. matmul — C(MxN) = A(MxK) · B(KxN), one output element per thread over a 2D block/grid + a k-loop.
// ==================================================================================================

/// Row-major matmul. `row = ctaid.y*ntid.y + tid.y`, `col = ctaid.x*ntid.x + tid.x`; each thread walks
/// `k = 0..K` accumulating `A[row*K+k] * B[k*N+col]` with `fma.rn.f32`. Bounds-guarded on both axes.
const MATMUL_PTX: &str = r#"
    .visible .entry matmul(
        .param .u64 mm_a,
        .param .u64 mm_b,
        .param .u64 mm_c,
        .param .u32 mm_m,
        .param .u32 mm_n,
        .param .u32 mm_k
    )
    {
        ld.param.u64  %ra, [mm_a];
        ld.param.u64  %rb, [mm_b];
        ld.param.u64  %rc, [mm_c];
        ld.param.u32  %rm, [mm_m];
        ld.param.u32  %rn, [mm_n];
        ld.param.u32  %rk, [mm_k];
        mov.u32       %rnx, %ntid.x;
        mov.u32       %rcx, %ctaid.x;
        mov.u32       %rtx, %tid.x;
        mad.lo.s32    %col, %rcx, %rnx, %rtx;
        mov.u32       %rny, %ntid.y;
        mov.u32       %rcy, %ctaid.y;
        mov.u32       %rty, %tid.y;
        mad.lo.s32    %row, %rcy, %rny, %rty;
        setp.ge.s32   %prm, %row, %rm;
        @%prm bra     DONE;
        setp.ge.s32   %pcn, %col, %rn;
        @%pcn bra     DONE;
        cvta.to.global.u64 %ga, %ra;
        cvta.to.global.u64 %gb, %rb;
        cvta.to.global.u64 %gc, %rc;
        mov.f32       %acc, 0f00000000;
        mov.u32       %k, 0;
    LOOP:
        setp.ge.s32   %pk, %k, %rk;
        @%pk bra      ENDLOOP;
        mad.lo.s32    %aidx, %row, %rk, %k;
        mul.wide.s32  %aoff, %aidx, 4;
        add.s64       %pa, %ga, %aoff;
        ld.global.f32 %av, [%pa];
        mad.lo.s32    %bidx, %k, %rn, %col;
        mul.wide.s32  %boff, %bidx, 4;
        add.s64       %pb, %gb, %boff;
        ld.global.f32 %bv, [%pb];
        fma.rn.f32    %acc, %av, %bv, %acc;
        add.s32       %k, %k, 1;
        bra           LOOP;
    ENDLOOP:
        mad.lo.s32    %cidx, %row, %rn, %col;
        mul.wide.s32  %coff, %cidx, 4;
        add.s64       %pc, %gc, %coff;
        st.global.f32 [%pc], %acc;
    DONE:
        ret;
    }
"#;

#[test]
fn matmul_tiled_exact() {
    let (m, k, n) = (4usize, 4usize, 4usize);
    // Fractional, non-trivial values so this is a genuine floating-point matmul.
    let a: Vec<f32> = (0..m * k).map(|i| (i as f32) * 0.5 - 1.0).collect();
    let b: Vec<f32> = (0..k * n).map(|i| (i as f32) * 0.25 + 0.5).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, MATMUL_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "matmul").unwrap();

    let da = upload(&mut sink, &mut ctx, &f32s_to_bytes(&a));
    let db = upload(&mut sink, &mut ctx, &f32s_to_bytes(&b));
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, (m * n * 4) as u64).unwrap();

    // 2D grid: block (2,2) × grid (2,2) covers the 4×4 output exactly.
    let args = vec![
        KernelArg::Ptr(da),
        KernelArg::Ptr(db),
        KernelArg::Ptr(dc),
        KernelArg::Scalar((m as i32).to_le_bytes().to_vec()),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
        KernelArg::Scalar((k as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(&mut ctx, &mut sink, func, (2, 2, 1), (2, 2, 1), &args).unwrap();

    let got = bytes_to_f32s(&readback(&mut sink, &ctx, dc, m * n * 4));

    // CPU triple-loop reference with the SAME fma accumulation order.
    let mut want = vec![0f32; m * n];
    for row in 0..m {
        for col in 0..n {
            let mut acc = 0f32;
            for kk in 0..k {
                acc = a[row * k + kk].mul_add(b[kk * n + col], acc);
            }
            want[row * n + col] = acc;
        }
    }
    assert_eq!(got, want, "matmul C = A·B, exact per element");
    assert_eq!(sink.executor().dispatches, 1);
}

// ==================================================================================================
// 4. elementwise — mul / add (f32) and min (s32, branch-selected) over two arrays. Three entries in
//    one PTX module; each launch checks every element.
// ==================================================================================================

const ELEMENTWISE_PTX: &str = r#"
    .visible .entry emul(
        .param .u64 em_a, .param .u64 em_b, .param .u64 em_c, .param .u32 em_n
    ) {
        ld.param.u64  %ra, [em_a];
        ld.param.u64  %rb, [em_b];
        ld.param.u64  %rc, [em_c];
        ld.param.u32  %rn, [em_n];
        mov.u32 %rnt, %ntid.x; mov.u32 %rct, %ctaid.x; mov.u32 %rtt, %tid.x;
        mad.lo.s32 %i, %rct, %rnt, %rtt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %ga, %ra;
        cvta.to.global.u64 %gb, %rb;
        cvta.to.global.u64 %gc, %rc;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pa, %ga, %off; add.s64 %pb, %gb, %off; add.s64 %pc, %gc, %off;
        ld.global.f32 %va, [%pa];
        ld.global.f32 %vb, [%pb];
        mul.f32 %vr, %va, %vb;
        st.global.f32 [%pc], %vr;
    DONE: ret;
    }

    .visible .entry eadd(
        .param .u64 ea_a, .param .u64 ea_b, .param .u64 ea_c, .param .u32 ea_n
    ) {
        ld.param.u64  %ra, [ea_a];
        ld.param.u64  %rb, [ea_b];
        ld.param.u64  %rc, [ea_c];
        ld.param.u32  %rn, [ea_n];
        mov.u32 %rnt, %ntid.x; mov.u32 %rct, %ctaid.x; mov.u32 %rtt, %tid.x;
        mad.lo.s32 %i, %rct, %rnt, %rtt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %ga, %ra;
        cvta.to.global.u64 %gb, %rb;
        cvta.to.global.u64 %gc, %rc;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pa, %ga, %off; add.s64 %pb, %gb, %off; add.s64 %pc, %gc, %off;
        ld.global.f32 %va, [%pa];
        ld.global.f32 %vb, [%pb];
        add.f32 %vr, %va, %vb;
        st.global.f32 [%pc], %vr;
    DONE: ret;
    }

    .visible .entry emin(
        .param .u64 en_a, .param .u64 en_b, .param .u64 en_c, .param .u32 en_n
    ) {
        ld.param.u64  %ra, [en_a];
        ld.param.u64  %rb, [en_b];
        ld.param.u64  %rc, [en_c];
        ld.param.u32  %rn, [en_n];
        mov.u32 %rnt, %ntid.x; mov.u32 %rct, %ctaid.x; mov.u32 %rtt, %tid.x;
        mad.lo.s32 %i, %rct, %rnt, %rtt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %ga, %ra;
        cvta.to.global.u64 %gb, %rb;
        cvta.to.global.u64 %gc, %rc;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pa, %ga, %off; add.s64 %pb, %gb, %off; add.s64 %pc, %gc, %off;
        ld.global.u32 %va, [%pa];
        ld.global.u32 %vb, [%pb];
        setp.lt.s32 %plt, %va, %vb;
        @%plt bra USEA;
        st.global.u32 [%pc], %vb;
        bra DONE;
    USEA:
        st.global.u32 [%pc], %va;
    DONE: ret;
    }
"#;

#[test]
fn elementwise_mul_add_min_exact() {
    let n = 512usize;
    let af: Vec<f32> = (0..n).map(|i| (i as f32) * 0.3 - 2.0).collect();
    let bf: Vec<f32> = (0..n).map(|i| (i as f32) * -0.1 + 4.0).collect();
    let ai: Vec<i32> = (0..n).map(|i| (i as i32 * 7) % 101 - 50).collect();
    let bi: Vec<i32> = (0..n).map(|i| (i as i32 * 13) % 97 - 40).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, ELEMENTWISE_PTX.as_bytes()).unwrap();

    let grid = (2u32, 1, 1);
    let block = (256u32, 1, 1); // 512 lanes total

    // mul (f32)
    let da = upload(&mut sink, &mut ctx, &f32s_to_bytes(&af));
    let db = upload(&mut sink, &mut ctx, &f32s_to_bytes(&bf));
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let mul_fn = load_module::module_get_function(&ctx, module, "emul").unwrap();
    let args = |x: DevicePtr, y: DevicePtr, z: DevicePtr| {
        vec![
            KernelArg::Ptr(x),
            KernelArg::Ptr(y),
            KernelArg::Ptr(z),
            KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
        ]
    };
    launch::launch(&mut ctx, &mut sink, mul_fn, grid, block, &args(da, db, dc)).unwrap();
    let got_mul = bytes_to_f32s(&readback(&mut sink, &ctx, dc, n * 4));
    let want_mul: Vec<f32> = af.iter().zip(&bf).map(|(x, y)| x * y).collect();
    assert_eq!(got_mul, want_mul, "elementwise mul");

    // add (f32) — reuse the same buffers, distinct entry.
    let add_fn = load_module::module_get_function(&ctx, module, "eadd").unwrap();
    launch::launch(&mut ctx, &mut sink, add_fn, grid, block, &args(da, db, dc)).unwrap();
    let got_add = bytes_to_f32s(&readback(&mut sink, &ctx, dc, n * 4));
    let want_add: Vec<f32> = af.iter().zip(&bf).map(|(x, y)| x + y).collect();
    assert_eq!(got_add, want_add, "elementwise add");

    // min (s32)
    let dai = upload(&mut sink, &mut ctx, &i32s_to_bytes(&ai));
    let dbi = upload(&mut sink, &mut ctx, &i32s_to_bytes(&bi));
    let dci = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let min_fn = load_module::module_get_function(&ctx, module, "emin").unwrap();
    launch::launch(
        &mut ctx,
        &mut sink,
        min_fn,
        grid,
        block,
        &args(dai, dbi, dci),
    )
    .unwrap();
    let got_min = bytes_to_i32s(&readback(&mut sink, &ctx, dci, n * 4));
    let want_min: Vec<i32> = ai.iter().zip(&bi).map(|(x, y)| *x.min(y)).collect();
    assert_eq!(got_min, want_min, "elementwise signed min");

    assert_eq!(sink.executor().dispatches, 3);
}

// ==================================================================================================
// 5. strided / 2D copy — extract a sub-rectangle from a wider row-major source into a packed dst.
// ==================================================================================================

/// `dst[y*rw + x] = src[(r0+y)*W + (c0+x)]` for `(x,y)` in the sub-rect, over a 2D block/grid.
const SUBRECT_PTX: &str = r#"
    .visible .entry subrect(
        .param .u64 sr_src,
        .param .u64 sr_dst,
        .param .u32 sr_w,
        .param .u32 sr_r0,
        .param .u32 sr_c0,
        .param .u32 sr_rw,
        .param .u32 sr_rh
    )
    {
        ld.param.u64  %rsrc, [sr_src];
        ld.param.u64  %rdst, [sr_dst];
        ld.param.u32  %rw, [sr_w];
        ld.param.u32  %rr0, [sr_r0];
        ld.param.u32  %rc0, [sr_c0];
        ld.param.u32  %rrw, [sr_rw];
        ld.param.u32  %rrh, [sr_rh];
        mov.u32 %rnx, %ntid.x; mov.u32 %rcx, %ctaid.x; mov.u32 %rtx, %tid.x;
        mad.lo.s32 %x, %rcx, %rnx, %rtx;
        mov.u32 %rny, %ntid.y; mov.u32 %rcy, %ctaid.y; mov.u32 %rty, %tid.y;
        mad.lo.s32 %y, %rcy, %rny, %rty;
        setp.ge.s32 %px, %x, %rrw;
        @%px bra DONE;
        setp.ge.s32 %py, %y, %rrh;
        @%py bra DONE;
        add.s32 %srow, %rr0, %y;
        add.s32 %scol, %rc0, %x;
        mad.lo.s32 %sidx, %srow, %rw, %scol;
        mad.lo.s32 %didx, %y, %rrw, %x;
        cvta.to.global.u64 %gsrc, %rsrc;
        cvta.to.global.u64 %gdst, %rdst;
        mul.wide.s32 %soff, %sidx, 4;
        mul.wide.s32 %doff, %didx, 4;
        add.s64 %sp, %gsrc, %soff;
        add.s64 %dp, %gdst, %doff;
        ld.global.u32 %v, [%sp];
        st.global.u32 [%dp], %v;
    DONE: ret;
    }
"#;

#[test]
fn strided_subrect_copy_exact() {
    let (w, h) = (8usize, 6usize);
    let (r0, c0, rw, rh) = (1usize, 2usize, 4usize, 3usize);
    // Distinct value per source cell = row*W + col, so any mis-index is caught.
    let src: Vec<i32> = (0..w * h).map(|i| i as i32).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, SUBRECT_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "subrect").unwrap();

    let d_src = upload(&mut sink, &mut ctx, &i32s_to_bytes(&src));
    let d_dst = allocate::mem_alloc(&mut ctx, &mut sink, (rw * rh * 4) as u64).unwrap();

    let sc = |v: usize| KernelArg::Scalar((v as i32).to_le_bytes().to_vec());
    let args = vec![
        KernelArg::Ptr(d_src),
        KernelArg::Ptr(d_dst),
        sc(w),
        sc(r0),
        sc(c0),
        sc(rw),
        sc(rh),
    ];
    // block (4,4) covers rw=4, rh=3 (the y=3 lane is guarded off) in one block.
    launch::launch(&mut ctx, &mut sink, func, (1, 1, 1), (4, 4, 1), &args).unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_dst, rw * rh * 4));
    let mut want = vec![0i32; rw * rh];
    for y in 0..rh {
        for x in 0..rw {
            want[y * rw + x] = src[(r0 + y) * w + (c0 + x)];
        }
    }
    assert_eq!(got, want, "strided sub-rectangle copy layout");
    // Spot-check the closed form: dst[0] = src[(1)*8 + 2] = 10.
    assert_eq!(got[0], 10);
    assert_eq!(sink.executor().dispatches, 1);
}

// ==================================================================================================
// 6. shared-memory + bar.sync — block-scoped tree reduction, one partial per block, multi-block grid.
// ==================================================================================================

/// Each block loads its `blockDim` slice into `.shared`, tree-reduces with `bar.sync` between halving
/// steps, and thread 0 writes the block partial to `out[blockIdx.x]`. Exercises workgroup memory +
/// the cooperative barrier model across a multi-block grid.
const BLOCKREDUCE_PTX: &str = r#"
    .visible .entry blockreduce(
        .param .u64 br_in,
        .param .u64 br_out,
        .param .u32 br_n
    )
    {
        .shared .align 4 .b32 sdata[256];
        ld.param.u64  %rin, [br_in];
        ld.param.u64  %rout, [br_out];
        ld.param.u32  %rn, [br_n];
        mov.u32 %tid, %tid.x;
        mov.u32 %bd, %ntid.x;
        mov.u32 %cx, %ctaid.x;
        mad.lo.s32 %gid, %cx, %bd, %tid;
        mov.u32 %val, 0;
        setp.ge.s32 %pge, %gid, %rn;
        @%pge bra STORE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %ioff, %gid, 4;
        add.s64 %ip, %gin, %ioff;
        ld.global.u32 %val, [%ip];
    STORE:
        mul.wide.s32 %sidx, %tid, 4;
        st.shared.u32 [%sidx], %val;
        bar.sync;
        shr.u32 %s, %bd, 1;
    RLOOP:
        setp.gt.s32 %pcont, %s, 0;
        @!%pcont bra DONE_REDUCE;
        setp.lt.s32 %plt, %tid, %s;
        @!%plt bra SKIP;
        mul.wide.s32 %ia, %tid, 4;
        add.s32 %tids, %tid, %s;
        mul.wide.s32 %ib, %tids, 4;
        ld.shared.u32 %va, [%ia];
        ld.shared.u32 %vb, [%ib];
        add.s32 %sum, %va, %vb;
        st.shared.u32 [%ia], %sum;
    SKIP:
        bar.sync;
        shr.u32 %s, %s, 1;
        bra RLOOP;
    DONE_REDUCE:
        setp.ne.s32 %pnz, %tid, 0;
        @%pnz bra DONE;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %ooff, %cx, 4;
        add.s64 %op, %gout, %ooff;
        ld.shared.u32 %res, [0];
        st.global.u32 [%op], %res;
    DONE: ret;
    }
"#;

#[test]
fn shared_memory_block_reduction_exact() {
    let block = 8usize;
    let grid = 4usize;
    let n = block * grid; // 32 elements, one partial per block
    let input: Vec<i32> = (0..n).map(|i| (i as i32 * 3 + 1) % 17).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, BLOCKREDUCE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "blockreduce").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_out = allocate::mem_alloc(&mut ctx, &mut sink, (grid * 4) as u64).unwrap();

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_out),
        KernelArg::Scalar((n as i32).to_le_bytes().to_vec()),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args,
    )
    .unwrap();

    let partials = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, grid * 4));

    // Reference: each block sums its own contiguous slice.
    let want_partials: Vec<i32> = (0..grid)
        .map(|b| input[b * block..(b + 1) * block].iter().sum())
        .collect();
    assert_eq!(
        partials, want_partials,
        "per-block shared-memory partials, each exact"
    );

    // And the host-summed total equals the whole-array sum.
    let total: i32 = partials.iter().sum();
    assert_eq!(
        total,
        input.iter().sum::<i32>(),
        "grid total from block partials"
    );
    assert_eq!(sink.executor().dispatches, 1);
}
