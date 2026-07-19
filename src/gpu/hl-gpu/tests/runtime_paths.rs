//! RUNTIME-PATH coverage: every resource KIND driven create -> lookup -> destroy through the real runtime
//! pipeline (validate -> account -> dispatch -> CpuExecutor), asserting the per-kind residency charge and
//! that each id is looked up live then reclaimed on destroy; plus the capability handshake accepting a
//! compatible peer and rejecting an incompatible one on EVERY negotiation axis (wire / shader / command /
//! format), both at the protocol boundary and wired through the runtime `negotiate` service.
//!
//! Complements `runtime.rs` (failure atomicity + budget) and `session_lifecycle.rs` (teardown) by pinning
//! the create/lookup/destroy path for the kinds those files exercise only partially (sampler, shader, both
//! pipeline kinds, bind group, surface) and the three negotiation reject branches they never hit.

use hl_gpu::protocol::model::capability::{
    shader_payload, Capabilities, FeatureRequest, ALL_COMMANDS, COLOR_FORMATS,
};
use hl_gpu::protocol::model::command::etag;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorTargetState, ComputePipelineDesc,
    RenderPipelineDesc, SamplerDesc, ShaderRef, SurfaceDesc, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{Inst, KernelProgram, KERNEL_MAGIC};
use hl_gpu::{Cmd, CommandSink, CpuExecutor, GpuError, GpuExecutor, InProcessCommandSink};

fn placeholder_shader() -> KernelProgram {
    KernelProgram {
        entry: "k".into(),
        block: [1, 1, 1],
        params: vec![],
        param_bytes: 0,
        num_regions: 0,
        shared_bytes: 0,
        reg_count: 1,
        insts: vec![Inst::Ret],
    }
}

// =================================================================================================
// every resource kind: create -> lookup (id is live in its table) -> destroy (id reclaimed), with the
// per-kind residency charge asserted against the accounting model.
// =================================================================================================

#[test]
fn every_resource_kind_creates_looks_up_and_destroys_through_the_runtime() {
    let mut exec = CpuExecutor::new();
    exec.define_kernel(1, placeholder_shader());
    let mut sink = InProcessCommandSink::with_session(
        {
            let limits = hl_gpu::Limits::from_capabilities(exec.capabilities());
            hl_gpu::Session::new(
                limits,
                hl_gpu::GlobalLedger::unbounded(),
                Box::new(hl_gpu::FakeClock::new(0)),
            )
        },
        exec,
    );

    // One of EVERY resource kind the tables hold (buffer/texture/sampler/shader/render+compute
    // pipeline/bind group/surface/fence), created in dependency order through the runtime pipeline.
    let create = vec![
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 4096,
                usage: buffer_usage::STORAGE,
                label: String::new(),
            },
        ),
        Cmd::CreateTexture(
            1,
            TextureDesc {
                width: 4,
                height: 4,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::SAMPLED,
                label: String::new(),
            },
        ),
        Cmd::CreateSampler(
            1,
            SamplerDesc {
                min_filter: Filter::Nearest,
                mag_filter: Filter::Nearest,
                mip_filter: Filter::Nearest,
                address_u: AddressMode::Repeat,
                address_v: AddressMode::Repeat,
                address_w: AddressMode::Repeat,
            },
        ),
        Cmd::CreateShader {
            id: 1,
            kind: hl_gpu::ShaderPayloadKind::PtxKernel,
            spirv: vec![KERNEL_MAGIC, 0],
        },
        Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 1,
                    entry: "k".into(),
                },
                fragment: None,
                vertex_buffers: vec![],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xF,
                }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        ),
        Cmd::CreateComputePipeline(
            2,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 1,
                    entry: "k".into(),
                },
                label: String::new(),
            },
        ),
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![BindEntry {
                    binding: 0,
                    resource: BindResource::Buffer {
                        id: 1,
                        offset: 0,
                        size: 4096,
                    },
                }],
            },
        ),
        Cmd::CreateSurface(
            1,
            SurfaceDesc {
                width: 4,
                height: 4,
                format: TextureFormat::Bgra8Unorm,
                hlp_surface: 1,
            },
        ),
        Cmd::CreateFence(1),
    ];
    sink.submit(&create)
        .expect("every-kind create batch runs cleanly");

    // LOOKUP: each id is live in its own kind's table (the runtime is the singular id -> native owner).
    let r = &sink.session().resources;
    assert!(
        r.buffers.contains(1) && r.textures.contains(1) && r.samplers.contains(1),
        "buffer/texture/sampler live"
    );
    assert!(
        r.shaders.contains(1) && r.pipelines.contains(1) && r.pipelines.contains(2),
        "shader + both pipelines live"
    );
    assert!(
        r.bind_groups.contains(1) && r.surfaces.contains(1) && r.fences.contains(1),
        "bind group/surface/fence live"
    );
    assert_eq!(r.live_count(), 9, "exactly nine live objects, one per kind");

    // Per-kind residency charge (from the accounting model): buffer 4096, texture 4*4*4=64, sampler 64,
    // shader 2 words*4=8, render pipe 4096, compute pipe 4096, bind group 256 + min(4096) = 4352,
    // surface 4*4*4=64, fence 128.
    let expect_bytes = 4096 + 64 + 64 + 8 + 4096 + 4096 + 4352 + 64 + 128;
    assert_eq!(
        sink.session().residency_bytes(),
        expect_bytes,
        "aggregate per-kind residency charge"
    );
    assert_eq!(sink.session().object_count(), 9);
    assert_eq!(
        sink.session().compiled_cache_bytes(),
        4096 + 4096,
        "only the two pipelines meter the compiled cache"
    );

    // DESTROY every kind (bind group + pipelines before the buffer/shader they reference is not required
    // by the CPU oracle, but destroy in reverse dependency order to stay realistic).
    let destroy = vec![
        Cmd::DestroyFence(1),
        Cmd::DestroySurface(1),
        Cmd::DestroyBindGroup(1),
        Cmd::DestroyPipeline(2),
        Cmd::DestroyPipeline(1),
        Cmd::DestroyShader(1),
        Cmd::DestroySampler(1),
        Cmd::DestroyTexture(1),
        Cmd::DestroyBuffer(1),
    ];
    sink.submit(&destroy)
        .expect("every-kind destroy batch runs cleanly");

    // Reclaimed: every table empty, residency + object count + compiled cache back to zero.
    let r = &sink.session().resources;
    assert_eq!(r.live_count(), 0, "all kinds reclaimed");
    assert!(!r.buffers.contains(1) && !r.pipelines.contains(1) && !r.pipelines.contains(2));
    assert_eq!(
        sink.session().residency_bytes(),
        0,
        "residency fully refunded"
    );
    assert_eq!(sink.session().object_count(), 0);
    assert_eq!(
        sink.session().compiled_cache_bytes(),
        0,
        "compiled cache refunded on pipeline destroy"
    );
}

