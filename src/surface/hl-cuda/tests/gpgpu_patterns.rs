//! CUDA **real-GPGPU-pattern** demo battery — the algorithm patterns real CUDA applications actually use
//! (tiled matmul, prefix-scan, atomic histogram, stencil convolution, segmented reduction, coalesced
//! transpose, bitonic sort), each driven through the REAL hl-cuda PTX front-end → the reference
//! [`CpuExecutor`] kernel-IR interpreter → device readback → asserted **bit-exact, element by element**
//! against an independent CPU reference computed in the test.
//!
//! Every kernel here exercises the genuine pattern — `.shared` workgroup memory, `bar.sync` barriers,
//! `atom`/`red` atomics under cross-block contention, multi-block grids with real global indexing and
//! remainder handling — and every input is integer (or exactly-representable), so the assertions are
//! bit-exact with no float tolerance. If a barrier failed to synchronize, if a block's shared memory
//! leaked into another block, if an atomic dropped an update, or if the grid indexing were wrong, these
//! assertions would catch it.
//!
//! Wiring is identical to `tests/compute_demo.rs` / `tests/advanced_demo.rs`: the same in-process
//! [`InProcessCommandSink`] over the [`CpuExecutor`] with the PTX compiler injected.
//!
//! Batteries:
//!   1. `tiled_matmul`     — shared-memory blocked (TILE=16) 64×64 integer matmul, C == A·B exact.
//!   2. `prefix_scan`      — multi-block Hillis-Steele inclusive scan (shared mem + double barrier per
//!                           step) + host block-offset combine, exact inclusive AND exclusive scan.
//!   3. `histogram`        — atomic histogram into K bins, BOTH a global-atomic and a shared-privatized
//!                           (shared atomics + merge) variant, exact bin counts under contention.
//!   4. `convolution`      — 1D box stencil with a shared-memory halo tile, 2D 3×3 box blur, and a 3×3
//!                           Sobel |Gx|+|Gy|, each exact vs a CPU convolution.
//!   5. `reduce_segmented` — per-segment sum AND signed max via atomics into per-segment bins, exact.
//!   6. `transpose`        — shared-memory coalesced tiled matrix transpose (non-square, remainder), exact.
//!   7. `bitonic_sort`     — in-shared-memory bitonic sorting network (power-of-2 N) with a barrier per
//!                           compare-exchange substep, exact vs a sorted reference.

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
// shared harness — identical wiring to tests/compute_demo.rs.
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

/// Allocate a device buffer of `n` i32 slots, zero-initialised.
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
// 1. tiled_matmul — shared-memory blocked (TILE=16) integer matmul, C(N×N) = A(N×K) · B(K×N).
//    Each 16×16 block cooperatively stages a TILE of A and a TILE of B into `.shared`, `bar.sync`,
//    accumulates the tile product, `bar.sync`, and advances — the canonical CUDA tiled GEMM.
// ==================================================================================================

const MATMUL_TILED_PTX: &str = r#"
    .visible .entry mm_tiled(
        .param .u64 p_a,
        .param .u64 p_b,
        .param .u64 p_c,
        .param .u32 p_n,
        .param .u32 p_k,
        .param .u32 p_tiles
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
        cvta.to.global.u64 %gA, %ra;
        cvta.to.global.u64 %gB, %rb;
        cvta.to.global.u64 %gC, %rc;
        mov.u32 %tx, %tid.x;
        mov.u32 %ty, %tid.y;
        mov.u32 %bx, %ctaid.x;
        mov.u32 %by, %ctaid.y;
        mad.lo.s32 %row, %by, 16, %ty;
        mad.lo.s32 %col, %bx, 16, %tx;
        // per-thread shared slot byte offset: (ty*16 + tx) * 4
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
        // As[ty][tx] = A[row*K + (t*16 + tx)]
        mad.lo.s32 %acol, %t, 16, %tx;
        mad.lo.s32 %aidx, %row, %rk, %acol;
        mul.wide.s32 %aoff, %aidx, 4;
        add.s64 %aptr, %gA, %aoff;
        ld.global.u32 %av, [%aptr];
        st.shared.u32 [%asaddr], %av;
        // Bs[ty][tx] = B[(t*16 + ty)*N + col]
        mad.lo.s32 %brow, %t, 16, %ty;
        mad.lo.s32 %bidx, %brow, %rn, %col;
        mul.wide.s32 %boff, %bidx, 4;
        add.s64 %bptr, %gB, %boff;
        ld.global.u32 %bv, [%bptr];
        st.shared.u32 [%bsaddr], %bv;
        bar.sync;
        // inner: acc += As[ty][k] * Bs[k][tx], k = 0..16
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
        mul.wide.s32 %coff, %cidx, 4;
        add.s64 %cptr, %gC, %coff;
        st.global.u32 [%cptr], %acc;
        ret;
    }
