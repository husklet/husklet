//! CUDA **real-GPGPU-algorithm** demo battery — a second tier of genuine parallel algorithms (sort,
//! spectral transform, graph traversal, sparse linear algebra, stream compaction, Monte-Carlo, k-means,
//! running reductions) beyond the primitive-pattern battery in `tests/gpgpu_patterns.rs`. Each is driven
//! through the REAL hl-cuda PTX front-end → the reference [`CpuExecutor`] kernel-IR interpreter → device
//! readback → asserted **bit-exact, element by element** against an independent CPU reference computed in
//! the test.
//!
//! Everything is integer or fixed-point (the DFT uses an integer twiddle table; Monte-Carlo uses a
//! deterministic integer LCG — NO clock, NO float rng), so the assertions carry ZERO float tolerance. The
//! kernels exercise the structure real applications rely on: multi-pass ping-pong grids (merge sort),
//! irregular CSR / scatter access (SpMV, BFS, compaction), cross-block atomics (BFS frontier count,
//! k-means accumulation, Monte-Carlo hit count), power-of-two modular twiddle indexing (DFT), and shared
//! memory + `bar.sync` running-reduction scans (prefix min/max). Every reference is non-degenerate and
//! every sort/compaction asserts its input was NOT already in the answer shape (anti-false-pass guards).
//!
//! Wiring is identical to `tests/gpgpu_patterns.rs`: the same in-process [`InProcessCommandSink`] over the
//! [`CpuExecutor`] with the PTX compiler injected.
//!
//! Batteries:
//!   1. `merge_sort`       — iterative bottom-up multi-block merge sort of N u32 keys, ping-pong buffers,
//!                           one kernel launch per doubling pass. Exact vs a stable-sorted reference.
//!   2. `dft_fixed_point`  — fixed-point DFT of an N=16 signal with an INTEGER twiddle table and power-of-two
//!                           modular index `(k·n)&(N−1)`. Exact real/imag bins vs the same integer DFT on CPU.
//!   3. `bfs_frontier`     — one BFS level-expansion step (pull model) over a symmetric CSR graph, with a
//!                           cross-block atomic frontier count. Exact new `dist`, frontier mask, and count.
//!   4. `spmv_csr`         — sparse matrix–vector product in CSR (ragged rows incl. an empty row). Exact.
//!   5. `stream_compaction`— predicate → multi-block inclusive prefix scan → scatter, compacting the
//!                           elements that pass. Exact compacted prefix + exact count.
//!   6. `monte_carlo_pi`   — deterministic per-thread LCG (seeded by thread index) → integer count of
//!                           samples inside the quarter circle, summed by atomics. Exact hit count vs the
//!                           identical LCG replayed on CPU (bit-exact count, NOT a statistical π).
//!   7. `kmeans_step`      — one k-means iteration: assign each point to its nearest of K centroids by
//!                           integer L2, then atomically accumulate per-cluster coordinate sums + counts.
//!                           Exact assignment, sums, and counts.
//!   8. `running_minmax`   — inclusive Hillis-Steele running min AND running max scans in shared memory
//!                           (a barrier per doubling step). Exact prefix-min and prefix-max.

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
fn u32s_to_bytes(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn bytes_to_u32s(raw: &[u8]) -> Vec<u32> {
    raw.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
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
// 1. merge_sort — iterative bottom-up multi-block merge sort of N u32 keys. Pass p merges adjacent
//    sorted runs of length `runlen = 1<<p` into runs of length `2·runlen`: each thread owns one merge
//    task (a pair of adjacent runs), does a sequential two-pointer merge into the OTHER buffer, and the
//    host ping-pongs the two device buffers between passes. Multi-block: at `runlen=1` there are N/2 merge
//    tasks spread across many blocks. Stable (`<=` keeps the left/earlier element first). Unsigned key
//    comparison (`setp.le.u32`) so keys above i32::MAX sort correctly.
// ==================================================================================================

const MERGE_PASS_PTX: &str = r#"
    .visible .entry merge_pass(
        .param .u64 p_in,
        .param .u64 p_out,
        .param .u32 p_n,
        .param .u32 p_runlen
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rrl, [p_runlen];
        mad.lo.s32 %gid, %ctaid.x, %ntid.x, %tid.x;
        shl.b32 %tworl, %rrl, 1;
        mul.lo.s32 %ls, %gid, %tworl;
        // if left_start >= n: nothing to do
        setp.ge.s32 %pdone, %ls, %rn;
        @%pdone bra DONE;
        // mid = min(ls + runlen, n)
        add.s32 %mid, %ls, %rrl;
        setp.le.s32 %pm, %mid, %rn;
        @%pm bra MIDOK;
        mov.u32 %mid, %rn;
    MIDOK:
        // rend = min(ls + 2*runlen, n)
        add.s32 %rend, %ls, %tworl;
        setp.le.s32 %pr, %rend, %rn;
        @%pr bra RENDOK;
        mov.u32 %rend, %rn;
    RENDOK:
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gout, %rout;
        mov.u32 %i, %ls;
        mov.u32 %j, %mid;
        mov.u32 %k, %ls;
    MERGE:
        setp.ge.s32 %pil, %i, %mid;
        @%pil bra DRAINR;
        setp.ge.s32 %pjr, %j, %rend;
        @%pjr bra DRAINL;
        // a = in[i], b = in[j]
        mul.wide.s32 %io, %i, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %a, [%ip];
        mul.wide.s32 %jo, %j, 4;
        add.s64 %jp, %gin, %jo;
        ld.global.u32 %b, [%jp];
        mul.wide.s32 %ko, %k, 4;
        add.s64 %kp, %gout, %ko;
        setp.le.u32 %ple, %a, %b;
        @%ple bra TAKEL;
        // take right
        st.global.u32 [%kp], %b;
        add.s32 %j, %j, 1;
        bra ADVK;
    TAKEL:
        st.global.u32 [%kp], %a;
        add.s32 %i, %i, 1;
    ADVK:
        add.s32 %k, %k, 1;
        bra MERGE;
    DRAINL:
        // copy remaining left [i, mid)
        setp.ge.s32 %pdl, %i, %mid;
        @%pdl bra DONE;
        mul.wide.s32 %io2, %i, 4;
        add.s64 %ip2, %gin, %io2;
        ld.global.u32 %lv, [%ip2];
        mul.wide.s32 %ko2, %k, 4;
        add.s64 %kp2, %gout, %ko2;
        st.global.u32 [%kp2], %lv;
        add.s32 %i, %i, 1;
        add.s32 %k, %k, 1;
        bra DRAINL;
    DRAINR:
        // copy remaining right [j, rend)
        setp.ge.s32 %pdr, %j, %rend;
        @%pdr bra DONE;
        mul.wide.s32 %jo2, %j, 4;
        add.s64 %jp2, %gin, %jo2;
        ld.global.u32 %rv, [%jp2];
        mul.wide.s32 %ko3, %k, 4;
        add.s64 %kp3, %gout, %ko3;
        st.global.u32 [%kp3], %rv;
        add.s32 %j, %j, 1;
        add.s32 %k, %k, 1;
        bra DRAINR;
    DONE:
        ret;
    }
"#;

#[test]
fn merge_sort_multiblock_u32_exact() {
    let n = 1000usize; // NOT a power of two → partial final runs exercise the min() clamps
    let block = 128u32;
    // A deterministic pseudo-shuffle including values ABOVE i32::MAX (so unsigned compare is load-bearing).
    let input: Vec<u32> = (0..n)
        .map(|i| {
            let base = ((i as u64 * 2654435761 + 1013904223) & 0xFFFF_FFFF) as u32;
            if i % 5 == 0 {
                base | 0x8000_0000 // force some keys into the high (unsigned-only) half
            } else {
                base & 0x7FFF_FFFF
            }
        })
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(MERGE_PASS_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "merge_pass").unwrap();

    let mut buf_a = upload(&mut sink, &mut ctx, &u32s_to_bytes(&input));
    let mut buf_b = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();

    let mut passes = 0u32;
    let mut runlen = 1u32;
    while (runlen as usize) < n {
        let tasks = (n as u32 + 2 * runlen - 1) / (2 * runlen);
        let grid = (tasks + block - 1) / block;
        let args = vec![
            KernelArg::Ptr(buf_a),
            KernelArg::Ptr(buf_b),
            sc(n as i32),
            sc(runlen as i32),
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
        std::mem::swap(&mut buf_a, &mut buf_b); // output becomes the input of the next pass
        runlen <<= 1;
        passes += 1;
    }

    let got = bytes_to_u32s(&readback(&mut sink, &ctx, buf_a, n * 4));

    let mut want = input.clone();
    want.sort_unstable();
    assert_eq!(
        got, want,
        "multi-block merge sort produces the exact ascending order"
    );
    // Anti-false-pass: input really was unsorted, and the result is a genuine permutation of the input.
    assert_ne!(input, want, "input must start unsorted");
    let mut got_sorted = got.clone();
    got_sorted.sort_unstable();
    assert_eq!(
        got_sorted, want,
        "output is a permutation of the input (no keys lost/duplicated)"
    );
    assert!(
        want.iter().any(|&k| k > i32::MAX as u32),
        "high-half keys present → unsigned compare tested"
    );
    assert_eq!(passes, 10, "ceil(log2(1000)) = 10 ping-pong passes");
    assert_eq!(
        sink.executor().dispatches,
        10,
        "one dispatch per merge pass"
    );
}

// ==================================================================================================
// 2. dft_fixed_point — fixed-point Discrete Fourier Transform of an N=16 real signal. Twiddles are a
//    precomputed INTEGER table `cosT[m] = round(cos(2π·m/N)·Q)`, `sinT[m] = round(sin(2π·m/N)·Q)` with a
//    fixed-point scale Q; each output bin k reads `cosT[(k·n) & (N−1)]` (power-of-two modular index — the
//    interpreter models no integer modulo). One thread per bin over a multi-block grid.
//        X_re[k] = Σ_n x[n]·cosT[(k·n)&(N−1)]        X_im[k] = −Σ_n x[n]·sinT[(k·n)&(N−1)]
// ==================================================================================================

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

const SPMV_PTX: &str = r#"
    .visible .entry spmv(
        .param .u64 p_roff,
        .param .u64 p_col,
        .param .u64 p_val,
        .param .u64 p_x,
        .param .u64 p_y,
        .param .u32 p_rows
    )
    {
        ld.param.u64 %rroff, [p_roff];
        ld.param.u64 %rcol, [p_col];
        ld.param.u64 %rval, [p_val];
        ld.param.u64 %rx, [p_x];
        ld.param.u64 %ry, [p_y];
        ld.param.u32 %rrows, [p_rows];
        mad.lo.s32 %row, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %row, %rrows;
        @%pdone bra DONE;
        cvta.to.global.u64 %groff, %rroff;
        cvta.to.global.u64 %gcol, %rcol;
        cvta.to.global.u64 %gval, %rval;
        cvta.to.global.u64 %gx, %rx;
        mul.wide.s32 %ro, %row, 4;
        add.s64 %rp, %groff, %ro;
        ld.global.u32 %start, [%rp];
        ld.global.u32 %end, [%rp+4];
        mov.u32 %acc, 0;
        mov.u32 %e, %start;
    LOOP:
        setp.ge.s32 %pl, %e, %end;
        @%pl bra STORE;
        mul.wide.s32 %eo, %e, 4;
        add.s64 %vp, %gval, %eo;
        ld.global.u32 %av, [%vp];
        add.s64 %cp, %gcol, %eo;
        ld.global.u32 %c, [%cp];
        mul.wide.s32 %xo, %c, 4;
        add.s64 %xp, %gx, %xo;
        ld.global.u32 %xv, [%xp];
        mad.lo.s32 %acc, %av, %xv, %acc;
        add.s32 %e, %e, 1;
        bra LOOP;
    STORE:
        cvta.to.global.u64 %gy, %ry;
        add.s64 %yp, %gy, %ro;
        st.global.u32 [%yp], %acc;
    DONE:
        ret;
    }
"#;

#[test]
fn spmv_csr_exact() {
    // 6×6 sparse matrix in CSR. Row 3 is empty (a real ragged-CSR edge case → y[3] == 0).
    let rows = 6usize;
    let cols = 6usize;
    // (col, val) per row
    let row_entries: Vec<Vec<(i32, i32)>> = vec![
        vec![(1, 3), (3, 2)],         // 0
        vec![(0, 5)],                 // 1
        vec![(2, 1), (4, 4), (5, 2)], // 2
        vec![],                       // 3  (empty)
        vec![(1, 7), (5, 1)],         // 4
        vec![(3, 6)],                 // 5
    ];
    let x: Vec<i32> = vec![2, -1, 4, 3, 5, -2];
    assert_eq!(x.len(), cols);

    let mut row_off = vec![0i32; rows + 1];
    let mut col = Vec::new();
    let mut val = Vec::new();
    for r in 0..rows {
        for &(c, vv) in &row_entries[r] {
            col.push(c);
            val.push(vv);
        }
        row_off[r + 1] = col.len() as i32;
    }

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(SPMV_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "spmv").unwrap();

    let d_roff = upload(&mut sink, &mut ctx, &i32s_to_bytes(&row_off));
    let d_col = upload(&mut sink, &mut ctx, &i32s_to_bytes(&col));
    let d_val = upload(&mut sink, &mut ctx, &i32s_to_bytes(&val));
    let d_x = upload(&mut sink, &mut ctx, &i32s_to_bytes(&x));
    let d_y = alloc_zeroed_i32(&mut sink, &mut ctx, rows);

    let block = 4u32;
    let grid = (rows as u32 + block - 1) / block; // 2 blocks
    let args = vec![
        KernelArg::Ptr(d_roff),
        KernelArg::Ptr(d_col),
        KernelArg::Ptr(d_val),
        KernelArg::Ptr(d_x),
        KernelArg::Ptr(d_y),
        sc(rows as i32),
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

    let got = bytes_to_i32s(&readback(&mut sink, &ctx, d_y, rows * 4));

    // CPU reference.
    let mut want = vec![0i32; rows];
    for r in 0..rows {
        let mut acc = 0i32;
        for &(c, vv) in &row_entries[r] {
            acc += vv * x[c as usize];
        }
        want[r] = acc;
    }
    assert_eq!(got, want, "SpMV (CSR) y = A·x exact");
    assert_eq!(got[3], 0, "empty row yields 0");
    assert!(want.iter().any(|&v| v != 0), "reference is non-degenerate");
}

// ==================================================================================================
// 5. stream_compaction — compact the elements passing a predicate, in order. Three device stages plus a
//    host block-offset combine:
//      (a) `predicate`   — flag[i] = keep(in[i]) ? 1 : 0  (here: even values).
//      (b) multi-block inclusive prefix scan of flags → incl (block Hillis-Steele + host offsets +
//          add_offset — the same scan skeleton as gpgpu_patterns::prefix_scan).
//      (c) `scatter`     — if flag[i]: out[incl[i]-1] = in[i]   (incl[i]-1 == exclusive output position).
//    count = incl[n-1]. The compacted prefix out[0..count] must equal the CPU filter, exactly.
// ==================================================================================================

const PREDICATE_PTX: &str = r#"
    .visible .entry predicate(
        .param .u64 p_in,
        .param .u64 p_flag,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rflag, [p_flag];
        ld.param.u32 %rn, [p_n];
        mad.lo.s32 %i, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %i, %rn;
        @%pdone bra DONE;
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %o, %i, 4;
        add.s64 %ip, %gin, %o;
        ld.global.u32 %v, [%ip];
        // flag = (v & 1) == 0 ? 1 : 0   (keep evens)
        and.b32 %lsb, %v, 1;
        mov.u32 %flag, 0;
        setp.ne.s32 %podd, %lsb, 0;
        @%podd bra WRITE;
        mov.u32 %flag, 1;
    WRITE:
        cvta.to.global.u64 %gflag, %rflag;
        add.s64 %fp, %gflag, %o;
        st.global.u32 [%fp], %flag;
    DONE:
        ret;
    }
"#;

// Per-block inclusive Hillis-Steele scan (+ block totals) — the gpgpu_patterns::prefix_scan skeleton.
const SCAN_PTX: &str = r#"
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
        ld.shared.u32 %res, [%toff];
        setp.ge.s32 %poob2, %gid, %rn;
        @%poob2 bra MAYBESUM;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %ooff, %gid, 4;
        add.s64 %optr, %gout, %ooff;
        st.global.u32 [%optr], %res;
    MAYBESUM:
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

const SCATTER_PTX: &str = r#"
    .visible .entry scatter(
        .param .u64 p_in,
        .param .u64 p_flag,
        .param .u64 p_incl,
        .param .u64 p_out,
        .param .u32 p_n
    )
    {
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %rflag, [p_flag];
        ld.param.u64 %rincl, [p_incl];
        ld.param.u64 %rout, [p_out];
        ld.param.u32 %rn, [p_n];
        mad.lo.s32 %i, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %i, %rn;
        @%pdone bra DONE;
        cvta.to.global.u64 %gflag, %rflag;
        mul.wide.s32 %o, %i, 4;
        add.s64 %fp, %gflag, %o;
        ld.global.u32 %f, [%fp];
        setp.eq.s32 %pskip, %f, 0;
        @%pskip bra DONE;
        // pos = incl[i] - 1
        cvta.to.global.u64 %gincl, %rincl;
        add.s64 %sp, %gincl, %o;
        ld.global.u32 %s, [%sp];
        sub.s32 %pos, %s, 1;
        // out[pos] = in[i]
        cvta.to.global.u64 %gin, %rin;
        add.s64 %ip, %gin, %o;
        ld.global.u32 %v, [%ip];
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %po, %pos, 4;
        add.s64 %op, %gout, %po;
        st.global.u32 [%op], %v;
    DONE:
        ret;
    }
"#;

#[test]
fn stream_compaction_predicate_scan_scatter_exact() {
    let block = 256usize;
    let n = 1000usize; // multi-block, non-multiple of block → partial last block
    let grid = (n + block - 1) / block; // 4 blocks
    let input: Vec<i32> = (0..n).map(|i| (i as i32 * 37 + 11) % 100).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let m_pred = ctx.load_module(PREDICATE_PTX.as_bytes()).unwrap();
    let pred_fn = load_module::module_get_function(&ctx, m_pred, "predicate").unwrap();
    let m_scan = ctx.load_module(SCAN_PTX.as_bytes()).unwrap();
    let scan_fn = load_module::module_get_function(&ctx, m_scan, "block_scan").unwrap();
    let add_fn = load_module::module_get_function(&ctx, m_scan, "add_offset").unwrap();
    let m_scat = ctx.load_module(SCATTER_PTX.as_bytes()).unwrap();
    let scat_fn = load_module::module_get_function(&ctx, m_scat, "scatter").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_flag = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_incl = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_sums = alloc_zeroed_i32(&mut sink, &mut ctx, grid);
    let d_out = alloc_zeroed_i32(&mut sink, &mut ctx, n);

    // (a) predicate → flags
    let args_p = vec![KernelArg::Ptr(d_in), KernelArg::Ptr(d_flag), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        pred_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args_p,
    )
    .unwrap();

    // (b) inclusive scan of flags → incl (per-block scan, host offsets, add_offset)
    let args_s = vec![
        KernelArg::Ptr(d_flag),
        KernelArg::Ptr(d_incl),
        KernelArg::Ptr(d_sums),
        sc(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        scan_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args_s,
    )
    .unwrap();
    let sums = bytes_to_i32s(&readback(&mut sink, &ctx, d_sums, grid * 4));
    let mut offsets = vec![0i32; grid];
    let mut running = 0i32;
    for b in 0..grid {
        offsets[b] = running;
        running += sums[b];
    }
    let d_off = upload(&mut sink, &mut ctx, &i32s_to_bytes(&offsets));
    let args_a = vec![KernelArg::Ptr(d_incl), KernelArg::Ptr(d_off), sc(n as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        add_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args_a,
    )
    .unwrap();

    // (c) scatter kept elements to their scanned positions
    let args_c = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_flag),
        KernelArg::Ptr(d_incl),
        KernelArg::Ptr(d_out),
        sc(n as i32),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        scat_fn,
        (grid as u32, 1, 1),
        (block as u32, 1, 1),
        &args_c,
    )
    .unwrap();

    let incl = bytes_to_i32s(&readback(&mut sink, &ctx, d_incl, n * 4));
    let count = incl[n - 1] as usize;
    let out = bytes_to_i32s(&readback(&mut sink, &ctx, d_out, n * 4));

    // CPU reference: filter evens, preserving order.
    let want: Vec<i32> = input.iter().copied().filter(|v| v & 1 == 0).collect();
    assert_eq!(count, want.len(), "compacted count exact");
    assert_eq!(
        &out[..count],
        &want[..],
        "compacted prefix equals the filtered elements, in order"
    );
    // Anti-false-pass: the predicate actually filtered (some kept, some dropped).
    assert!(
        count > 0 && count < n,
        "predicate is non-trivial: 0 < count < n"
    );
    assert!(
        out[..count].iter().all(|v| v & 1 == 0),
        "every compacted element passes the predicate"
    );
}

// ==================================================================================================
// 6. monte_carlo_pi — deterministic per-thread LCG, count-in-quarter-circle. Thread t seeds an LCG from
//    its GLOBAL index (NO clock, NO device rng), draws M samples: two LCG steps give (x,y) in
//    [0, 2^12)^2 (top 12 bits of each 32-bit state), and a sample is a hit when `x²+y² ≤ R²`
//    (R = 2^12−1, so x²+y² ≤ ~3.4e7 stays well inside i32). Each thread accumulates its local hit count,
//    then one `red.global.add` folds it into a global counter. The result is a deterministic INTEGER hit
//    count — asserted bit-exact against the identical LCG replayed on CPU (this is NOT a statistical π).
// ==================================================================================================

const MONTECARLO_PTX: &str = r#"
    .visible .entry mc_pi(
        .param .u64 p_cnt,
        .param .u32 p_iters,
        .param .u32 p_r2
    )
    {
        ld.param.u64 %rcnt, [p_cnt];
        ld.param.u32 %riters, [p_iters];
        ld.param.u32 %rr2, [p_r2];
        mad.lo.s32 %gid, %ctaid.x, %ntid.x, %tid.x;
        // seed = gid * 2654435761 + 1   (32-bit)
        mul.lo.u32 %s, %gid, 2654435761;
        add.s32 %s, %s, 1;
        mov.u32 %hits, 0;
        mov.u32 %it, 0;
    LOOP:
        setp.ge.s32 %pl, %it, %riters;
        @%pl bra STORE;
        // x from one LCG step
        mul.lo.u32 %s, %s, 1664525;
        add.s32 %s, %s, 1013904223;
        shr.u32 %x, %s, 20;
        // y from the next LCG step
        mul.lo.u32 %s, %s, 1664525;
        add.s32 %s, %s, 1013904223;
        shr.u32 %y, %s, 20;
        // d = x*x + y*y
        mul.lo.s32 %xx, %x, %x;
        mul.lo.s32 %yy, %y, %y;
        add.s32 %d, %xx, %yy;
        setp.gt.s32 %pmiss, %d, %rr2;
        @%pmiss bra NEXT;
        add.s32 %hits, %hits, 1;
    NEXT:
        add.s32 %it, %it, 1;
        bra LOOP;
    STORE:
        cvta.to.global.u64 %gcnt, %rcnt;
        red.global.add.u32 [%gcnt], %hits;
        ret;
    }
"#;

// The identical LCG replayed on CPU — must produce the exact same integer hit count.
fn mc_reference(threads: u32, iters: u32, r2: u32) -> u32 {
    let mut total = 0u32;
    for gid in 0..threads {
        let mut s = gid.wrapping_mul(2654435761).wrapping_add(1);
        for _ in 0..iters {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let x = s >> 20;
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let y = s >> 20;
            let d = (x * x + y * y) as i32; // matches the kernel's signed compare (values < 2^31)
            if d <= r2 as i32 {
                total += 1;
            }
        }
    }
    total
}

#[test]
fn monte_carlo_pi_deterministic_hit_count_exact() {
    let block = 128u32;
    let grid = 8u32;
    let threads = block * grid; // 1024 deterministic streams
    let iters = 32u32; // 32 768 samples total
    let r = (1u32 << 12) - 1; // 4095
    let r2 = r * r; // 16 769 025

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(MONTECARLO_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "mc_pi").unwrap();

    let d_cnt = alloc_zeroed_i32(&mut sink, &mut ctx, 1);
    let args = vec![KernelArg::Ptr(d_cnt), sc(iters as i32), sc(r2 as i32)];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (grid, 1, 1),
        (block, 1, 1),
        &args,
    )
    .unwrap();

    let got = bytes_to_u32s(&readback(&mut sink, &ctx, d_cnt, 4))[0];
    let want = mc_reference(threads, iters, r2);

    assert_eq!(
        got, want,
        "Monte-Carlo quarter-circle hit count exact vs the identical CPU LCG"
    );
    // Anti-false-pass: a non-degenerate count (roughly π/4 of the samples, but asserted EXACT above).
    let total = threads * iters;
    assert!(
        got > 0 && got < total,
        "hit count is a real interior count: 0 < hits < samples"
    );
    // Sanity band only (NOT the assertion): π/4 ≈ 0.785 of samples land inside.
    assert!(
        got > total / 2,
        "roughly π/4 of samples are hits (sanity band, not the exactness check)"
    );
}

// ==================================================================================================
// 7. kmeans_step — one Lloyd iteration. Each of N 2-D integer points is assigned to its nearest of K
//    centroids by integer squared-L2 distance (ties → lowest centroid index, from strict `<`), then the
//    per-cluster coordinate sums and counts are accumulated with cross-block atomics. One thread per
//    point over a multi-block grid. Outputs the assignment array + the (pre-division) new-centroid
//    accumulators — the exact inputs the next mean-update would divide.
// ==================================================================================================

const KMEANS_PTX: &str = r#"
    .visible .entry kmeans(
        .param .u64 p_px,
        .param .u64 p_py,
        .param .u64 p_cx,
        .param .u64 p_cy,
        .param .u64 p_assign,
        .param .u64 p_sumx,
        .param .u64 p_sumy,
        .param .u64 p_count,
        .param .u32 p_n,
        .param .u32 p_k
    )
    {
        ld.param.u64 %rpx, [p_px];
        ld.param.u64 %rpy, [p_py];
        ld.param.u64 %rcx, [p_cx];
        ld.param.u64 %rcy, [p_cy];
        ld.param.u64 %rassign, [p_assign];
        ld.param.u64 %rsumx, [p_sumx];
        ld.param.u64 %rsumy, [p_sumy];
        ld.param.u64 %rcount, [p_count];
        ld.param.u32 %rn, [p_n];
        ld.param.u32 %rk, [p_k];
        mad.lo.s32 %i, %ctaid.x, %ntid.x, %tid.x;
        setp.ge.s32 %pdone, %i, %rn;
        @%pdone bra DONE;
        cvta.to.global.u64 %gpx, %rpx;
        cvta.to.global.u64 %gpy, %rpy;
        cvta.to.global.u64 %gcx, %rcx;
        cvta.to.global.u64 %gcy, %rcy;
        mul.wide.s32 %io, %i, 4;
        add.s64 %pxp, %gpx, %io;
        ld.global.u32 %px, [%pxp];
        add.s64 %pyp, %gpy, %io;
        ld.global.u32 %py, [%pyp];
        // scan centroids for the nearest (strict < keeps the lowest index on ties)
        mov.u32 %best, 2147483647;
        mov.u32 %bestk, 0;
        mov.u32 %c, 0;
    KLOOP:
        setp.ge.s32 %pk, %c, %rk;
        @%pk bra ASSIGN;
        mul.wide.s32 %co, %c, 4;
        add.s64 %cxp, %gcx, %co;
        ld.global.u32 %cxv, [%cxp];
        add.s64 %cyp, %gcy, %co;
        ld.global.u32 %cyv, [%cyp];
        sub.s32 %dx, %px, %cxv;
        sub.s32 %dy, %py, %cyv;
        mul.lo.s32 %dxx, %dx, %dx;
        mul.lo.s32 %dyy, %dy, %dy;
        add.s32 %dist, %dxx, %dyy;
        setp.ge.s32 %pnb, %dist, %best;
        @%pnb bra KNEXT;
        mov.u32 %best, %dist;
        mov.u32 %bestk, %c;
    KNEXT:
        add.s32 %c, %c, 1;
        bra KLOOP;
    ASSIGN:
        cvta.to.global.u64 %gassign, %rassign;
        add.s64 %ap, %gassign, %io;
        st.global.u32 [%ap], %bestk;
        // atomic accumulate into cluster bestk
        mul.wide.s32 %bko, %bestk, 4;
        cvta.to.global.u64 %gsumx, %rsumx;
        add.s64 %sxp, %gsumx, %bko;
        red.global.add.u32 [%sxp], %px;
        cvta.to.global.u64 %gsumy, %rsumy;
        add.s64 %syp, %gsumy, %bko;
        red.global.add.u32 [%syp], %py;
        cvta.to.global.u64 %gcount, %rcount;
        add.s64 %ctp, %gcount, %bko;
        red.global.add.u32 [%ctp], 1;
    DONE:
        ret;
    }
"#;

#[test]
fn kmeans_step_assign_and_accumulate_exact() {
    let k = 3usize;
    // Three loose clusters of integer points around (10,10), (40,12), (25,40).
    let centers = [(10i32, 10i32), (40, 12), (25, 40)];
    let mut px = Vec::new();
    let mut py = Vec::new();
    for (ci, &(bx, by)) in centers.iter().enumerate() {
        for j in 0..10i32 {
            // deterministic scatter around each center
            let ox = ((j * 7 + ci as i32 * 3) % 11) - 5;
            let oy = ((j * 5 + ci as i32 * 2) % 9) - 4;
            px.push(bx + ox);
            py.push(by + oy);
        }
    }
    let n = px.len(); // 30
                      // Initial centroids (deliberately offset from the true centers so assignment is non-trivial).
    let cx: Vec<i32> = vec![8, 42, 27];
    let cy: Vec<i32> = vec![12, 10, 38];

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(KMEANS_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "kmeans").unwrap();

    let d_px = upload(&mut sink, &mut ctx, &i32s_to_bytes(&px));
    let d_py = upload(&mut sink, &mut ctx, &i32s_to_bytes(&py));
    let d_cx = upload(&mut sink, &mut ctx, &i32s_to_bytes(&cx));
    let d_cy = upload(&mut sink, &mut ctx, &i32s_to_bytes(&cy));
    let d_assign = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_sumx = alloc_zeroed_i32(&mut sink, &mut ctx, k);
    let d_sumy = alloc_zeroed_i32(&mut sink, &mut ctx, k);
    let d_count = alloc_zeroed_i32(&mut sink, &mut ctx, k);

    let block = 16u32;
    let grid = (n as u32 + block - 1) / block; // 2 blocks
    let args = vec![
        KernelArg::Ptr(d_px),
        KernelArg::Ptr(d_py),
        KernelArg::Ptr(d_cx),
        KernelArg::Ptr(d_cy),
        KernelArg::Ptr(d_assign),
        KernelArg::Ptr(d_sumx),
        KernelArg::Ptr(d_sumy),
        KernelArg::Ptr(d_count),
        sc(n as i32),
        sc(k as i32),
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

    let got_assign = bytes_to_i32s(&readback(&mut sink, &ctx, d_assign, n * 4));
    let got_sumx = bytes_to_i32s(&readback(&mut sink, &ctx, d_sumx, k * 4));
    let got_sumy = bytes_to_i32s(&readback(&mut sink, &ctx, d_sumy, k * 4));
    let got_count = bytes_to_i32s(&readback(&mut sink, &ctx, d_count, k * 4));

    // CPU reference: nearest-centroid assignment (lowest index on ties) + per-cluster accumulation.
    let mut want_assign = vec![0i32; n];
    let mut want_sumx = vec![0i32; k];
    let mut want_sumy = vec![0i32; k];
    let mut want_count = vec![0i32; k];
    for i in 0..n {
        let mut best = i32::MAX;
        let mut bestk = 0usize;
        for c in 0..k {
            let dx = px[i] - cx[c];
            let dy = py[i] - cy[c];
            let dist = dx * dx + dy * dy;
            if dist < best {
                best = dist;
                bestk = c;
            }
        }
        want_assign[i] = bestk as i32;
        want_sumx[bestk] += px[i];
        want_sumy[bestk] += py[i];
        want_count[bestk] += 1;
    }
    assert_eq!(
        got_assign, want_assign,
        "k-means point→cluster assignment exact"
    );
    assert_eq!(
        got_sumx, want_sumx,
        "per-cluster x-sum exact (cross-block atomic)"
    );
    assert_eq!(
        got_sumy, want_sumy,
        "per-cluster y-sum exact (cross-block atomic)"
    );
    assert_eq!(got_count, want_count, "per-cluster count exact");
    // Anti-false-pass: every point counted once, and the clustering is genuinely spread (no empty/monopoly).
    assert_eq!(
        got_count.iter().sum::<i32>(),
        n as i32,
        "every point assigned exactly once"
    );
    assert!(
        got_count.iter().all(|&c| c > 0),
        "no empty cluster — assignment is non-degenerate"
    );
}

// ==================================================================================================
// 8. running_minmax — inclusive Hillis-Steele running MIN and running MAX in one pass, computed
//    simultaneously in two `.shared` arrays with a `bar.sync` per doubling step (a read barrier + a write
//    barrier, so no lane reads a slot mid-write). blockDim == N so every lane is live and hits every
//    barrier identically. Exact prefix-min and prefix-max vs the CPU running reductions.
// ==================================================================================================

const RUNNING_MINMAX_PTX: &str = r#"
    .visible .entry running_minmax(
        .param .u64 p_in,
        .param .u64 p_omin,
        .param .u64 p_omax
    )
    {
        .shared .align 4 .b32 smin[256];
        .shared .align 4 .b32 smax[256];
        ld.param.u64 %rin, [p_in];
        ld.param.u64 %romin, [p_omin];
        ld.param.u64 %romax, [p_omax];
        mov.u32 %tid, %tid.x;
        mov.u32 %bd, %ntid.x;
        mul.lo.s32 %toff, %tid, 4;
        mov.u32 %sminb, smin;
        mov.u32 %smaxb, smax;
        add.s32 %minaddr, %sminb, %toff;
        add.s32 %maxaddr, %smaxb, %toff;
        // load in[tid] into both shared arrays
        cvta.to.global.u64 %gin, %rin;
        mul.wide.s32 %io, %tid, 4;
        add.s64 %ip, %gin, %io;
        ld.global.u32 %v, [%ip];
        st.shared.u32 [%minaddr], %v;
        st.shared.u32 [%maxaddr], %v;
        bar.sync;
        mov.u32 %d, 1;
    DLOOP:
        setp.ge.s32 %pd, %d, %bd;
        @%pd bra ENDD;
        // current values
        ld.shared.u32 %curmin, [%minaddr];
        ld.shared.u32 %curmax, [%maxaddr];
        // if tid >= d, combine with the value d slots back
        setp.lt.s32 %plt, %tid, %d;
        @%plt bra HAVE;
        sub.s32 %jidx, %tid, %d;
        mul.lo.s32 %joff, %jidx, 4;
        add.s32 %jminaddr, %sminb, %joff;
        add.s32 %jmaxaddr, %smaxb, %joff;
        ld.shared.u32 %pmin, [%jminaddr];
        ld.shared.u32 %pmax, [%jmaxaddr];
        // curmin = min(curmin, pmin)
        setp.le.s32 %pkeepmin, %curmin, %pmin;
        @%pkeepmin bra MAXCMP;
        mov.u32 %curmin, %pmin;
    MAXCMP:
        // curmax = max(curmax, pmax)
        setp.ge.s32 %pkeepmax, %curmax, %pmax;
        @%pkeepmax bra HAVE;
        mov.u32 %curmax, %pmax;
    HAVE:
        bar.sync;
        st.shared.u32 [%minaddr], %curmin;
        st.shared.u32 [%maxaddr], %curmax;
        bar.sync;
        shl.b32 %d, %d, 1;
        bra DLOOP;
    ENDD:
        ld.shared.u32 %rmin, [%minaddr];
        ld.shared.u32 %rmax, [%maxaddr];
        cvta.to.global.u64 %gomin, %romin;
        cvta.to.global.u64 %gomax, %romax;
        add.s64 %ominp, %gomin, %io;
        st.global.u32 [%ominp], %rmin;
        add.s64 %omaxp, %gomax, %io;
        st.global.u32 [%omaxp], %rmax;
        ret;
    }
"#;

#[test]
fn running_minmax_prefix_scan_exact() {
    let n = 200usize; // single block, blockDim == N; NOT a power of two (Hillis-Steele still exact)
                      // Signed values that wander up and down so the running min/max genuinely change over the sequence.
                      // Index 0 is deliberately a moderate value; the deep dip / high spike land later, so the running
                      // min strictly decreases and the running max strictly increases past the start (anti-false-pass).
    let input: Vec<i32> = (0..n)
        .map(|i| {
            let t = i as i32;
            let base = ((t * 37 + 50) % 191) - 95; // in [-95, 95], value 0 at t=0 is ~-45
            let dip = if t % 13 == 6 { -40 } else { 0 };
            let spike = if t % 17 == 3 { 40 } else { 0 };
            base + dip + spike
        })
        .collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = ctx.load_module(RUNNING_MINMAX_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "running_minmax").unwrap();

    let d_in = upload(&mut sink, &mut ctx, &i32s_to_bytes(&input));
    let d_omin = alloc_zeroed_i32(&mut sink, &mut ctx, n);
    let d_omax = alloc_zeroed_i32(&mut sink, &mut ctx, n);

    let args = vec![
        KernelArg::Ptr(d_in),
        KernelArg::Ptr(d_omin),
        KernelArg::Ptr(d_omax),
    ];
    launch::launch(
        &mut ctx,
        &mut sink,
        func,
        (1, 1, 1),
        (n as u32, 1, 1),
        &args,
    )
    .unwrap();

    let got_min = bytes_to_i32s(&readback(&mut sink, &ctx, d_omin, n * 4));
    let got_max = bytes_to_i32s(&readback(&mut sink, &ctx, d_omax, n * 4));

    // CPU reference: inclusive running min / max.
    let mut want_min = vec![0i32; n];
    let mut want_max = vec![0i32; n];
    let mut cmin = i32::MAX;
    let mut cmax = i32::MIN;
    for i in 0..n {
        cmin = cmin.min(input[i]);
        cmax = cmax.max(input[i]);
        want_min[i] = cmin;
        want_max[i] = cmax;
    }
    assert_eq!(got_min, want_min, "inclusive running minimum exact");
    assert_eq!(got_max, want_max, "inclusive running maximum exact");
    // Anti-false-pass: the scans are monotone and actually move (not a constant array).
    assert!(
        want_min.windows(2).all(|w| w[1] <= w[0]),
        "running min is non-increasing"
    );
    assert!(
        want_max.windows(2).all(|w| w[1] >= w[0]),
        "running max is non-decreasing"
    );
    assert!(
        want_min[n - 1] < want_min[0],
        "running min genuinely decreased over the sequence"
    );
    assert!(
        want_max[n - 1] > want_max[0],
        "running max genuinely increased over the sequence"
    );
    assert_eq!(
        want_min[n - 1],
        *input.iter().min().unwrap(),
        "final running min = global min"
    );
    assert_eq!(
        want_max[n - 1],
        *input.iter().max().unwrap(),
        "final running max = global max"
    );
}