// =================================================================================================
// dispatch routing: a submit carrying a draw AND a compute dispatch reaches the executor, incrementing
// the CpuExecutor's per-op work counters (proof the dispatch path routes each op, not just accepts them).
// =================================================================================================

#[test]
fn dispatch_routes_draw_and_compute_work_to_the_executor() {
    use hl_gpu::{CommandBuffer, Enc};
    let mut exec = CpuExecutor::new();
    exec.define_kernel(1, placeholder_shader());
    let mut sink = InProcessCommandSink::with_session(
        {
            let mut limits = hl_gpu::Limits::from_capabilities(exec.capabilities());
            limits.copy_alignment = 1;
            hl_gpu::Session::new(
                limits,
                hl_gpu::GlobalLedger::unbounded(),
                Box::new(hl_gpu::FakeClock::new(0)),
            )
        },
        exec,
    );

    sink.submit(&[
        Cmd::CreateShader {
            id: 1,
            kind: hl_gpu::ShaderPayloadKind::PtxKernel,
            spirv: vec![KERNEL_MAGIC, 0],
        },
        Cmd::CreateTexture(
            1,
            TextureDesc {
                width: 2,
                height: 2,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::RENDER_TARGET,
                label: String::new(),
            },
        ),
        Cmd::CreateRenderPipeline(
            1,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 1,
                    entry: "k".into(),
                },
                fragment: None,
                vertex_buffers: vec![],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xF,
                }],
                depth: None,
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        ),
        Cmd::CreateComputePipeline(
            2,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 1,
                    entry: "k".into(),
                },
                label: String::new(),
            },
        ),
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 16,
                usage: buffer_usage::STORAGE,
                label: String::new(),
            },
        ),
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
    ])
    .unwrap();

    sink.submit(&[Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![hl_gpu::protocol::model::descriptor::ColorAttachment {
                    texture: 1,
                    load: hl_gpu::protocol::model::enums::LoadOp::Load,
                    clear: [0.0; 4],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(1),
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
            Enc::BeginComputePass,
            Enc::SetPipeline(2),
            Enc::SetBindGroup { index: 0, group: 1 },
            Enc::Dispatch { x: 1, y: 1, z: 1 },
            Enc::EndComputePass,
        ],
        signal: None,
    })])
    .unwrap();

    assert_eq!(
        sink.executor().draws,
        1,
        "the draw op routed to the executor"
    );
    assert_eq!(
        sink.executor().dispatches,
        1,
        "the compute dispatch routed to the executor"
    );
}

// =================================================================================================
// capability handshake: negotiation ACCEPTS a compatible peer and REJECTS an incompatible one on every
// axis (wire version / shader payload / command tag / texture format).
// =================================================================================================

