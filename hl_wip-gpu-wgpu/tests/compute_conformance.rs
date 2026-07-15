//! Systematic COMPUTE-path conformance battery for the `WgpuExecutor`.
//!
//! Where `spirv_compute.rs` proves a SPIR-V compute pipeline can be created + dispatched at all, this file
//! stresses the *substance* of the compute path against a bit-exact CPU reference: workgroup-size
//! independence, workgroup-local shared memory + barriers, atomic serialization, storage read-modify-write
//! across a multi-workgroup dispatch, and cross-pass data dependency. Every case mints REAL SPIR-V from a
//! WGSL seed (naga `wgsl-in → spv-out`, the exact round trip the guest's SPIR-V ABI relies on); the
//! executor translates it straight back (`spv-in → wgsl-out`) and builds a real compute pipeline with an
//! AUTO bind-group layout, so `var<workgroup>`, `atomic<u32>`, and `workgroupBarrier()` genuinely execute
//! on the device (headless software Vulkan / lavapipe) — none of them are modeled host-side.
//!
//! Each assertion is EXACT (tol 0): a compute kernel's integer output is not subject to the last-ULP
//! interpolation/rounding slack the raster differential tolerates, so any divergence from the race-free CPU
//! answer is a real executor bug (a mis-derived workgroup dispatch, shared memory that is not zeroed or not
//! isolated per workgroup, an atomic that loses an update, or a barrier that fails to synchronize).
//!
//! If no wgpu adapter is reachable (no lavapipe / Vulkan ICD) the shared `exec()` panics on first use like
//! the rest of the suite — these cases are conformance, not capability probes.

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ComputePipelineDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// -------------------------------------------------------------------------------------------------
// shared device + runtime-pipeline harness (mirrors tests/spirv_compute.rs)
// -------------------------------------------------------------------------------------------------

static EXEC: OnceLock<Mutex<WgpuExecutor>> = OnceLock::new();

fn exec() -> MutexGuard<'static, WgpuExecutor> {
    EXEC.get_or_init(|| {
        Mutex::new(
            WgpuExecutor::new(DeviceConfig::default())
                .expect("acquire a wgpu adapter (is a Vulkan ICD / lavapipe reachable?)"),
        )
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

/// Fresh session with byte-addressable copies (`copy_alignment = 1`) so any buffer size/offset works.
fn session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)))
}

/// Submit `cmds` as one batch through validate → account → dispatch → execute, returning the `Session` so
/// its `resources` can be read back.
fn run_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> Session {
    let mut s = session(exec);
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds).expect("compute program must run cleanly");
    s
}

/// STORAGE + copy-both: every conformance buffer is bound as storage AND read back / seeded from the host.
fn sbuf(size: u64) -> BufferDesc {
    BufferDesc {
        size,
        usage: buffer_usage::STORAGE | buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    }
}

fn u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn read_u32s(g: &WgpuExecutor, s: &Session, id: u32, n: usize) -> Vec<u32> {
    let out = g.read_buffer(&s.resources, hl_gpu::BufferId(id), 0, n * 4).unwrap();
    out.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

/// Mint real SPIR-V from a WGSL seed (naga `wgsl-in → spv-out`), with ALL validation capabilities so
/// `var<workgroup>` / `atomic` / barriers pass the seed validator; the executor lowers it straight back.
fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("seed wgsl parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("seed wgsl validates: {e:?}\n---\n{src}"));
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit spir-v")
}

/// A buffer to create + seed before the dispatch.
struct Buf {
    id: u32,
    init: Vec<u8>,
}

/// Run ONE compute pipeline: create the SPIR-V shader (id 1) + pipeline (id 1), create + seed every buffer,
/// create bind group 1 (set 0) with `entries`, then a single `Dispatch(dispatch)` in one compute pass.
fn run_one(
    g: &mut WgpuExecutor,
    src: &str,
    bufs: &[Buf],
    entries: Vec<BindEntry>,
    dispatch: (u32, u32, u32),
) -> Session {
    let spirv = wgsl_to_spirv(src);
    let mut cmds = vec![
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv },
        Cmd::CreateComputePipeline(
            1,
            ComputePipelineDesc {
                compute: ShaderRef { module: 1, entry: "cs_main".into() },
                label: String::new(),
            },
        ),
    ];
    for b in bufs {
        cmds.push(Cmd::CreateBuffer(b.id, sbuf(b.init.len() as u64)));
        cmds.push(Cmd::WriteBuffer { id: b.id, offset: 0, data: b.init.clone() });
    }
    cmds.push(Cmd::CreateBindGroup(1, BindGroupDesc { set: 0, entries }));
    let (x, y, z) = dispatch;
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(1),
            Enc::SetBindGroup { index: 0, group: 1 },
            Enc::Dispatch { x, y, z },
            Enc::EndComputePass,
        ],
        signal: None,
    }));
    run_batch(g, &cmds)
}

