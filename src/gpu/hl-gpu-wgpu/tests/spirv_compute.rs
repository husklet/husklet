//! Real SPIR-V compute pipelines on the wgpu executor — the path Zed (and any wgpu app that builds an
//! internal compute pipeline, e.g. wgpu-core's indirect-draw VALIDATION pipeline) needs to reach our
//! device.
//!
//! Before this path existed, `create_compute_pipeline` accepted only a PTX-`Kernel` shader; every SPIR-V
//! module became a stage-neutral `ShaderNative::Module`, so an arbitrary SPIR-V *compute* shader was
//! rejected with `needs-kernel-shader` — which the guest wgpu saw as a lost device (Zed then fell back to
//! llvmpipe). These cases drive genuine SPIR-V (minted from a WGSL seed via naga `wgsl-in → spv-out`, the
//! exact round trip the guest's SPIR-V ABI relies on) through the real runtime pipeline and assert the
//! compute actually RAN — proving the pipeline is not just created but correctly bound.
//!
//! Each case runs against the same headless software Vulkan device (lavapipe) the conformance suite uses.

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ComputePipelineDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// -------------------------------------------------------------------------------------------------
// shared device + runtime-pipeline harness (mirrors tests/conformance.rs)
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

/// Submit `cmds` as one batch through validate → account → dispatch → execute, returning the `Session`
/// so its `resources` can be read back.
fn run_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds).expect("compute program must run cleanly");
    s
}

/// Same as [`run_batch`] but returns the runtime result so a case can assert an error (or its absence).
fn try_batch(exec: &mut WgpuExecutor, cmds: &[Cmd]) -> hl_gpu::Result<Session> {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    let mut s = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut s, exec, 0, cmds)?;
    Ok(s)
}

fn buf(size: u64, usage: u32) -> BufferDesc {
    BufferDesc {
        size,
        usage,
        label: String::new(),
    }
}

fn u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn read_u32s(g: &WgpuExecutor, s: &Session, id: u32, n: usize) -> Vec<u32> {
    let out = g.read_buffer(&s.resources, BufferId(id), 0, n * 4).unwrap();
    out.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

/// Mint real SPIR-V from a WGSL seed (naga `wgsl-in → spv-out`). The executor translates it straight back
/// (`spv-in → wgsl-out`) and builds a real compute pipeline, so the SPIR-V genuinely drives the dispatch.
fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("seed wgsl parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed wgsl validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit spir-v")
}

// -------------------------------------------------------------------------------------------------
// cases
// -------------------------------------------------------------------------------------------------

/// The core proof: a SPIR-V compute shader with ONE storage bind group (`data[i] *= 2`), dispatched and
/// read back. Before the SPIR-V compute path, `CreateComputePipeline` on this module errored with
/// `needs-kernel-shader` and the batch never reached the dispatch — so this case is the FAIL-before /
/// PASS-after boundary for the whole feature.
#[test]
fn spirv_compute_doubles_storage_buffer() {
    let seed = r#"
        @group(0) @binding(0) var<storage, read_write> data: array<u32>;
        @compute @workgroup_size(4)
        fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
            data[gid.x] = data[gid.x] * 2u;
        }
    "#;
    let spirv = wgsl_to_spirv(seed);

    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
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
            Cmd::CreateBuffer(1, buf(16, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: u32s(&[1, 2, 3, 4]),
            },
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 16,
                        },
                    }],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Dispatch { x: 1, y: 1, z: 1 },
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    assert_eq!(
        read_u32s(&g, &s, 1, 4),
        vec![2, 4, 6, 8],
        "real SPIR-V compute must double each element on lavapipe"
    );
}

/// Two bind groups: group 0 the read-write storage data, group 1 a uniform scale factor. Each group binds
/// at its declared set index against the pipeline's OWN auto-derived per-group layout
/// (`get_bind_group_layout(index)`) — the multi-group shape wgpu-core's validation compute pipeline has.
#[test]
fn spirv_compute_two_bind_groups_scale() {
    let seed = r#"
        @group(0) @binding(0) var<storage, read_write> data: array<u32>;
        @group(1) @binding(0) var<uniform> factor: vec4<u32>;
        @compute @workgroup_size(4)
        fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
            data[gid.x] = data[gid.x] * factor.x;
        }
    "#;
    let spirv = wgsl_to_spirv(seed);

    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
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
            Cmd::CreateBuffer(1, buf(16, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateBuffer(2, buf(16, buffer_usage::UNIFORM | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: u32s(&[1, 2, 3, 4]),
            },
            // factor.x = 3 (remaining vec4 lanes unused).
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: u32s(&[3, 0, 0, 0]),
            },
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 16,
                        },
                    }],
                },
            ),
            Cmd::CreateBindGroup(
                2,
                BindGroupDesc {
                    set: 1,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 2,
                            offset: 0,
                            size: 16,
                        },
                    }],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginComputePass,
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::SetBindGroup { index: 1, group: 2 },
                    Enc::Dispatch { x: 1, y: 1, z: 1 },
                    Enc::EndComputePass,
                ],
                signal: None,
            }),
        ],
    );
    assert_eq!(
        read_u32s(&g, &s, 1, 4),
        vec![3, 6, 9, 12],
        "2-bind-group SPIR-V compute must scale each element by the uniform factor"
    );
}

/// The exact Zed blocker: a SPIR-V compute shader that declares a `var<push_constant>`. wgpu-core builds
/// such a pipeline internally during device creation (indirect-draw validation). Creation with an auto
/// layout must SUCCEED (wgpu derives the push-constant range from the module; the PUSH_CONSTANTS feature is
/// requested by `device::acquire` when the adapter advertises it). Before the SPIR-V compute path this
/// `CreateComputePipeline` returned `Unsupported("needs a kernel shader")`, which the guest read as a lost
/// device. The pipeline is only CREATED here (not dispatched): the protocol carries no push-constant data,
/// and Zed's validation pipeline is likewise never dispatched through this executor.
#[test]
fn spirv_compute_pipeline_with_push_constant_creates() {
    let seed = r#"
        var<push_constant> pc: u32;
        @group(0) @binding(0) var<storage, read_write> data: array<u32>;
        @compute @workgroup_size(1)
        fn cs_main() {
            data[0] = data[0] + pc;
        }
    "#;
    let spirv = wgsl_to_spirv(seed);

    let mut g = exec();
    let r = try_batch(
        &mut g,
        &[
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
        ],
    );
    assert!(
        r.is_ok(),
        "a SPIR-V compute pipeline with a push constant must CREATE (the Zed device-creation blocker): {:?}",
        r.err()
    );
}
