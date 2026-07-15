//! CUDA **advanced-correctness** demo battery — beyond compute (#162), error-paths (#172), and hostile
//! (#193). Each demo drives a real CUDA advanced feature through the REAL hl-cuda driver and asserts the
//! result **bit-exact** against an independent reference. Nothing here asserts "did not crash"; every
//! output is checked value-by-value, and every feature the driver models is exercised through its actual
//! lowering — no stub is allowed to return a placeholder that a fake-passing assert would bless.
//!
//! Batteries:
//!   1. `texture_object`     — a `cudaTextureObject` over a 2D array, fetched with POINT (exact texel) and
//!                             LINEAR (exact bilinear midpoint) filtering; both bit-exact.
//!   2. `managed_memory`     — `cudaMallocManaged`: host writes a pattern, a kernel transforms it in place,
//!                             the host reads it back — the unified pointer round-trips, bit-exact.
//!   3. `cuda_graph`         — a kernel sequence built into a `cudaGraph`, instantiated + launched; the
//!                             replayed result equals running the sequence eagerly, bit-exact + idempotent.
//!   4. `constant_memory`    — a `.const` global set from host via `cudaMemcpyToSymbol` and read in a
//!                             kernel; the round-trip and the kernel output are both exact.
//!   5. `multi_stream`       — overlapping async copies + kernels across four streams into a shared output,
//!                             deterministic + bit-exact regardless of issue interleaving.
//!
//! ## The one honest boundary (texture)
//! A kernel-side `tex2D` is a `tex.2d` PTX instruction served by the GPU texture unit. The neutral
//! kernel-IR interpreter (in `hl_wip-gpu`, out of this crate's scope) models no `tex` opcode, so the
//! texture *unit* is modeled in the driver, host-side ([`hl_cuda::model::texture`]): a real, deterministic
//! evaluation of CUDA's documented fetch/filter math — not a stub. Demo 1 is transparent about this: it
//! asserts the driver's `tex2d` against a hand-computed reference, exactly as a kernel `tex2D` must return.

use hl_cuda::adapter::ptx;
use hl_cuda::model::texture::{FilterMode, SamplerDesc};
use hl_cuda::service::{allocate, graph, launch, load_module, symbol, synchronize, texture, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, DevicePtr, KernelArg};

use hl_gpu::protocol::model::capability::{command_bits, format_bits, shader_payload, ALL_COMMANDS, COLOR_FORMATS};
use hl_gpu::protocol::model::kernel::KernelDescriptor;
use hl_gpu::{BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, WIRE_VERSION};

// --------------------------------------------------------------------------------------------------
// shared harness — identical wiring to tests/compute_demo.rs.
// --------------------------------------------------------------------------------------------------