"#;

#[test]
fn tiled_matmul_shared_memory_exact() {
    const TILE: usize = 16;
    let (n, k) = (64usize, 64usize); // square 64×64, K=64
                                     // Bounded signed values so the i32 accumulation cannot overflow (|a|,|b| ≤ 9 ⇒ |Σ| ≤ 64·81 = 5184).
    let a: Vec<i32> = (0..n * k)
        .map(|i| (i as i32 * 7 + 3).rem_euclid(19) - 9)
        .collect();
    let b: Vec<i32> = (0..k * n)
        .map(|i| (i as i32 * 5 + 1).rem_euclid(19) - 9)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, MATMUL_TILED_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "mm_tiled").unwrap();

    let da = upload(&mut sink, &mut ctx, &i32s_to_bytes(&a));
    let db = upload(&mut sink, &mut ctx, &i32s_to_bytes(&b));
    let dc = allocate::mem_alloc(&mut ctx, &mut sink, (n * n * 4) as u64).unwrap();

    let tiles = k / TILE; // 4
    let args = vec![
        KernelArg::Ptr(da),
        KernelArg::Ptr(db),
        KernelArg::Ptr(dc),
        sc(n as i32),
        sc(k as i32),
        sc(tiles as i32),
    ];
    // grid (4,4) blocks × block (16,16) threads = exactly the 64×64 output.
    launch::launch(&mut ctx, &mut sink, func, (4, 4, 1), (16, 16, 1), &args).unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, dc, n * n * 4));

    // CPU reference: wrapping i32 triple loop (matches the interpreter's 32-bit mad semantics exactly).
    let mut want = vec![0i32; n * n];
    for row in 0..n {
        for col in 0..n {
            let mut acc = 0i32;
            for kk in 0..k {
                acc = acc.wrapping_add(a[row * k + kk].wrapping_mul(b[kk * n + col]));
            }
            want[row * n + col] = acc;
        }
    }
    assert_eq!(got, want, "tiled matmul C = A·B, every element exact");
    // Spot-check one non-trivial element is actually non-zero (guards against an all-zero fake pass).
    assert!(
        want.iter().any(|&v| v != 0),
        "reference must be non-degenerate"
    );
    assert_eq!(sink.executor().dispatches, 1);
}

// ==================================================================================================
// 2. prefix_scan — multi-block inclusive scan. Each block does a Hillis-Steele inclusive scan of its
//    slice in `.shared` (a read barrier + a write barrier per doubling step, so no thread reads a slot
//    another is mid-writing), writes the block total to `sums[block]`; the host exclusively-scans the
//    block totals; a second kernel adds each block's offset. Result = the global inclusive scan.
// ==================================================================================================

