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
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
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
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
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
    let out = g
        .read_buffer(&s.resources, hl_gpu::BufferId(id), 0, n * 4)
        .unwrap();
    out.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
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
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv,
        },
        Cmd::CreateComputePipeline(
            1,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 1,
                    entry: "cs_main".into(),
                },
                label: String::new(),
            },
        ),
    ];
    for b in bufs {
        cmds.push(Cmd::CreateBuffer(b.id, sbuf(b.init.len() as u64)));
        cmds.push(Cmd::WriteBuffer {
            id: b.id,
            offset: 0,
            data: b.init.clone(),
        });
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
    BindEntry {
        binding,
        resource: BindResource::Buffer {
            id,
            offset: 0,
            size,
        },
    }
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
#[path = "compute_conformance/atomic.rs"]
mod atomic;
#[path = "compute_conformance/dependency.rs"]
mod dependency;
#[path = "compute_conformance/shared.rs"]
mod shared;
#[path = "compute_conformance/storage.rs"]
mod storage;
#[path = "compute_conformance/workgroup.rs"]
mod workgroup;