/// A whole-buffer binding of storage buffer `id` at `binding`.
fn whole(binding: u32, id: u32, size: u64) -> BindEntry {
    BindEntry { binding, resource: BindResource::Buffer { id, offset: 0, size } }
}

// =================================================================================================
// 1. WORKGROUP SIZES — one elementwise kernel, many @workgroup_size configs, one correct output
// =================================================================================================

/// The SAME elementwise map `dst[i] = src[i]*3 + 7` run under FIVE workgroup configurations — 1, 64, 256, a
/// non-power-of-2 (96), a 2D `(8,8)`, and a 3D `(4,4,4)` block — each over the identical `N`-element input,
/// each DELIBERATELY over-dispatched so the grid covers `[0, N)` WITH a remainder of out-of-range
/// invocations. Every config must (a) produce the bit-exact CPU output on `[0, N)` and (b) leave the padded
/// tail `[N, N+PAD)` at its sentinel — proving the guarded out-of-range invocations write NOTHING (a missing
/// `i < N` guard would corrupt the sentinel, since the over-dispatch total stays inside the padded buffer).
/// Workgroup-size independence is the core contract: the dispatch dimensioning is derived per config, but
/// the linear element index each invocation computes is a bijection onto the covered grid, so all five agree.
#[test]
fn workgroup_sizes_all_agree_and_respect_bounds() {
    const N: u32 = 1000;
    const PAD: u32 = 64; // >= the largest per-config remainder (96-block leaves 56)
    const SENTINEL: u32 = 0xDEAD_BEEF;

    // Deterministic input; wrapping CPU reference for the in-range slots, sentinel for the padded tail.
    let src: Vec<u32> = (0..N).map(|i| i.wrapping_mul(7).wrapping_add(3)).collect();
    let mut expect: Vec<u32> = src.iter().map(|v| v.wrapping_mul(3).wrapping_add(7)).collect();
    expect.extend(std::iter::repeat(SENTINEL).take(PAD as usize));
    let out_len = (N + PAD) as usize;
    let dst_init = u32s(&vec![SENTINEL; out_len]);

    // (label, WGSL body computing the linear index `i`, dispatch groups). Each grid is a bijection onto
    // `[0, total)` with `N <= total <= N+PAD`, so exactly the remainder `[N, total)` is out-of-range.
    let head = "\
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
const N: u32 = 1000u;
";
    let configs: [(&str, String, (u32, u32, u32)); 6] = [
        // 1D, workgroup_size(1): over-dispatch to 1024 groups → 24 out-of-range invocations.
        ("ws1", format!("{head}
@compute @workgroup_size(1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"), (1024, 1, 1)),
        // 1D, workgroup_size(64): 16 groups → total 1024, remainder 24.
        ("ws64", format!("{head}
@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"), (16, 1, 1)),
        // 1D, workgroup_size(256): 4 groups → total 1024, remainder 24.
        ("ws256", format!("{head}
@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"), (4, 1, 1)),
        // 1D, NON-power-of-2 workgroup_size(96): 11 groups → total 1056, remainder 56.
        ("ws96", format!("{head}
@compute @workgroup_size(96)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"), (11, 1, 1)),
        // 2D block (8,8), dispatch (4,4) → 32x32 grid, row-major linear index, total 1024, remainder 24.
        ("ws2d_8x8", format!("{head}
@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = gid.y * 32u + gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"), (4, 4, 1)),
        // 3D block (4,4,4), dispatch (2,4,2) → 8x16x8 grid, z-major linear index, total 1024, remainder 24.
        ("ws3d_4x4x4", format!("{head}
@compute @workgroup_size(4, 4, 4)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    let i = (gid.z * 16u + gid.y) * 8u + gid.x;
    if (i < N) {{ dst[i] = src[i] * 3u + 7u; }}
}}"), (2, 4, 2)),
    ];

    let mut g = exec();
    for (label, src_wgsl, dispatch) in &configs {
        let s = run_one(
            &mut g,
            src_wgsl,
            &[Buf { id: 1, init: u32s(&src) }, Buf { id: 2, init: dst_init.clone() }],
            vec![
                whole(0, 1, (N * 4) as u64),
                whole(1, 2, (out_len * 4) as u64),
            ],
            *dispatch,
        );
        let got = read_u32s(&g, &s, 2, out_len);
        assert_eq!(
            got, expect,
            "workgroup config {label}: elementwise map must be bit-exact on [0,N) AND leave the padded tail \
             at the sentinel (guarded out-of-range invocations wrote nothing)"
        );
    }
}

// =================================================================================================
// 2. SHARED MEMORY + BARRIERS — a workgroup-local tree reduction, one result per workgroup
// =================================================================================================

/// A workgroup-local reduction (`var<workgroup>` scratch + `workgroupBarrier()` between tree-halving steps),
/// one result per workgroup, asserted bit-exact against a per-workgroup CPU reduction. The barrier is
/// load-bearing: without it a thread would read a neighbour's scratch slot before that neighbour's write
/// landed, and the reduced value would diverge — so an exact match across every workgroup proves the barrier
/// synchronizes the workgroup and that each workgroup's shared memory is isolated (a bleed from another
/// workgroup's scratch would corrupt the sum/max). Run for BOTH `+` (sum) and `max`.
#[test]
fn shared_memory_reduction_sum_and_max() {
    const WG: u32 = 64;
    const NUM_WG: u32 = 17; // not a power of two — the per-workgroup result vector is 17 wide
    let n = (WG * NUM_WG) as usize; // exact multiple → no guard needed, every thread has an element
    let input: Vec<u32> = (0..n as u32).map(|i| i.wrapping_mul(31).wrapping_add(1) & 0xFFFF).collect();

    // (label, WGSL combine op, WGSL identity, CPU fold).
    #[allow(clippy::type_complexity)]
    let variants: [(&str, &str, &str, fn(u32, u32) -> u32); 2] = [
        ("sum", "acc + scratch[lid.x + stride]", "0u", |a, b| a.wrapping_add(b)),
        ("max", "max(acc, scratch[lid.x + stride])", "0u", |a, b| a.max(b)),
    ];

    let mut g = exec();
    for (label, combine, _identity, fold) in &variants {
        let src = format!("\
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
var<workgroup> scratch: array<u32, 64>;
@compute @workgroup_size(64)
fn cs_main(@builtin(local_invocation_id) lid: vec3<u32>,
           @builtin(workgroup_id) wid: vec3<u32>,
           @builtin(global_invocation_id) gid: vec3<u32>) {{
    scratch[lid.x] = input[gid.x];
    workgroupBarrier();
    var stride: u32 = 32u;
    loop {{
        if (stride == 0u) {{ break; }}
        if (lid.x < stride) {{
            let acc = scratch[lid.x];
            scratch[lid.x] = {combine};
        }}
        workgroupBarrier();
        stride = stride / 2u;
    }}
    if (lid.x == 0u) {{ output[wid.x] = scratch[0]; }}
}}");

        // CPU reference: fold each workgroup's 64-element slice.
        let expect: Vec<u32> = (0..NUM_WG as usize)
            .map(|w| input[w * WG as usize..(w + 1) * WG as usize].iter().copied().reduce(fold).unwrap())
            .collect();

        let s = run_one(
            &mut g,
            &src,
            &[
                Buf { id: 1, init: u32s(&input) },
                Buf { id: 2, init: u32s(&vec![0u32; NUM_WG as usize]) },
            ],
            vec![whole(0, 1, (n * 4) as u64), whole(1, 2, (NUM_WG * 4) as u64)],
            (NUM_WG, 1, 1),
        );
        let got = read_u32s(&g, &s, 2, NUM_WG as usize);
        assert_eq!(
            got, expect,
            "shared-memory {label} reduction: each workgroup's result must be the bit-exact per-workgroup \
             {label} (proves workgroupBarrier synchronizes and scratch is isolated per workgroup)"
        );
    }
}

// =================================================================================================
// 3. ATOMICS — many invocations racing one storage location; the final value is the race-free answer
// =================================================================================================

/// Thousands of invocations hammer a SINGLE atomic storage location with `atomicAdd` / `atomicMax` /
/// `atomicMin`, and a fourth kernel proves `atomicExchange` conserves values. Each final value equals the
/// race-free arithmetic answer computed on the CPU — an atomic that dropped even one update (a lost
/// read-modify-write, not serialized) would fall short. This is the direct proof that the executor's compute
/// path serializes atomics correctly on lavapipe.
#[test]
fn atomics_serialize_add_max_min_exchange() {
    const WG: u32 = 64;
    const GROUPS: u32 = 64;
    let t: u32 = WG * GROUPS; // 4096 racing invocations

    // A deterministic per-invocation value, matched in WGSL and on the CPU (kept < 2^24 so max/min are lively
    // and no sum overflows u32).
    let hval = |g: u32| g.wrapping_mul(2_654_435_761) & 0x00FF_FFFF;

    let mut g = exec();

    // --- atomicAdd: final == sum_{g<T}(g+1) == T*(T+1)/2 (exact race-free total) ---
    {
        let src = format!("\
@group(0) @binding(0) var<storage, read_write> counter: atomic<u32>;
@compute @workgroup_size({WG})
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    atomicAdd(&counter, gid.x + 1u);
}}");
        let s = run_one(&mut g, &src, &[Buf { id: 1, init: u32s(&[0]) }], vec![whole(0, 1, 4)], (GROUPS, 1, 1));
        let expect = (0..t).fold(0u32, |a, k| a.wrapping_add(k + 1));
        assert_eq!(read_u32s(&g, &s, 1, 1)[0], expect,
            "atomicAdd of {t} invocations must sum to the exact race-free total (no lost update)");
    }

    // --- atomicMax: final == max_{g<T} hval(g) ---
    {
        let src = format!("\
@group(0) @binding(0) var<storage, read_write> m: atomic<u32>;
fn h(g: u32) -> u32 {{ return (g * 2654435761u) & 0x00FFFFFFu; }}
@compute @workgroup_size({WG})
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    atomicMax(&m, h(gid.x));
}}");
        let s = run_one(&mut g, &src, &[Buf { id: 1, init: u32s(&[0]) }], vec![whole(0, 1, 4)], (GROUPS, 1, 1));
        let expect = (0..t).map(hval).max().unwrap();
        assert_eq!(read_u32s(&g, &s, 1, 1)[0], expect,
            "atomicMax must settle on the true maximum across all invocations");
    }

    // --- atomicMin: final == min_{g<T} hval(g), starting from u32::MAX ---
    {
        let src = format!("\
@group(0) @binding(0) var<storage, read_write> m: atomic<u32>;
fn h(g: u32) -> u32 {{ return (g * 2654435761u) & 0x00FFFFFFu; }}
@compute @workgroup_size({WG})
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    atomicMin(&m, h(gid.x));
}}");
        let s = run_one(&mut g, &src, &[Buf { id: 1, init: u32s(&[u32::MAX]) }], vec![whole(0, 1, 4)], (GROUPS, 1, 1));
        let expect = (0..t).map(hval).min().unwrap();
        assert_eq!(read_u32s(&g, &s, 1, 1)[0], expect,
            "atomicMin must settle on the true minimum across all invocations");
    }

    // --- atomicExchange conservation: sum(out[]) + final_slot == sum_{g<T}(g+1) ---
    // Each invocation swaps `gid+1` into the slot and records the displaced old value. Every value that ever
    // occupied the slot (the 0 seed, plus every injected `gid+1`) is either later displaced out (recorded in
    // `out`) or remains as the final slot value — so the two multisets are equal and their sums match. A
    // dropped or duplicated exchange would break the invariant, so this proves exchange serializes.
    {
        let src = format!("\
@group(0) @binding(0) var<storage, read_write> slot: atomic<u32>;
@group(0) @binding(1) var<storage, read_write> out: array<u32>;
@compute @workgroup_size({WG})
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {{
    out[gid.x] = atomicExchange(&slot, gid.x + 1u);
}}");
        let s = run_one(
            &mut g,
            &src,
            &[Buf { id: 1, init: u32s(&[0]) }, Buf { id: 2, init: u32s(&vec![0u32; t as usize]) }],
            vec![whole(0, 1, 4), whole(1, 2, (t * 4) as u64)],
            (GROUPS, 1, 1),
        );
        let final_slot = read_u32s(&g, &s, 1, 1)[0] as u64;
        let out_sum: u64 = read_u32s(&g, &s, 2, t as usize).iter().map(|&v| v as u64).sum();
        let expect: u64 = (0..t as u64).map(|k| k + 1).sum();
        assert_eq!(out_sum + final_slot, expect,
            "atomicExchange must conserve every value (displaced-out sum + residue == injected sum): \
             proves exchanges serialize with no lost/duplicated swap");
    }
}

// =================================================================================================
// 4. STORAGE READ-MODIFY-WRITE — in-place transform across a multi-workgroup dispatch
// =================================================================================================

/// Each invocation reads its own storage element, transforms it (`x*x + i`), and writes it back — a
/// multi-workgroup, non-power-of-2-count dispatch with a guarded remainder. The bit-exact in-place result on
/// `[0, N)` plus the untouched sentinel tail proves the RMW is correct at every element and that the
/// remainder invocations (i >= N) write nothing.
#[test]
fn storage_rmw_in_place_multi_workgroup() {
    const N: u32 = 1500; // not a multiple of 64 → remainder invocations
    const PAD: u32 = 64;
    const SENTINEL: u32 = 0x0BAD_F00D;
    let out_len = (N + PAD) as usize;

    let mut data: Vec<u32> = (0..N).map(|i| i.wrapping_mul(2_246_822_519).wrapping_add(11) & 0xFFFF).collect();
    let mut expect: Vec<u32> = data.iter().enumerate().map(|(i, &v)| v.wrapping_mul(v).wrapping_add(i as u32)).collect();
    data.extend(std::iter::repeat(SENTINEL).take(PAD as usize));
    expect.extend(std::iter::repeat(SENTINEL).take(PAD as usize));

    let src = "\
@group(0) @binding(0) var<storage, read_write> data: array<u32>;
const N: u32 = 1500u;
@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < N) {
        let v = data[i];
        data[i] = v * v + i;
    }
}";
    let groups = N.div_ceil(64); // 24 groups → total 1536, remainder 36 (< PAD)

    let mut g = exec();
    let s = run_one(
        &mut g,
        src,
        &[Buf { id: 1, init: u32s(&data) }],
        vec![whole(0, 1, (out_len * 4) as u64)],
        (groups, 1, 1),
    );
    let got = read_u32s(&g, &s, 1, out_len);
    assert_eq!(got, expect,
        "storage RMW must transform every element bit-exact across the multi-workgroup dispatch and leave \
         the guarded remainder tail untouched");
}

// =================================================================================================
// 5. TWO-PASS COMPUTE — pass B reads the buffer pass A produced; the dependency is honored
// =================================================================================================

/// Two compute passes in ONE command buffer: pass A writes `mid[i] = src[i] + 100`, pass B reads `mid` and
/// writes `dst[i] = mid[i] * 2`. The executor runs each pass as its own submit+wait, so pass B observing
/// pass A's writes is the cross-pass dependency under test. Both `mid` (A's product) and `dst` (B's product,
/// which can only be right if it read A's output) are asserted bit-exact.
#[test]
fn two_pass_compute_dependency_honored() {
    const N: u32 = 500;
    let groups = N.div_ceil(64); // 8 groups → total 512, remainder 12 guarded
    let src_vals: Vec<u32> = (0..N).map(|i| i.wrapping_mul(13).wrapping_add(5)).collect();
    let mid_expect: Vec<u32> = src_vals.iter().map(|v| v.wrapping_add(100)).collect();
    let dst_expect: Vec<u32> = mid_expect.iter().map(|v| v.wrapping_mul(2)).collect();

    let pass_a = "\
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<storage, read_write> mid: array<u32>;
const N: u32 = 500u;
@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < N) { mid[i] = src[i] + 100u; }
}";
    let pass_b = "\
@group(0) @binding(0) var<storage, read> mid: array<u32>;
@group(0) @binding(1) var<storage, read_write> dst: array<u32>;
const N: u32 = 500u;
@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if (i < N) { dst[i] = mid[i] * 2u; }
}";

    let sz = (N * 4) as u64;
    let mut g = exec();
    // Shaders/pipelines: 1 = pass A, 2 = pass B. Buffers: 1 = src, 2 = mid, 3 = dst. Bind groups: 1 = A's
    // (src, mid), 2 = B's (mid, dst) — each a single set-0 group matching its pipeline's auto layout.
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv: wgsl_to_spirv(pass_a) },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::SpirV, spirv: wgsl_to_spirv(pass_b) },
            Cmd::CreateComputePipeline(1, ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "cs_main".into() }, label: String::new() }),
            Cmd::CreateComputePipeline(2, ComputePipelineDesc { compute: ShaderRef { module: 2, entry: "cs_main".into() }, label: String::new() }),
            Cmd::CreateBuffer(1, sbuf(sz)),
            Cmd::CreateBuffer(2, sbuf(sz)),
            Cmd::CreateBuffer(3, sbuf(sz)),
            Cmd::WriteBuffer { id: 1, offset: 0, data: u32s(&src_vals) },
            Cmd::CreateBindGroup(1, BindGroupDesc { set: 0, entries: vec![whole(0, 1, sz), whole(1, 2, sz)] }),
            Cmd::CreateBindGroup(2, BindGroupDesc { set: 0, entries: vec![whole(0, 2, sz), whole(1, 3, sz)] }),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Dispatch { x: groups, y: 1, z: 1 },
                    Enc::EndComputePass,
                    Enc::BeginComputePass,
                    Enc::SetPipeline(2),
                    Enc::SetBindGroup { index: 0, group: 2 },
                    Enc::Dispatch { x: groups, y: 1, z: 1 },
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    assert_eq!(read_u32s(&g, &s, 2, N as usize), mid_expect, "pass A must write mid = src + 100");
    assert_eq!(read_u32s(&g, &s, 3, N as usize), dst_expect,
        "pass B must read pass A's mid and write dst = mid*2 — the cross-pass dependency is honored");
}