const BLOCK_SCAN_PTX: &str = r#"
    .visible .entry block_scan(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u64 p_sums,
        .param .u32 p_n
    )
    {
        .shared .align 4 .b32 sh[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u64 %rsums, [p_sums];
        ld.param.u32 %rn, [p_n];
        mov.u32 %tid, %tid.x;
        mov.u32 %bd, %ntid.x;
        mov.u32 %cx, %ctaid.x;
        mad.lo.s32 %gid, %cx, %bd, %tid;
        mul.lo.s32 %toff, %tid, 4;
        // val = (gid < n) ? in[gid] : 0
        mov.u32 %val, 0;
        setp.ge.s32 %poob, %gid, %rn;
        @%poob bra STORE0;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %ioff, %gid, 4;
        add.s64 %iptr, %gin, %ioff;
        ld.global.u32 %val, [%iptr];
    STORE0:
        st.shared.u32 [%toff], %val;
        bar.sync;
        mov.u32 %d, 1;
    DLOOP:
        setp.ge.s32 %pd, %d, %bd;
        @%pd bra ENDD;
        // read phase: t = (tid >= d) ? sh[tid] + sh[tid-d] : sh[tid]
        ld.shared.u32 %tcur, [%toff];
        setp.lt.s32 %plt, %tid, %d;
        @%plt bra HAVE;
        sub.s32 %jidx, %tid, %d;
        mul.lo.s32 %joff, %jidx, 4;
        ld.shared.u32 %tprev, [%joff];
        add.s32 %tcur, %tcur, %tprev;
    HAVE:
        bar.sync;
        st.shared.u32 [%toff], %tcur;
        bar.sync;
        shl.b32 %d, %d, 1;
        bra DLOOP;
    ENDD:
        // out[gid] = sh[tid] for in-range lanes
        ld.shared.u32 %res, [%toff];
        setp.ge.s32 %poob2, %gid, %rn;
        @%poob2 bra MAYBESUM;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %ooff, %gid, 4;
        add.s64 %optr, %gout, %ooff;
        st.global.u32 [%optr], %res;
    MAYBESUM:
        // last lane writes the block total (sh[bd-1]) to sums[blockIdx]
        sub.s32 %last, %bd, 1;
        setp.ne.s32 %pnl, %tid, %last;
        @%pnl bra DONE;
        cvta.to.global.u64 %gsums, %rsums;
        mul.wide.s32 %soff2, %cx, 4;
        add.s64 %sptr, %gsums, %soff2;
        st.global.u32 [%sptr], %res;
    DONE:
        ret;
    }

    .visible .entry add_offset(
        .param .u64 p_out,
        .param .u64 p_off,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rout, [p_out];
        ld.param.u64 %roff, [p_off];
        ld.param.u32 %rn, [p_n];
        mov.u32 %tid, %tid.x;
        mov.u32 %bd, %ntid.x;
        mov.u32 %cx, %ctaid.x;
        mad.lo.s32 %gid, %cx, %bd, %tid;
        setp.ge.s32 %poob, %gid, %rn;
        @%poob bra DONE;
        cvta.to.global.u64 %goff, %roff;
        mul.wide.s32 %co, %cx, 4;
        add.s64 %offp, %goff, %co;
        ld.global.u32 %delta, [%offp];
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %go, %gid, 4;
        add.s64 %outp, %gout, %go;
        ld.global.u32 %v, [%outp];
        add.s32 %v, %v, %delta;
        st.global.u32 [%outp], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn prefix_scan_inclusive_and_exclusive_exact() {
    let block = 256usize;
    let n = 1000usize; // NOT a multiple of the block → last block is partial (remainder handling)
    let grid = (n + block - 1) / block; // 4 blocks
    let input: Vec<i32> = (0..n).map(|i| (i as i32 % 7) + 1).collect(); // 1..=7, sum ≤ 7000 (no overflow)

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, BLOCK_SCAN_PTX.as_bytes()).unwrap();
    let scan_fn = load_module::module_get_function(&ctx, module, "block_scan").unwrap();
    let add_fn = load_module::module_get_function(&ctx, module, "add_offset").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_sums = alloc_zeroed_i32(&mut sink, &mut ctx, grid);

    // Phase 1: per-block inclusive scan + block totals.
    let args1 = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_out),
        KernelArg::Ptr(d_sums),
        sc(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        scan_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args1,
    )
    .unwrap();

    // Phase 2 (host): exclusive scan of the per-block totals → per-block offsets.
    let sums = bytes_to_i32s(&readback(&mut sink, &ctx, d_sums, grid * 4));
    let mut offsets = vec![0i32; grid];
    let mut running = 0i32;
    for b in 0..grid {
        offsets[b] = running;
        running += sums[b];
    }
    let d_off = upload(&mut sink, &mut ctx, &i32s_to_bytes(&offsets));

    // Phase 3: add each block's offset into its elements → global inclusive scan.
    let args3 = vec![KernelArg::Ptr(d_out), KernelArg::Ptr(d_off), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        add_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args3,
    )
    .unwrap();

    let got_inclusive = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, n * 4));

    // Reference inclusive + exclusive scans.
    let mut want_incl = vec![0i32; n];
    let mut acc = 0i32;
    for i in 0..n {
        acc += input[i];
        want_incl[i] = acc;
    }
    assert_eq!(
        got_inclusive, want_incl,
        "multi-block inclusive prefix scan, every element exact"
    );

    // Exclusive scan derived from the device inclusive result: excl[i] = incl[i] - input[i].
    let got_exclusive: Vec<i32> = (0..n).map(|i| got_inclusive[i] - input[i]).collect();
    let mut want_excl = vec![0i32; n];
    let mut e = 0i32;
    for i in 0..n {
        want_excl[i] = e;
        e += input[i];
    }
    assert_eq!(
        got_exclusive, want_excl,
        "exclusive prefix scan, every element exact"
    );
    assert_eq!(
        want_incl[n - 1],
        input.iter().sum::<i32>(),
        "final inclusive = total sum"
    );
    assert_eq!(
        sink.executor().dispatches,
        2,
        "two kernel dispatches: block-scan + add-offset"
    );
}