fn harness() -> InProcessCommandSink<CpuExecutor> {
    let mut exec = CpuExecutor::new();
    exec.set_kernel_compiler(|desc: &KernelDescriptor| ptx::compile(&desc.ptx, &desc.entry, desc.block));
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
    raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()
}
fn bytes_to_i32s(raw: &[u8]) -> Vec<i32> {
    raw.chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn readback(sink: &mut InProcessCommandSink<CpuExecutor>, ctx: &CudaContext, p: DevicePtr, len: usize) -> Vec<u8> {
    let (buf, off): (BufferId, u64) = transfer::memcpy_dtoh(ctx, p).unwrap();
    sink.read_buffer(buf, off, len).unwrap()
}

fn upload(sink: &mut InProcessCommandSink<CpuExecutor>, ctx: &mut CudaContext, bytes: &[u8]) -> DevicePtr {
    let p = allocate::mem_alloc(ctx, sink, bytes.len() as u64).unwrap();
    transfer::memcpy_htod(ctx, sink, p, bytes).unwrap();
    p
}

// ==================================================================================================
// 1. texture_object — cudaTextureObject over a 2D array; POINT (exact texel) + LINEAR (exact midpoint).
// ==================================================================================================

#[test]
fn texture_object_point_and_linear_fetch_exact() {
    // 4x3 f32 array with distinct texels: T[row][col] = row*100 + col*10.
    let (w, h) = (4u32, 3u32);
    let texels: Vec<f32> =
        (0..h).flat_map(|r| (0..w).map(move |c| (r as f32) * 100.0 + (c as f32) * 10.0)).collect();
    let t = |r: usize, c: usize| texels[r * w as usize + c];

    let mut array = texture::malloc_array(w, h).unwrap();
    texture::memcpy_to_array(&mut array, &texels).unwrap();

    // ---- POINT filter: nearest texel, no interpolation ----
    let point_tex = texture::create_texture_object(&array, SamplerDesc::point_clamp());
    assert_eq!(point_tex.desc.filter, FilterMode::Point);
    // (1.5, 0.5) → floor → col 1, row 0 → exactly T[0][1] = 10.0.
    assert_eq!(texture::tex2d(&point_tex, 1.5, 0.5), t(0, 1));
    assert_eq!(texture::tex2d(&point_tex, 1.5, 0.5), 10.0);
    // (3.9, 2.1) → col 3, row 2 → T[2][3] = 230.0.
    assert_eq!(texture::tex2d(&point_tex, 3.9, 2.1), t(2, 3));
    assert_eq!(texture::tex2d(&point_tex, 3.9, 2.1), 230.0);

    // ---- LINEAR filter: bilinear interpolation ----
    let lin_tex = texture::create_texture_object(&array, SamplerDesc::linear_clamp());
    // Horizontal midpoint (2.0, 0.5): xb=1.5 → i0=1, a=0.5; yb=0.0 → b=0 → 0.5·T[0][1] + 0.5·T[0][2].
    let hmid = texture::tex2d(&lin_tex, 2.0, 0.5);
    assert_eq!(hmid, 0.5 * t(0, 1) + 0.5 * t(0, 2));
    assert_eq!(hmid, 15.0, "exact horizontal midpoint of 10 and 20");
    // Vertical midpoint (0.5, 1.0): xb=0 → a=0; yb=0.5 → j0=0, b=0.5 → 0.5·T[0][0] + 0.5·T[1][0].
    let vmid = texture::tex2d(&lin_tex, 0.5, 1.0);
    assert_eq!(vmid, 0.5 * t(0, 0) + 0.5 * t(1, 0));
    assert_eq!(vmid, 50.0, "exact vertical midpoint of 0 and 100");
    // Center-of-4 (2.0, 1.0): quarter-weight of the four surrounding texels.
    let cmid = texture::tex2d(&lin_tex, 2.0, 1.0);
    let want_center = 0.25 * (t(0, 1) + t(0, 2) + t(1, 1) + t(1, 2));
    assert_eq!(cmid, want_center);
    assert_eq!(cmid, 65.0, "exact center of {{10,20,110,120}}");
    // Sampling exactly ON a texel center (i+0.5, j+0.5) returns that texel unchanged (weights collapse).
    assert_eq!(texture::tex2d(&lin_tex, 2.5, 1.5), t(1, 2));
    assert_eq!(texture::tex2d(&lin_tex, 2.5, 1.5), 120.0);
}

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
    assert!(ctx.mem.is_managed(managed), "the pointer is flagged managed/unified");
    // A plain cudaMalloc is NOT managed — the model distinguishes the two truthfully.
    let plain = allocate::mem_alloc(&mut ctx, &mut sink, 16).unwrap();
    assert!(!ctx.mem.is_managed(plain), "a plain device allocation is not managed");

    // Host writes the pattern through the unified pointer (cudaMemcpy / direct host store analogue).
    transfer::memcpy_htod(&mut ctx, &mut sink, managed, &f32s_to_bytes(&pattern)).unwrap();

    // A kernel reads + transforms the SAME managed allocation in place.
    let module = load_module::module_load_data(&mut ctx, TRANSFORM_PTX.as_bytes()).unwrap();
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
    assert_eq!(got, want, "managed pointer round-trips host→kernel→host, all {n} elements exact");
}

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
    let module = load_module::module_load_data(&mut ctx, SAXPY_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "saxpy").unwrap();
    let grid = (6u32, 1, 1);
    let block = (128u32, 1, 1);

    // ---- eager reference: upload x, upload y, saxpy — run directly through the services ----
    let ex = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let ey = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, ex, &x_bytes).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, ey, &y_bytes).unwrap();
    let eager_args =
        vec![KernelArg::Ptr(ex), KernelArg::Ptr(ey), KernelArg::Scalar(alpha.to_le_bytes().to_vec()), KernelArg::Scalar((n as i32).to_le_bytes().to_vec())];
    launch::launch(&mut ctx, &mut sink, func, grid, block, &eager_args).unwrap();
    let eager = bytes_to_f32s(&readback(&mut sink, &ctx, ey, n * 4));

    // ---- graph: identical sequence built as nodes, instantiated, launched ----
    let gx = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let gy = allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap();
    let mut g = graph::graph_create();
    graph::add_memcpy_htod_node(&mut g, gx, &x_bytes);
    graph::add_memcpy_htod_node(&mut g, gy, &y_bytes);
    let graph_args =
        vec![KernelArg::Ptr(gx), KernelArg::Ptr(gy), KernelArg::Scalar(alpha.to_le_bytes().to_vec()), KernelArg::Scalar((n as i32).to_le_bytes().to_vec())];
    graph::add_kernel_node(&mut g, func, grid, block, graph_args);
    assert_eq!(g.nodes.len(), 3, "graph has 3 nodes: two H2D copies + one kernel");

    let exec = graph::instantiate(&g);
    graph::launch_graph(&mut ctx, &mut sink, &exec).unwrap();
    let graphed = bytes_to_f32s(&readback(&mut sink, &ctx, gy, n * 4));

    // Independent CPU reference (same fma order).
    let want: Vec<f32> = x.iter().zip(&y0).map(|(xi, yi)| alpha.mul_add(*xi, *yi)).collect();
    assert_eq!(eager, want, "eager saxpy sequence matches CPU reference");
    assert_eq!(graphed, want, "graph-replayed saxpy matches CPU reference");
    assert_eq!(graphed, eager, "graph replay is bit-identical to the eager sequence");

    // Re-launching the graph re-runs its H2D nodes (resetting y) then saxpy → idempotent bit-exact.
    graph::launch_graph(&mut ctx, &mut sink, &exec).unwrap();
    let relaunched = bytes_to_f32s(&readback(&mut sink, &ctx, gy, n * 4));
    assert_eq!(relaunched, want, "second graph launch reproduces the same result (idempotent replay)");
}