#[test]
fn negotiate_accepts_a_compatible_request() {
    let caps = Capabilities::full("backend");
    // A subset request: a lower/equal-or-matching wire, a subset of shader payloads/commands/formats.
    let req = FeatureRequest {
        wire_version: caps.wire_version,
        shader_payloads: shader_payload::SPIRV | shader_payload::GLSL,
        command_bits: hl_gpu::Capabilities::command_bits(&[
            etag::BEGIN_RENDER_PASS,
            etag::DRAW,
            etag::DISPATCH,
        ]),
        texture_formats: TextureFormat::bits(&[
            TextureFormat::Rgba8Unorm,
            TextureFormat::Bgra8Unorm,
        ]),
    };
    assert!(
        caps.negotiate(&req).is_ok(),
        "a fully-covered request negotiates cleanly"
    );
    // The advertised full descriptor covers the whole IR surface, so the maximal request also passes.
    let full_req = FeatureRequest {
        wire_version: caps.wire_version,
        shader_payloads: caps.shader_payloads,
        command_bits: hl_gpu::Capabilities::command_bits(ALL_COMMANDS),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
    };
    assert!(
        caps.negotiate(&full_req).is_ok(),
        "the maximal in-surface request negotiates cleanly"
    );
}

#[test]
fn negotiate_rejects_each_incompatible_axis() {
    let caps = Capabilities::full("backend");
    let base = FeatureRequest {
        wire_version: caps.wire_version,
        shader_payloads: 0,
        command_bits: 0,
        texture_formats: 0,
    };

    // wire version mismatch.
    assert_eq!(
        caps.negotiate(&FeatureRequest {
            wire_version: caps.wire_version + 1,
            ..base.clone()
        })
        .unwrap_err(),
        GpuError::Unsupported("capability: wire version mismatch"),
    );
    // a shader payload the backend never advertised (bit 31, outside the advertised set).
    assert_eq!(
        caps.negotiate(&FeatureRequest {
            shader_payloads: 1 << 31,
            ..base.clone()
        })
        .unwrap_err(),
        GpuError::Unsupported("capability: shader payload not supported"),
    );
    // a command tag the backend does not replay (bit 63).
    assert_eq!(
        caps.negotiate(&FeatureRequest {
            command_bits: 1 << 63,
            ..base.clone()
        })
        .unwrap_err(),
        GpuError::Unsupported("capability: command tag not supported"),
    );
    // a texture format the backend cannot materialize (a depth format is not in the color-only advertisement).
    let depth_bit = TextureFormat::bits(&[TextureFormat::Depth24PlusStencil8]);
    assert_eq!(
        caps.negotiate(&FeatureRequest {
            texture_formats: depth_bit,
            ..base.clone()
        })
        .unwrap_err(),
        GpuError::Unsupported("capability: texture format not supported"),
    );
}

#[test]
fn negotiate_through_the_runtime_service_records_or_rejects() {
    use hl_gpu::runtime::service;
    // ACCEPT: a compatible request wired through the service records the negotiated caps on the session.
    let exec = CpuExecutor::new();
    let caps = exec.capabilities();
    let mut s = hl_gpu::Session::new(
        hl_gpu::Limits::from_capabilities(caps.clone()),
        hl_gpu::GlobalLedger::unbounded(),
        Box::new(hl_gpu::FakeClock::new(0)),
    );
    let req = FeatureRequest {
        wire_version: caps.wire_version,
        shader_payloads: shader_payload::KERNEL,
        command_bits: hl_gpu::Capabilities::command_bits(&[etag::DISPATCH]),
        texture_formats: 0,
    };
    let negotiated =
        service::negotiate::negotiate(&mut s, &exec, &req).expect("compatible negotiate");
    assert_eq!(negotiated.name, "hl-cpu");
    assert!(
        s.caps.is_some(),
        "a successful negotiation records the caps on the session"
    );

    // REJECT (non-wire reason): the CpuExecutor advertises only KERNEL, so requesting SPIR-V is rejected
    // and NO caps are recorded — the runtime wiring surfaces the protocol reject cleanly.
    let exec2 = CpuExecutor::new();
    let caps2 = exec2.capabilities();
    let mut s2 = hl_gpu::Session::new(
        hl_gpu::Limits::from_capabilities(caps2.clone()),
        hl_gpu::GlobalLedger::unbounded(),
        Box::new(hl_gpu::FakeClock::new(0)),
    );
    let bad = FeatureRequest {
        wire_version: caps2.wire_version,
        shader_payloads: shader_payload::SPIRV,
        command_bits: 0,
        texture_formats: 0,
    };
    assert_eq!(
        service::negotiate::negotiate(&mut s2, &exec2, &bad).unwrap_err(),
        GpuError::Unsupported("capability: shader payload not supported"),
    );
    assert!(s2.caps.is_none(), "a failed negotiation records no caps");
}