// ==================================================================================================
// 3. histogram — K=16 bins over N inputs. Two variants, both exact under contention:
//    (a) GLOBAL atomics: every lane does red.global.add into its bin.
//    (b) SHARED-PRIVATIZED: each block accumulates a private histogram in `.shared` via shared atomics,
//        then merges it into the global histogram with one global atomic per bin (the standard fast path).
//    bin = value & (K-1) with K=16 a power of two (the interpreter models no integer modulo).
// ==================================================================================================

const HIST_GLOBAL_PTX: &str = r#"
    .visible .entry hist_global(
        .param .u64 p_in,
        .param .u64 p_hist,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rhist, [p_hist];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        and.b32 %bin, %v, 15;
        cvta.to.global.u64 %gh, %rhist;
        mul.wide.s32 %boff, %bin, 4;
        add.s64 %ph, %gh, %boff;
        red.global.add.u32 [%ph], 1;
    DONE:
        ret;
    }
"#;

const HIST_SHARED_PTX: &str = r#"
    .visible .entry hist_shared(
        .param .u64 p_in,
        .param .u64 p_hist,
        .param .u32 p_n
    )
    {
        .shared .align 4 .b32 sh[16];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rhist, [p_hist];
        ld.param.u32 %rn, [p_n];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        // zero the private histogram: lanes 0..16 clear one bin each.
        setp.ge.s32 %pz, %tt, 16;
        @%pz bra AFTERZERO;
        mul.lo.s32 %zoff, %tt, 4;
        st.shared.u32 [%zoff], 0;
    AFTERZERO:
        bar.sync;
        // accumulate into the private (shared) histogram with shared atomics.
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra AFTERACC;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        and.b32 %bin, %v, 15;
        mul.lo.s32 %sboff, %bin, 4;
        red.shared.add.u32 [%sboff], 1;
    AFTERACC:
        bar.sync;
        // merge: lanes 0..16 add one private bin into the global histogram.
        setp.ge.s32 %pm, %tt, 16;
        @%pm bra DONE;
        mul.lo.s32 %moff, %tt, 4;
        ld.shared.u32 %cnt, [%moff];
        cvta.to.global.u64 %gh, %rhist;
        mul.wide.s32 %goff, %tt, 4;
        add.s64 %ph, %gh, %goff;
        red.global.add.u32 [%ph], %cnt;
    DONE:
        ret;
    }
"#;

#[test]
fn histogram_atomic_global_and_shared_exact() {
    let k = 16usize;
    let n = 5000usize;
    // Skewed distribution so several bins receive heavy contention.
    let input: Vec<i32> = (0..n).map(|i| ((i * 2718281 + 13) % 251) as i32).collect();

    // Reference bin counts: bin = value & 15.
    let mut want = vec![0i32; k];
    for &v in &input {
        want[(v as u32 & 15) as usize] += 1;
    }
    assert!(
        want.iter().filter(|&&c| c > 0).count() >= 8,
        "distribution must actually spread across bins"
    );

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module_g = load_module::module_load_data(&mut ctx, HIST_GLOBAL_PTX.as_bytes()).unwrap();
    let gfn = load_module::module_get_function(&ctx, module_g, "hist_global").unwrap();
    let module_s = load_module::module_load_data(&mut ctx, HIST_SHARED_PTX.as_bytes()).unwrap();
    let sfn = load_module::module_get_function(&ctx, module_s, "hist_shared").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let grid = ((n + 255) / 256) as u32; // 20 blocks × 256 threads

    // (a) global-atomic histogram
    let d_hist_g = alloc_zeroed_i32(&mut sink, &mut ctx, k);
    let args_g = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_hist_g), sc(n as i32)];
    launch::launch(&mut ctx, &mut sink, gfn, (grid, 1, 1), (256, 1, 1), &args_g).unwrap();
    let got_g = bytes_to_i32s(&readback(&mut sink, &ctx, d_hist_g, k * 4));
    assert_eq!(got_g, want, "global-atomic histogram bin counts exact");
    assert_eq!(
        got_g.iter().sum::<i32>(),
        n as i32,
        "every input counted exactly once"
    );

    // (b) shared-privatized histogram
    let d_hist_s = alloc_zeroed_i32(&mut sink, &mut ctx, k);
    let args_s = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_hist_s), sc(n as i32)];
    launch::launch(&mut ctx, &mut sink, sfn, (grid, 1, 1), (256, 1, 1), &args_s).unwrap();
    let got_s = bytes_to_i32s(&readback(&mut sink, &ctx, d_hist_s, k * 4));
    assert_eq!(got_s, want, "shared-privatized histogram bin counts exact");
    assert_eq!(got_s, got_g, "shared and global histograms agree exactly");
}

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
    let module = load_module::module_load_data(&mut ctx, CONV1D_PTX.as_bytes()).unwrap();
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
    let module = load_module::module_load_data(&mut ctx, CONV2D_PTX.as_bytes()).unwrap();
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
    let module = load_module::module_load_data(&mut ctx, SOBEL_PTX.as_bytes()).unwrap();
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