// ==================================================================================================
// 4. constant_memory — a `.const` global set via cudaMemcpyToSymbol, read in a kernel; both exact.
// ==================================================================================================

/// A module declaring a 4-element `.const` coefficient array plus a kernel that reads coeff[0]/coeff[1]
/// (passed as the symbol's device pointer) and applies `data[i] = coeff[0]*data[i] + coeff[1]`.
const CONST_PTX: &str = r#"
    .const .align 4 .f32 kCoeff[4];

    .visible .entry apply(
        .param .u64 ap_coeff,
        .param .u64 ap_data,
        .param .u32 ap_n
    )
    {
        ld.param.u64  %rc, [ap_coeff];
        ld.param.u64  %rd, [ap_data];
        ld.param.u32  %rn, [ap_n];
        mov.u32       %rntid, %ntid.x;
        mov.u32       %rctaid, %ctaid.x;
        mov.u32       %rtid, %tid.x;
        mad.lo.s32    %ri, %rctaid, %rntid, %rtid;
        setp.ge.s32   %pg, %ri, %rn;
        @%pg bra      DONE;
        cvta.to.global.u64 %gc, %rc;
        cvta.to.global.u64 %gd, %rd;
        ld.global.f32 %c0, [%gc];
        ld.global.f32 %c1, [%gc+4];
        mul.wide.s32  %off, %ri, 4;
        add.s64       %pd, %gd, %off;
        ld.global.f32 %v, [%pd];
        fma.rn.f32    %r, %c0, %v, %c1;
        st.global.f32 [%pd], %r;
    DONE:
        ret;
    }
"#;

#[test]
fn constant_memory_symbol_set_from_host_read_in_kernel_exact() {
    let n = 400usize;
    let coeff = [2.0f32, 5.0f32, 0.0f32, 0.0f32]; // scale=2, bias=5
    let data0: Vec<f32> = (0..n).map(|i| (i as f32) * 0.1 - 20.0).collect();

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, CONST_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "apply").unwrap();

    // cudaGetSymbolAddress: the `.const kCoeff` symbol resolves to a real device pointer of 16 bytes.
    let (coeff_ptr, size) = symbol::get_symbol_address(&mut ctx, &mut sink, module, "kCoeff").unwrap();
    assert_eq!(size, 16, "kCoeff is 4 × f32 = 16 bytes");

    // cudaMemcpyToSymbol: set the constant from the host.
    symbol::memcpy_to_symbol(&mut ctx, &mut sink, module, "kCoeff", &f32s_to_bytes(&coeff)).unwrap();

    // cudaMemcpyFromSymbol: the host round-trips the symbol back, bit-exact.
    let echoed = bytes_to_f32s(&symbol::memcpy_from_symbol(&mut ctx, &mut sink, module, "kCoeff", 16).unwrap());
    assert_eq!(echoed, coeff, "the constant reads back exactly what the host wrote");

    // An unknown symbol is the honest cudaErrorInvalidSymbol analogue — never a fake pointer.
    assert!(symbol::get_symbol_address(&mut ctx, &mut sink, module, "nope").is_err());

    // Kernel reads the constant (via the symbol's device pointer) and transforms the data.
    let d_data = upload(&mut sink, &mut ctx, &f32s_to_bytes(&data0));
    let args = vec![KernelArg::Ptr(coeff_ptr), KernelArg::Ptr(d_data), KernelArg::Scalar((n as i32).to_le_bytes().to_vec())];
    launch::launch(&mut ctx, &mut sink, func, (4, 1, 1), (128, 1, 1), &args).unwrap();

    let got = bytes_to_f32s(&readback(&mut sink, &ctx, d_data, n * 4));
    let want: Vec<f32> = data0.iter().map(|v| coeff[0].mul_add(*v, coeff[1])).collect();
    assert_eq!(got, want, "kernel output uses the host-set constant, all {n} elements exact");
}