// ==================================================================================================
// 5. reduce_segmented — per-segment sum AND signed max. Segments have a fixed width (a power of two so
//    seg = i >> log2(width), the interpreter modeling no integer division), so segment boundaries do NOT
//    align with block boundaries — atomics from many blocks land in the same segment bin, and the
//    cross-block totals must still be exact.
// ==================================================================================================

const SEG_REDUCE_PTX: &str = r#"
    .visible .entry seg_reduce(
        .param .u64 p_in,
        .param .u64 p_sum,
        .param .u64 p_max,
        .param .u32 p_n,
        .param .u32 p_shift
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rsum, [p_sum];
        ld.param.u64 %rmax, [p_max];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rshift, [p_shift];
        mov.u32 %nt, %ntid.x;
        mov.u32 %ct, %ctaid.x;
        mov.u32 %tt, %tid.x;
        mad.lo.s32 %i, %ct, %nt, %tt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        ld.global.u32 %v, [%pin];
        // seg = i >> shift
        shr.u32 %seg, %i, %rshift;
        mul.wide.s32 %soff, %seg, 4;
        cvta.to.global.u64 %gsum, %rsum;
        add.s64 %psum, %gsum, %soff;
        red.global.add.u32 [%psum], %v;
        cvta.to.global.u64 %gmax, %rmax;
        add.s64 %pmax, %gmax, %soff;
        red.global.max.s32 [%pmax], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn reduce_segmented_sum_and_max_exact() {
    let n = 1000usize;
    let seg_shift = 6u32; // segment width = 64
    let seg_w = 1usize << seg_shift;
    let nseg = (n + seg_w - 1) / seg_w; // 16 segments (last partial: 960..999)
    let input: Vec<i32> = (0..n).map(|i| (i as i32 * 37 + 11) % 200 - 60).collect(); // signed, spans ±

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, SEG_REDUCE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "seg_reduce").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_sum = alloc_zeroed_i32(&mut sink, &mut ctx, nseg);
    // seed the max bins with i32::MIN.
    let d_max = allocate::mem_alloc(&mut ctx, &mut sink, (nseg * 4) as u64).unwrap();
    transfer::memset(
        &mut ctx,
        &mut sink,
        d_max,
        &i32s_to_bytes(&vec![i32::MIN; nseg]),
    )
    .unwrap();

    let block = 128u32;
    let grid = ((n as u32) + block - 1) / block; // 8 blocks; boundaries cross segments
    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_sum),
        KernelArg::Ptr(d_max),
        sc(n as i32),
        sc(seg_shift as i32),
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

    let got_sum = bytes_to_i32s(&readback(&mut sink, &ctx, d_sum, nseg * 4));
    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_max, nseg * 4));

    // Reference per-segment sum + max.
    let mut want_sum = vec![0i32; nseg];
    let mut want_max = vec![i32::MIN; nseg];
    for i in 0..n {
        let s = i >> seg_shift;
        want_sum[s] += input[i];
        want_max[s] = want_max[s].max(input[i]);
    }
    assert_eq!(
        got_sum, want_sum,
        "per-segment sum exact (cross-block atomic accumulation)"
    );
    assert_eq!(got_max, want_max, "per-segment signed max exact");
    assert_eq!(
        got_sum.iter().sum::<i32>(),
        input.iter().sum::<i32>(),
        "segment sums cover every element"
    );
}

// ==================================================================================================
// 6. transpose — shared-memory coalesced tiled transpose. A 16×16 tile is staged into `.shared` with a
//    coalesced read, `bar.sync`, then written to the output at the transposed block offset (the classic
//    NVIDIA transpose). Non-square with a remainder tile on both axes; bounds-guarded.
// ==================================================================================================

const TRANSPOSE_PTX: &str = r#"
    .visible .entry transpose(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_w,
        .param .u32 p_h
    )
    {
        .shared .align 4 .b32 tile[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rw, [p_w];
        ld.param.u32 %rh, [p_h];
        mov.u32 %tx, %tid.x;
        mov.u32 %ty, %tid.y;
        mov.u32 %bx, %ctaid.x;
        mov.u32 %by, %ctaid.y;
        // load phase: x = bx*16+tx, y = by*16+ty ; tile[ty*16+tx] = in[y*w + x]
        mad.lo.s32 %x, %bx, 16, %tx;
        mad.lo.s32 %y, %by, 16, %ty;
        setp.ge.s32 %pxo, %x, %rw;
        @%pxo bra SYNC;
        setp.ge.s32 %pyo, %y, %rh;
        @%pyo bra SYNC;
        cvta.to.global.u64 %gin, %rin;
        mad.lo.s32 %iidx, %y, %rw, %x;
        mul.wide.s32 %io, %iidx, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %v, [%ip];
        mad.lo.s32 %tsi, %ty, 16, %tx;
        mul.lo.s32 %tsb, %tsi, 4;
        st.shared.u32 [%tsb], %v;
    SYNC:
        bar.sync;
        // store phase: xo = by*16+tx, yo = bx*16+ty ; out[yo*h + xo] = tile[tx*16+ty]
        mad.lo.s32 %xo, %by, 16, %tx;
        mad.lo.s32 %yo, %bx, 16, %ty;
        setp.ge.s32 %pxo2, %xo, %rh;
        @%pxo2 bra DONE;
        setp.ge.s32 %pyo2, %yo, %rw;
        @%pyo2 bra DONE;
        mad.lo.s32 %tri, %tx, 16, %ty;
        mul.lo.s32 %trb, %tri, 4;
        ld.shared.u32 %tv, [%trb];
        cvta.to.global.u64 %gout, %rout;
        mad.lo.s32 %oidx, %yo, %rh, %xo;
        mul.wide.s32 %oo, %oidx, 4;
        add.s64 %op, %gout, %oo;
        st.global.u32 [%op], %tv;
    DONE:
        ret;
    }
"#;

#[test]
fn transpose_shared_memory_coalesced_exact() {
    let (w, h) = (48usize, 34usize); // non-square, remainder tiles on both axes
    let input: Vec<i32> = (0..w * h).map(|i| i as i32).collect(); // distinct per cell → any misindex caught

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, TRANSPOSE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "transpose").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, w * h);
    let gx = ((w + 15) / 16) as u32; // 3
    let gy = ((h + 15) / 16) as u32; // 3

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_out),
        sc(w as i32),
        sc(h as i32),
    ];
    launch::launch(&mut ctx, &mut sink, func, (gx, gy, 1), (16, 16, 1), &args).unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, w * h * 4));

    // Reference: out[c][r] = in[r][c], out is (w rows × h cols).
    let mut want = vec![0i32; w * h];
    for r in 0..h {
        for c in 0..w {
            want[c * h + r] = input[r * w + c];
        }
    }
    assert_eq!(
        got, want,
        "shared-memory tiled transpose, every element exact"
    );
}

// ==================================================================================================
// 7. bitonic_sort — a bitonic sorting network over a power-of-2 N in `.shared`, one `bar.sync` per
//    compare-exchange substep. Only the lower index of each pair performs the swap, so writes never
//    race; every thread executes the identical (k,j) loop nest, hitting every barrier. Exact vs sorted.
// ==================================================================================================