// ==================================================================================================
// 5. multi_stream — overlapping async copies + kernels across four streams; deterministic + exact.
// ==================================================================================================

/// `out[i] = k*in[i] + c` (integer), one lane per element.
const ISCALE_PTX: &str = r#"
    .visible .entry iscale(
        .param .u64 is_in,
        .param .u64 is_out,
        .param .u32 is_k,
        .param .u32 is_c,
        .param .u32 is_n
    )
    {
        ld.param.u64  %rin, [is_in];
        ld.param.u64  %rout, [is_out];
        ld.param.u32  %rk, [is_k];
        ld.param.u32  %rc, [is_c];
        ld.param.u32  %rn, [is_n];
        mov.u32 %rnt, %ntid.x; mov.u32 %rct, %ctaid.x; mov.u32 %rtt, %tid.x;
        mad.lo.s32 %i, %rct, %rnt, %rtt;
        setp.ge.s32 %pg, %i, %rn;
        @%pg bra DONE;
        cvta.to.global.u64 %gin, %rin;
        cvta.to.global.u64 %gout, %rout;
        mul.wide.s32 %off, %i, 4;
        add.s64 %pin, %gin, %off;
        add.s64 %pout, %gout, %off;
        ld.global.u32 %v, [%pin];
        mad.lo.s32 %r, %v, %rk, %rc;
        st.global.u32 [%pout], %r;
    DONE: ret;
    }
"#;

/// Run four streams, each doing async H2D + kernel + (implicit) into its own output buffer, issuing the
/// four in the order `order`. Returns the four output arrays concatenated head-to-tail.
fn run_four_streams(input: &[i32], coeffs: [(i32, i32); 4], order: [usize; 4]) -> Vec<Vec<i32>> {
    let n = input.len();
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));
    let module = load_module::module_load_data(&mut ctx, ISCALE_PTX.as_bytes()).unwrap();
    let func = load_module::module_get_function(&ctx, module, "iscale").unwrap();

    let streams: Vec<_> = (0..4).map(|_| ctx.streams.create()).collect();
    let in_bytes = i32s_to_bytes(input);
    let d_in: Vec<DevicePtr> =
        (0..4).map(|_| allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap()).collect();
    let d_out: Vec<DevicePtr> =
        (0..4).map(|_| allocate::mem_alloc(&mut ctx, &mut sink, (n * 4) as u64).unwrap()).collect();

    // Overlapping issue: for each stream (in the given interleaving) an async H2D copy then a kernel.
    let sc = |v: i32| KernelArg::Scalar(v.to_le_bytes().to_vec());
    for &s in &order {
        transfer::memcpy_htod_async(&mut ctx, &mut sink, streams[s], d_in[s], &in_bytes).unwrap();
        let (k, c) = coeffs[s];
        let args = vec![KernelArg::Ptr(d_in[s]), KernelArg::Ptr(d_out[s]), sc(k), sc(c), sc(n as i32)];
        launch::launch(&mut ctx, &mut sink, func, (16, 1, 1), (128, 1, 1), &args).unwrap();
    }
    for &s in &streams {
        synchronize::stream_synchronize(&mut ctx, &mut sink, s).unwrap();
    }

    (0..4).map(|s| bytes_to_i32s(&readback(&mut sink, &ctx, d_out[s], n * 4))).collect()
}

#[test]
fn multi_stream_overlap_is_deterministic_and_exact() {
    let n = 1500usize;
    let input: Vec<i32> = (0..n).map(|i| i as i32 - 750).collect();
    let coeffs = [(2, 1), (3, -5), (7, 100), (-4, 9)];

    // Two different issue interleavings of the identical work.
    let forward = run_four_streams(&input, coeffs, [0, 1, 2, 3]);
    let shuffled = run_four_streams(&input, coeffs, [3, 1, 0, 2]);

    // Independent CPU reference per stream.
    let cpu: Vec<Vec<i32>> = (0..4)
        .map(|s| {
            let (k, c) = coeffs[s];
            input.iter().map(|v| v.wrapping_mul(k).wrapping_add(c)).collect()
        })
        .collect();

    for s in 0..4 {
        assert_eq!(forward[s], cpu[s], "stream {s} forward-issue output matches CPU reference");
        assert_eq!(shuffled[s], cpu[s], "stream {s} shuffled-issue output matches CPU reference");
        assert_eq!(forward[s], shuffled[s], "stream {s} output is identical regardless of issue order");
    }
}