const BITONIC_PTX: &str = r#"
    .visible .entry bitonic(
        .param .u64 p_data,
        .param .u32 p_n
    )
    {
        .shared .align 4 .b32 s[256];
        ld.param.u64 %rd, [p_data];
        ld.param.u32 %rn, [p_n];
        mov.u32 %tid, %tid.x;
        cvta.to.global.u64 %gd, %rd;
        mul.lo.s32 %toff, %tid, 4;
        mul.wide.s32 %goff, %tid, 4;
        add.s64 %gp, %gd, %goff;
        ld.global.u32 %mine, [%gp];
        st.shared.u32 [%toff], %mine;
        bar.sync;
        mov.u32 %k, 2;
    KLOOP:
        setp.gt.s32 %pkend, %k, %rn;
        @%pkend bra KEND;
        shr.u32 %j, %k, 1;
    JLOOP:
        setp.le.s32 %pjend, %j, 0;
        @%pjend bra JEND;
        xor.b32 %ixj, %tid, %j;
        // act only if ixj > tid (lower index owns the compare-exchange)
        setp.le.s32 %pskip, %ixj, %tid;
        @%pskip bra AFTER;
        mul.lo.s32 %ioff, %ixj, 4;
        ld.shared.u32 %a, [%toff];
        ld.shared.u32 %b, [%ioff];
        // direction: ascending when (tid & k) == 0, else descending
        and.b32 %tk, %tid, %k;
        setp.eq.s32 %pasc, %tk, 0;
        @%pasc bra ASC;
        // descending: swap if a < b
        setp.lt.s32 %psw, %a, %b;
        @%psw bra SWAP;
        bra AFTER;
    ASC:
        // ascending: swap if a > b
        setp.gt.s32 %psw2, %a, %b;
        @%psw2 bra SWAP;
        bra AFTER;
    SWAP:
        st.shared.u32 [%toff], %b;
        st.shared.u32 [%ioff], %a;
    AFTER:
        bar.sync;
        shr.u32 %j, %j, 1;
        bra JLOOP;
    JEND:
        shl.b32 %k, %k, 1;
        bra KLOOP;
    KEND:
        ld.shared.u32 %res, [%toff];
        st.global.u32 [%gp], %res;
        ret;
    }
"#;

#[test]
fn bitonic_sort_power_of_two_exact() {
    let n = 256usize; // power of two, single block
                      // A deterministic pseudo-shuffle with duplicates (stability irrelevant for value-equality of a sort).
    let input: Vec<i32> = (0..n)
        .map(|i| ((i * 1103515245 + 12345) % 1000) as i32)
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, BITONIC_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "bitonic").unwrap();

    let d_data = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let args = vec![KernelArg::Ptr(d_data), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, 1),
        (n as u32, 1, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_data, n * 4));

    let mut want = input.clone();
    want.sort();
    assert_eq!(got, want, "bitonic sort produces the exact ascending order");
    // Sanity: it really was unsorted to begin with (guards against a no-op fake pass).
    assert_ne!(input, want, "input must start unsorted");
    assert_eq!(sink.executor().dispatches, 1);
}

// ==================================================================================================
// 8. special-register OPERANDS — the silent-wrong-result footgun (task #230). Special registers are
//    legal PTX ALU operands used DIRECTLY, with NO `mov` first (e.g.
//    `mad.lo.s32 %idx, %ntid.x, %ctaid.x, %tid.x;` computes blockDim*blockIdx + threadIdx). The old
//    front-end recognized `%ntid.x`/`%tid.x`/… only inside a `mov`; used as an operand they were
//    silently interned as fresh ZERO registers, so every thread computed global index 0. These tests
//    dispatch a multi-block grid and assert the operand form yields the exact per-thread global index,
//    bit-identical to the `mov`-first spelling — and that an UNKNOWN special register operand ERRORS
//    rather than silently zeroing.
// ==================================================================================================

// `out[gidx] = gidx`, with the global index read straight from special registers as `mad` operands.
const SREG_OPERAND_PTX: &str = r#"
    .visible .entry gidx_operand(
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %n, [p_n];
        cvta.to.global.u64 %gout, %rout;
        // NO mov first: special registers are the ALU operands directly.
        mad.lo.s32 %idx, %ntid.x, %ctaid.x, %tid.x;
        setp.ge.s32 %pdone, %idx, %n;
        @%pdone bra DONE;
        mul.wide.s32 %off, %idx, 4;
        add.s64 %addr, %gout, %off;
        st.global.u32 [%addr], %idx;
    DONE:
        ret;
    }
"#;

// Bit-exact reference: the same kernel written the old `mov`-first way.
const SREG_MOVFIRST_PTX: &str = r#"
    .visible .entry gidx_movfirst(
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %n, [p_n];
        cvta.to.global.u64 %gout, %rout;
        mov.u32 %rntid, %ntid.x;
        mov.u32 %rctaid, %ctaid.x;
        mov.u32 %rtid, %tid.x;
        mad.lo.s32 %idx, %rntid, %rctaid, %rtid;
        setp.ge.s32 %pdone, %idx, %n;
        @%pdone bra DONE;
        mul.wide.s32 %off, %idx, 4;
        add.s64 %addr, %gout, %off;
        st.global.u32 [%addr], %idx;
    DONE:
        ret;
    }
"#;

fn run_gidx(ptx_src: &str, entry: &str, grid: u32, block: u32, n: usize) -> Vec<i32> {
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, ptx_src.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, entry).unwrap();
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let args = vec![KernelArg::Ptr(d_out), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (grid, 1, 1),
        (block, 1, 1),
        &args,
    )
    .unwrap();
    bytes_to_i32s(&readback(&mut sink, &ctx, d_out, n * 4))
}

#[test]
fn special_register_as_operand_computes_global_index() {
    let (grid, block) = (5u32, 8u32); // multi-block grid: 40 threads across 5 blocks
    let n = (grid * block) as usize;

    let got_operand = run_gidx(SREG_OPERAND_PTX, "gidx_operand", grid, block, n);
    let got_movfirst = run_gidx(SREG_MOVFIRST_PTX, "gidx_movfirst", grid, block, n);

    // The correct per-thread global index: out[i] == i for every thread in the grid.
    let want: Vec<i32> = (0..n as i32).collect();

    // The operand form must compute the REAL index — not a silent all-zero (the footgun would give
    // out == [0,0,…] since block 0 thread 0 is the only writer of slot 0).
    assert_eq!(
        got_operand, want,
        "sreg-as-operand global index, every thread exact"
    );
    assert!(
        got_operand.iter().any(|&v| v != 0),
        "guards against the all-zero silent-wrong footgun"
    );
    // …and it is bit-identical to the mov-first spelling.
    assert_eq!(
        got_operand, got_movfirst,
        "operand form == mov-first form, bit-exact"
    );
}

#[test]
fn sreg_operand_ir_matches_mov_first() {
    // Same guarantee at the IR level: both spellings compile and, run over identical launch config,
    // must be observationally identical (already asserted above), and both must actually reference the
    // special registers (a MovSReg for each of ntid/ctaid/tid appears in each program).
    let block = [8u32, 1, 1];
    let op = ptx::compile(SREG_OPERAND_PTX, "gidx_operand", block).unwrap();
    let mv = ptx::compile(SREG_MOVFIRST_PTX, "gidx_movfirst", block).unwrap();
    let count_movsreg = |p: &hl_gpu::protocol::model::kernel::KernelProgram| {
        p.insts
            .iter()
            .filter(|i| matches!(i, hl_gpu::protocol::model::kernel::Inst::MovSReg { .. }))
            .count()
    };
    // Operand form materializes its three sregs via a MovSReg prelude; mov-first via its three movs.
    assert_eq!(
        count_movsreg(&op),
        3,
        "operand form materializes ntid/ctaid/tid via MovSReg prelude"
    );
    assert_eq!(
        count_movsreg(&mv),
        3,
        "mov-first form materializes ntid/ctaid/tid via MovSReg"
    );
}

#[test]
fn unknown_special_register_operand_errors_not_silent_zero() {
    // An unknown/mistyped special register used as an operand (`%bogus.x`, `%ntid.w`, or an unmodeled
    // dotless sreg `%warpid`) must ERROR — never be silently interned as a fresh zero register.
    for bad in [
        "mad.lo.s32 %idx, %ntid.x, %ctaid.x, %bogus.x;",
        "mad.lo.s32 %idx, %ntid.w, %ctaid.x, %tid.x;",
        "add.s32 %idx, %warpid, %tid.x;",
    ] {
        let src = format!(
            ".visible .entry k(.param .u64 p_out) {{ ld.param.u64 %r, [p_out]; {bad} ret; }}"
        );
        let r = ptx::compile(&src, "k", [8, 1, 1]);
        assert!(
            r.is_err(),
            "unknown special register must error, got Ok for `{bad}`"
        );
    }

    // The same guard applies to the `mov` form: `mov %r, %ntid.w` is not a silent zero read either.
    let src = ".visible .entry k(.param .u64 p_out) { ld.param.u64 %r, [p_out]; mov.u32 %z, %ntid.w; ret; }";
    assert!(
        ptx::compile(src, "k", [8, 1, 1]).is_err(),
        "unknown sreg in mov must error too"
    );

    // Control: the well-formed operand kernel still compiles.
    assert!(ptx::compile(SREG_OPERAND_PTX, "gidx_operand", [8, 1, 1]).is_ok());
}
