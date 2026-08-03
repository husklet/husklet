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
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment,
    ColorTargetState, ComputePipelineDesc, DepthAttachment, DepthState, Extent3d, Origin3d,
    PipelineBinding, PipelineBindingKind, PipelineLayout, RenderMultisample, RenderPipelineDesc,
    SamplerDesc, ShaderRef, TextureDesc, TextureSubresource, TextureViewDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat,
    Topology,
};
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

fn wgsl_2d_to_spirv_1d(src: &str) -> Vec<u32> {
    let mut module = naga::front::wgsl::parse_str(src).expect("seed wgsl parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed wgsl validates");
    let images = module.global_variables.iter().filter_map(|(handle, variable)| {
        matches!(module.types[variable.ty].inner, naga::TypeInner::Image { .. })
            .then_some((handle, variable.ty))
    }).collect::<Vec<_>>();
    for (global, old_ty) in images {
        let mut ty = module.types[old_ty].clone();
        let naga::TypeInner::Image { ref mut dim, .. } = ty.inner else { unreachable!() };
        *dim = naga::ImageDimension::D1;
        module.global_variables[global].ty = module.types.insert(ty, naga::Span::default());
    }
    let function = &mut module.entry_points[0].function;
    let coordinates = function.body.iter().filter_map(|statement| match statement {
        naga::Statement::ImageAtomic { coordinate, .. } => {
            let naga::Expression::Compose { components, .. } = &function.expressions[*coordinate]
                else { return None };
            Some((*coordinate, components[0]))
        }
        _ => None,
    }).collect::<Vec<_>>();
    for statement in function.body.iter_mut() {
        if let naga::Statement::ImageAtomic { coordinate, .. } = statement {
            *coordinate = coordinates.iter().find(|(vector, _)| vector == coordinate).unwrap().1;
        }
    }
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit 1D SPIR-V")
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

#[test]
fn spirv_compute_dynamically_selects_second_storage_buffer() {
    let seed = r#"
        struct Item { value: u32 }
        @group(0) @binding(0) var<storage, read_write> items: binding_array<Item, 2>;
        @group(0) @binding(1) var<uniform> selected: vec4<u32>;
        @compute @workgroup_size(1)
        fn cs_main() {
            items[selected.x].value = 99u;
        }
    "#;
    let spirv = wgsl_to_spirv(seed);
    let mut g = exec();
    if g.capabilities().binding_arrays
        & hl_gpu::protocol::model::capability::binding_array::STORAGE_BUFFER
        == 0
    {
        return;
    }
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateComputePipelineLayout(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "cs_main".into(),
                    },
                    label: String::new(),
                },
                PipelineLayout {
                    bindings: vec![
                        PipelineBinding {
                            group: 0,
                            binding: 0,
                            count: 2,
                            kind: PipelineBindingKind::StorageBuffer,
                        },
                        PipelineBinding {
                            group: 0,
                            binding: 1,
                            count: 1,
                            kind: PipelineBindingKind::UniformBuffer,
                        },
                    ],
                },
            ),
            Cmd::CreateBuffer(1, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateBuffer(2, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateBuffer(3, buf(16, buffer_usage::UNIFORM | buffer_usage::COPY_DST)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: u32s(&[7]),
            },
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: u32s(&[8]),
            },
            Cmd::WriteBuffer {
                id: 3,
                offset: 0,
                data: u32s(&[1, 0, 0, 0]),
            },
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 4,
                            },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: 4,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 3,
                                offset: 0,
                                size: 16,
                            },
                        },
                    ],
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
            // Vulkan out-of-range descriptor indexing must not alias element zero.
            Cmd::WriteBuffer {
                id: 3,
                offset: 0,
                data: u32s(&[9, 0, 0, 0]),
            },
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
    assert_eq!(read_u32s(&g, &s, 1, 1), vec![7]);
    assert_eq!(read_u32s(&g, &s, 2, 1), vec![99]);
}

#[test]
fn spirv_compute_scalarizes_dynamic_uniform_buffer_array_reads() {
    let seed = r#"
        struct Item { value: vec4<u32> }
        @group(0) @binding(0) var<uniform> items: binding_array<Item, 2>;
        @group(0) @binding(1) var<uniform> selected: vec4<u32>;
        @group(0) @binding(2) var<storage, read_write> output: array<u32>;
        @compute @workgroup_size(1)
        fn cs_main() {
            output[0] = items[selected.x].value.x;
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
            Cmd::CreateComputePipelineLayout(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "cs_main".into(),
                    },
                    label: String::new(),
                },
                PipelineLayout {
                    bindings: vec![
                        PipelineBinding {
                            group: 0,
                            binding: 0,
                            count: 2,
                            kind: PipelineBindingKind::UniformBuffer,
                        },
                        PipelineBinding {
                            group: 0,
                            binding: 1,
                            count: 1,
                            kind: PipelineBindingKind::UniformBuffer,
                        },
                        PipelineBinding {
                            group: 0,
                            binding: 2,
                            count: 1,
                            kind: PipelineBindingKind::StorageBuffer,
                        },
                    ],
                },
            ),
            Cmd::CreateBuffer(1, buf(16, buffer_usage::UNIFORM)),
            Cmd::CreateBuffer(2, buf(16, buffer_usage::UNIFORM)),
            Cmd::CreateBuffer(3, buf(16, buffer_usage::UNIFORM)),
            Cmd::CreateBuffer(4, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: u32s(&[17, 0, 0, 0]),
            },
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: u32s(&[29, 0, 0, 0]),
            },
            Cmd::WriteBuffer {
                id: 3,
                offset: 0,
                data: u32s(&[1, 0, 0, 0]),
            },
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 16,
                            },
                        },
                        BindEntry {
                            // Scalar tail after guest bindings 0, 1 and 2.
                            binding: 3,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: 16,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 3,
                                offset: 0,
                                size: 16,
                            },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Buffer {
                                id: 4,
                                offset: 0,
                                size: 4,
                            },
                        },
                    ],
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
    assert_eq!(read_u32s(&g, &s, 4, 1), vec![29]);
}

#[test]
fn spirv_compute_dynamically_samples_second_texture() {
    let source = r#"
        @group(0) @binding(0) var images: binding_array<texture_2d<f32>, 2>;
        @group(0) @binding(1) var<uniform> selected: vec4<u32>;
        @group(0) @binding(2) var<storage, read_write> output: array<u32>;
        @compute @workgroup_size(1)
        fn cs_main() {
            output[0] = u32(textureLoad(images[selected.x], vec2<i32>(0, 0), 0).x * 255.0);
        }
    "#;
    let shader = wgsl_to_spirv(source);
    let mut g = exec();
    let required = hl_gpu::protocol::model::capability::binding_array::SAMPLED_TEXTURE;
    if g.capabilities().binding_arrays & required == 0
        || g.capabilities().non_uniform_binding_arrays & required == 0
    {
        return;
    }
    let texture = |label: &str| TextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
        label: label.into(),
    };
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: shader,
            },
            Cmd::CreateComputePipelineLayout(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "cs_main".into(),
                    },
                    label: String::new(),
                },
                PipelineLayout {
                    bindings: vec![
                        PipelineBinding {
                            group: 0,
                            binding: 0,
                            count: 2,
                            kind: PipelineBindingKind::SampledTexture,
                        },
                        PipelineBinding {
                            group: 0,
                            binding: 1,
                            count: 1,
                            kind: PipelineBindingKind::UniformBuffer,
                        },
                        PipelineBinding {
                            group: 0,
                            binding: 2,
                            count: 1,
                            kind: PipelineBindingKind::StorageBuffer,
                        },
                    ],
                },
            ),
            Cmd::CreateTexture(1, texture("first")),
            Cmd::CreateTexture(2, texture("second")),
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_SRC)),
            Cmd::CreateBuffer(2, buf(16, buffer_usage::UNIFORM)),
            Cmd::CreateBuffer(3, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![17, 0, 0, 255, 231, 0, 0, 255],
            },
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: u32s(&[1, 0, 0, 0]),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 4,
                        dst: 1,
                        mip: 0,
                        width: 1,
                        height: 1,
                    },
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 4,
                        bytes_per_row: 4,
                        dst: 2,
                        mip: 0,
                        width: 1,
                        height: 1,
                    },
                ],
                signal: None,
            }),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::TextureArray { ids: vec![1, 2] },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: 16,
                            },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Buffer {
                                id: 3,
                                offset: 0,
                                size: 4,
                            },
                        },
                    ],
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
    assert_eq!(read_u32s(&g, &s, 3, 1), vec![231]);
}

#[test]
fn spirv_compute_dynamically_writes_second_storage_texture() {
    let source = r#"
        @group(0) @binding(0)
        var images: binding_array<texture_storage_2d<rgba8unorm, write>, 2>;
        @group(0) @binding(1) var<uniform> selected: vec4<u32>;
        @compute @workgroup_size(1)
        fn cs_main() {
            textureStore(images[selected.x], vec2<i32>(0, 0), vec4<f32>(0.2, 0.4, 0.8, 1.0));
        }
    "#;
    let shader = wgsl_to_spirv(source);
    let mut g = exec();
    let texture = |label: &str| TextureDesc {
        width: 1,
        height: 1,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::STORAGE | texture_usage::COPY_SRC,
        label: label.into(),
    };
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: shader,
            },
            Cmd::CreateComputePipelineLayout(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "cs_main".into(),
                    },
                    label: String::new(),
                },
                PipelineLayout {
                    bindings: vec![
                        PipelineBinding {
                            group: 0,
                            binding: 0,
                            count: 2,
                            kind: PipelineBindingKind::StorageTexture,
                        },
                        PipelineBinding {
                            group: 0,
                            binding: 1,
                            count: 1,
                            kind: PipelineBindingKind::UniformBuffer,
                        },
                    ],
                },
            ),
            Cmd::CreateTexture(1, texture("first")),
            Cmd::CreateTexture(2, texture("second")),
            Cmd::CreateBuffer(1, buf(16, buffer_usage::UNIFORM)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: u32s(&[1, 0, 0, 0]),
            },
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Texture { id: 1 },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 16,
                            },
                        },
                    ],
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
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: u32s(&[9, 0, 0, 0]),
            },
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
    assert_eq!(g.read_texture(&s.resources, 1).unwrap(), vec![0, 0, 0, 0]);
    assert_eq!(
        g.read_texture(&s.resources, 2).unwrap(),
        vec![51, 102, 204, 255]
    );
}

#[test]
fn spirv_compute_atomically_adds_r32uint_storage_texture() {
    let source = r#"
        @group(0) @binding(0)
        var image: texture_storage_2d<r32uint, atomic>;
        @group(0) @binding(1) var<storage, read_write> output: array<u32>;
        @compute @workgroup_size(1)
        fn cs_main() {
            output[0] = textureAtomicAdd(image, vec2<i32>(0, 0), 7u);
            output[1] = textureAtomicCompareExchangeWeak(image, vec2<i32>(0, 0), 7u, 11u);
        }
    "#;
    let shaders = [
        (TextureDim::D2, wgsl_to_spirv(source)),
        (TextureDim::D1, wgsl_2d_to_spirv_1d(source)),
    ];
    // Keep this native-feature probe on its own device so the assertion covers feature negotiation,
    // texture creation, dispatch and readback as one isolated device lifetime.
    for (dim, shader) in shaders {
        let mut g = WgpuExecutor::new(DeviceConfig::default()).expect("acquire atomic-capable Metal device");
        let s = run_batch(
            &mut g,
            &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: shader,
            },
            Cmd::CreateComputePipelineLayout(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "cs_main".into(),
                    },
                    label: String::new(),
                },
                PipelineLayout {
                    bindings: vec![
                        PipelineBinding {
                            group: 0,
                            binding: 0,
                            count: 1,
                            kind: PipelineBindingKind::StorageTexture,
                        },
                        PipelineBinding {
                            group: 0,
                            binding: 1,
                            count: 1,
                            kind: PipelineBindingKind::StorageBuffer,
                        },
                    ],
                },
            ),
            Cmd::CreateTexture(
                1,
                TextureDesc {
                    width: 1,
                    height: 1,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim,
                    format: TextureFormat::R32Uint,
                    usage: texture_usage::STORAGE | texture_usage::COPY_SRC,
                    label: "atomic-r32uint".into(),
                },
            ),
            Cmd::CreateBuffer(1, buf(8, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Texture { id: 1 },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 8,
                            },
                        },
                    ],
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
        assert_eq!(read_u32s(&g, &s, 1, 2), vec![0, 7]);
        assert_eq!(g.read_texture(&s.resources, 1).unwrap(), 11u32.to_le_bytes());
    }
}

#[test]
fn spirv_compute_loads_and_stores_r32sint_storage_texture() {
    let source = r#"
        @group(0) @binding(0)
        var image: texture_storage_2d<r32sint, read_write>;
        @group(0) @binding(1) var<storage, read_write> output: array<i32>;
        @compute @workgroup_size(1)
        fn cs_main() {
            output[0] = textureLoad(image, vec2<i32>(0, 0)).x;
            textureStore(image, vec2<i32>(0, 0), vec4<i32>(-13, 0, 0, 0));
        }
    "#;
    let shader = wgsl_to_spirv(source);
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: shader,
            },
            Cmd::CreateComputePipelineLayout(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "cs_main".into(),
                    },
                    label: String::new(),
                },
                PipelineLayout {
                    bindings: vec![
                        PipelineBinding {
                            group: 0,
                            binding: 0,
                            count: 1,
                            kind: PipelineBindingKind::StorageTexture,
                        },
                        PipelineBinding {
                            group: 0,
                            binding: 1,
                            count: 1,
                            kind: PipelineBindingKind::StorageBuffer,
                        },
                    ],
                },
            ),
            Cmd::CreateTexture(
                1,
                TextureDesc {
                    width: 1,
                    height: 1,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim: TextureDim::D2,
                    format: TextureFormat::R32Sint,
                    usage: texture_usage::STORAGE | texture_usage::COPY_SRC,
                    label: "read-write-r32sint".into(),
                },
            ),
            Cmd::CreateBuffer(1, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Texture { id: 1 },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 4,
                            },
                        },
                    ],
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
    assert_eq!(read_u32s(&g, &s, 1, 1), vec![0]);
    assert_eq!(g.read_texture(&s.resources, 1).unwrap(), (-13i32).to_le_bytes());
}

#[test]
fn spirv_compute_samples_native_bc1_and_bc3_uploads() {
    let source = r#"
        @group(0) @binding(0) var image: texture_2d<f32>;
        @group(0) @binding(1) var<storage, read_write> output: array<u32>;
        @compute @workgroup_size(1)
        fn cs_main() {
            let color = textureLoad(image, vec2<i32>(0, 0), 0);
            output[0] = u32(color.r * 255.0);
            output[1] = u32(color.g * 255.0);
            output[2] = u32(color.b * 255.0);
            output[3] = u32(color.a * 255.0);
        }
    "#;
    let shader = wgsl_to_spirv(source);
    let mut g = exec();
    for (format, block, extra_usage, expected) in [
        (
            TextureFormat::Bc1RgbaUnorm,
            vec![0x00, 0xf8, 0x00, 0x00, 0, 0, 0, 0],
            0,
            vec![255, 0, 0, 255],
        ),
        (
            // c0 < c1 selects BC1's three-colour palette. Selector 3 is transparent for RGBA but
            // opaque black for Vulkan's RGB spelling, carried by the internal semantic usage bit.
            TextureFormat::Bc1RgbaUnorm,
            vec![0x1f, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff, 0xff],
            texture_usage::OPAQUE_BC1_RGB,
            vec![0, 0, 0, 255],
        ),
        (
            TextureFormat::Bc3RgbaUnorm,
            vec![
                0xff, 0x00, 0, 0, 0, 0, 0, 0, 0x00, 0xf8, 0x00, 0x00, 0, 0, 0, 0,
            ],
            0,
            vec![255, 0, 0, 255],
        ),
    ] {
        if !g.capabilities().supports_format(format) {
            continue;
        }
        let block_len = block.len() as u32;
        let s = run_batch(
            &mut g,
            &[
                Cmd::CreateShader {
                    id: 1,
                    kind: ShaderPayloadKind::SpirV,
                    spirv: shader.clone(),
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
                Cmd::CreateTexture(
                    1,
                    TextureDesc {
                        width: 4,
                        height: 4,
                        depth: 1,
                        mip_levels: 1,
                        sample_count: 1,
                        dim: TextureDim::D2,
                        format,
                        usage: texture_usage::SAMPLED | texture_usage::COPY_DST | extra_usage,
                        label: "bc".into(),
                    },
                ),
                Cmd::CreateBuffer(1, buf(block.len() as u64, buffer_usage::COPY_SRC)),
                Cmd::CreateBuffer(2, buf(16, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
                Cmd::WriteBuffer {
                    id: 1,
                    offset: 0,
                    data: block,
                },
                Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: block_len,
                        dst: 1,
                        mip: 0,
                        width: 4,
                        height: 4,
                    }],
                    signal: None,
                }),
                Cmd::CreateBindGroup(
                    1,
                    BindGroupDesc {
                        set: 0,
                        entries: vec![
                            BindEntry {
                                binding: 0,
                                resource: BindResource::Texture { id: 1 },
                            },
                            BindEntry {
                                binding: 1,
                                resource: BindResource::Buffer {
                                    id: 2,
                                    offset: 0,
                                    size: 16,
                                },
                            },
                        ],
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
        assert_eq!(read_u32s(&g, &s, 2, 4), expected, "{format:?}");
        if extra_usage == texture_usage::OPAQUE_BC1_RGB {
            assert_eq!(g.read_texture(&s.resources, 1).unwrap(), [0x1f, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff, 0xff]);
        }
    }
}

#[test]
fn opaque_bc1_shadow_tracks_mip_layer_view_and_texture_copy() {
    let mut g = exec();
    let block = vec![0x1f, 0x00, 0x00, 0xf8, 0xff, 0xff, 0xff, 0xff];
    let texture = |label: &str| TextureDesc {
        width: 8,
        height: 8,
        depth: 2,
        mip_levels: 2,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Bc1RgbaUnorm,
        usage: texture_usage::SAMPLED
            | texture_usage::COPY_SRC
            | texture_usage::COPY_DST
            | texture_usage::OPAQUE_BC1_RGB,
        label: label.into(),
    };
    let sub = TextureSubresource {
        mip: 1,
        layer: 1,
        aspect: hl_gpu::protocol::model::enums::TextureAspect::All,
    };
    let view = |texture| TextureViewDesc {
        texture,
        dim: TextureDim::D2,
        format: TextureFormat::Bc1RgbaUnorm,
        aspect: hl_gpu::protocol::model::enums::TextureAspect::All,
        base_mip: 1,
        mip_count: 1,
        base_layer: 1,
        layer_count: 1,
    };
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(1, texture("opaque-bc1-source")),
            Cmd::CreateTexture(2, texture("opaque-bc1-destination")),
            Cmd::CreateBuffer(1, buf(8, buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: block.clone(),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTextureRegion {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 8,
                        rows_per_image: 1,
                        dst: 1,
                        dst_sub: sub.clone(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    },
                    Enc::CopyTextureToTexture {
                        src: 1,
                        src_sub: sub.clone(),
                        src_origin: Origin3d::default(),
                        dst: 2,
                        dst_sub: sub,
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 4,
                            height: 4,
                            depth: 1,
                        },
                    },
                ],
                signal: None,
            }),
            Cmd::CreateTextureView(3, view(1)),
            Cmd::CreateTextureView(4, view(2)),
        ],
    );
    assert_eq!(g.read_texture(&s.resources, 3).unwrap(), block);
    assert_eq!(g.read_texture(&s.resources, 4).unwrap(), block);
}

#[test]
fn native_bc_family_upload_roundtrips_exact_blocks() {
    let mut g = exec();
    for (index, &format) in hl_gpu::protocol::model::capability::BC_FORMATS
        .iter()
        .enumerate()
    {
        if !g.capabilities().supports_format(format) {
            continue;
        }
        let block_bytes = format.block_geometry().unwrap().2 as usize;
        let block = (0..block_bytes)
            .map(|byte| (byte as u8).wrapping_add(index as u8))
            .collect::<Vec<_>>();
        let s = run_batch(
            &mut g,
            &[
                Cmd::CreateTexture(
                    1,
                    TextureDesc {
                        width: 4,
                        height: 4,
                        depth: 1,
                        mip_levels: 1,
                        sample_count: 1,
                        dim: TextureDim::D2,
                        format,
                        usage: texture_usage::SAMPLED
                            | texture_usage::COPY_SRC
                            | texture_usage::COPY_DST,
                        label: format!("{format:?}"),
                    },
                ),
                Cmd::CreateBuffer(1, buf(block_bytes as u64, buffer_usage::COPY_SRC)),
                Cmd::WriteBuffer {
                    id: 1,
                    offset: 0,
                    data: block.clone(),
                },
                Cmd::Submit(CommandBuffer {
                    encoder: vec![Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: block_bytes as u32,
                        dst: 1,
                        mip: 0,
                        width: 4,
                        height: 4,
                    }],
                    signal: None,
                }),
            ],
        );
        assert_eq!(
            g.read_texture(&s.resources, 1).unwrap(),
            block,
            "{format:?}"
        );
    }
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

/// Vulkan WSI's basic render cases declare a push-constant block in the vertex shader. The executor
/// builds an explicit render pipeline layout, so that layout must carry a matching vertex-stage range;
/// otherwise wgpu rejects pipeline creation before the case can reach presentation.
#[test]
fn spirv_render_pipeline_with_vertex_push_constant_creates() {
    let seed = r#"
        var<push_constant> pc: vec4<f32>;

        @vertex
        fn vs_main(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
            var positions = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0),
                vec2<f32>(3.0, -1.0),
                vec2<f32>(-1.0, 3.0));
            return vec4<f32>(positions[vertex] + pc.xy, 0.0, 1.0);
        }

        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(0.0, 1.0, 0.0, 1.0);
        }
    "#;
    let spirv = wgsl_to_spirv(seed);
    let mut g = exec();
    let result = try_batch(
        &mut g,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vs_main".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 1,
                        entry: "fs_main".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xf,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
        ],
    );
    assert!(
        result.is_ok(),
        "a SPIR-V render pipeline with a vertex push constant must create: {:?}",
        result.err()
    );
}

#[test]
fn external_spirv_confines_out_of_bounds_uniform_and_storage_access() {
    let seed = r#"
        @group(0) @binding(0) var<uniform> uniform_values: array<vec4<u32>, 2>;
        @group(0) @binding(1) var<storage, read_write> storage_values: array<u32>;
        @group(0) @binding(2) var<storage, read_write> results: array<u32>;

        @compute @workgroup_size(1)
        fn cs_main() {
            let index = results[0];
            results[0] = uniform_values[index].x;
            results[1] = storage_values[index];
            storage_values[index] = 77u;
            results[2] = 123u;
        }
    "#;
    let spirv = wgsl_to_spirv(seed);
    let mut g = exec();
    assert_ne!(
        g.capabilities().gpu_features
            & hl_gpu::protocol::model::capability::gpu_feature::ROBUST_BUFFER_ACCESS,
        0
    );
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
            Cmd::CreateBuffer(1, buf(32, buffer_usage::UNIFORM)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: u32s(&[11, 0, 0, 0, 22, 0, 0, 0]),
            },
            Cmd::CreateBuffer(2, buf(8, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 2,
                offset: 0,
                data: u32s(&[31, 32]),
            },
            Cmd::CreateBuffer(3, buf(16, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 3,
                offset: 0,
                data: u32s(&[99, 0xdead_beef, 0, 0xcafe_babe]),
            },
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Buffer {
                                id: 1,
                                offset: 0,
                                size: 32,
                            },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: 8,
                            },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Buffer {
                                id: 3,
                                offset: 0,
                                size: 16,
                            },
                        },
                    ],
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
    let results = read_u32s(&g, &s, 3, 4);
    assert!(
        [0, 11, 22].contains(&results[0]),
        "OOB uniform read escaped its bound resource: {results:?}"
    );
    assert!(
        [0, 31, 32].contains(&results[1]),
        "OOB storage read escaped its bound resource: {results:?}"
    );
    assert_eq!(results[2], 123, "OOB access must not lose the invocation");
    assert_eq!(
        results[3], 0xcafe_babe,
        "OOB storage write crossed into another binding"
    );
    let storage = read_u32s(&g, &s, 2, 2);
    assert!(
        matches!(storage.as_slice(), [31, 32] | [77, 32] | [31, 77]),
        "robust storage write must remain within its bound resource: {storage:?}"
    );
}

#[test]
fn external_spirv_fragment_ssbo_atomic_renders_and_reads_back() {
    let seed = r#"
        @group(0) @binding(0) var<storage, read_write> hits: atomic<u32>;

        @vertex
        fn vs_main(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
            var positions = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0),
                vec2<f32>(3.0, -1.0),
                vec2<f32>(-1.0, 3.0));
            return vec4<f32>(positions[vertex], 0.0, 1.0);
        }

        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            _ = atomicAdd(&hits, 1u);
            return vec4<f32>(0.0, 1.0, 0.0, 1.0);
        }
    "#;
    let spirv = wgsl_to_spirv(seed);
    let mut g = exec();
    if g.capabilities().gpu_features
        & hl_gpu::protocol::model::capability::gpu_feature::FRAGMENT_STORES_ATOMICS
        == 0
    {
        return;
    }
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                TextureDesc {
                    width: 1,
                    height: 1,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim: TextureDim::D2,
                    format: TextureFormat::Rgba8Unorm,
                    usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(1, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vs_main".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 1,
                        entry: "fs_main".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xf,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
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
                            size: 4,
                        },
                    }],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    );
    assert_eq!(read_u32s(&g, &s, 1, 1), vec![1]);
    assert_eq!(
        g.read_texture(&s.resources, 1).unwrap(),
        vec![0, 255, 0, 255]
    );
}

#[test]
fn external_spirv_sample_shading_runs_once_per_enabled_sample() {
    let seed = r#"
        @group(0) @binding(0) var<storage, read_write> hits: atomic<u32>;

        @vertex
        fn vs_main(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
            var positions = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0),
                vec2<f32>(3.0, -1.0),
                vec2<f32>(-1.0, 3.0));
            return vec4<f32>(positions[vertex], 0.0, 1.0);
        }

        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            _ = atomicAdd(&hits, 1u);
            return vec4<f32>(1.0, 0.0, 0.0, 1.0);
        }
    "#;
    let spirv = wgsl_to_spirv(seed);
    let mut g = exec();
    let required = hl_gpu::protocol::model::capability::gpu_feature::FRAGMENT_STORES_ATOMICS
        | hl_gpu::protocol::model::capability::gpu_feature::SAMPLE_RATE_SHADING;
    if g.capabilities().gpu_features & required != required {
        return;
    }
    let pipeline = RenderPipelineDesc {
        vertex: ShaderRef {
            module: 1,
            entry: "vs_main".into(),
        },
        fragment: Some(ShaderRef {
            module: 1,
            entry: "fs_main".into(),
        }),
        vertex_buffers: vec![],
        color_targets: vec![ColorTargetState {
            format: TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask: 0xf,
        }],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        sample_count: 4,
        label: String::new(),
    };
    let outcome = try_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                TextureDesc {
                    width: 1,
                    height: 1,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 4,
                    dim: TextureDim::D2,
                    format: TextureFormat::Rgba8Unorm,
                    usage: texture_usage::RENDER_TARGET,
                    label: String::new(),
                },
            ),
            Cmd::CreateTexture(
                2,
                TextureDesc {
                    width: 1,
                    height: 1,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim: TextureDim::D2,
                    format: TextureFormat::Rgba8Unorm,
                    usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(1, buf(4, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateRenderPipelineLayout(
                1,
                pipeline,
                PipelineLayout {
                    bindings: vec![PipelineBinding {
                        group: 0,
                        binding: 0,
                        count: 1,
                        kind: PipelineBindingKind::StorageBuffer,
                    }],
                },
                RenderMultisample {
                    mask: 0b0101,
                    sample_shading: true,
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
                            size: 4,
                        },
                    }],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                    Enc::ResolveTexture {
                        src: 1,
                        src_sub: TextureSubresource::base(),
                        src_origin: Origin3d::default(),
                        dst: 2,
                        dst_sub: TextureSubresource::base(),
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                    },
                ],
                signal: None,
            }),
        ],
    );
    // A backend that cannot program a raster sample mask (wgpu's Metal HAL) must REFUSE the mask rather
    // than shade every sample and return a wrong pixel.
    let s = match outcome {
        Ok(session) => session,
        Err(error) => {
            assert!(
                matches!(
                    error,
                    hl_gpu::GpuError::Unsupported(
                        "wgpu: this backend does not honour a multisample sample mask"
                    )
                ),
                "an unhonourable sample mask must be an explicit typed refusal, got {error:?}"
            );
            return;
        }
    };
    // Forced per-sample shading is a LOWER bound: full rate is at least the requested minimum.
    assert!(
        read_u32s(&g, &s, 1, 1)[0] >= 2,
        "the injected SampleIndex input must force at least one fragment invocation per enabled sample"
    );
    assert_eq!(
        g.read_texture(&s.resources, 2).unwrap(),
        vec![128, 0, 0, 255],
        "sample mask 0b0101 must resolve two red and two clear samples"
    );
}

#[test]
fn external_spirv_independent_targets_keep_distinct_blend_and_masks() {
    let seed = r#"
        struct Outputs {
            @location(0) first: vec4<f32>,
            @location(1) second: vec4<f32>,
        }

        @vertex
        fn vs_main(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
            var positions = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0),
                vec2<f32>(3.0, -1.0),
                vec2<f32>(-1.0, 3.0));
            return vec4<f32>(positions[vertex], 0.0, 1.0);
        }

        @fragment
        fn fs_main() -> Outputs {
            return Outputs(
                vec4<f32>(1.0, 0.0, 0.0, 0.5),
                vec4<f32>(0.0, 1.0, 0.0, 1.0));
        }
    "#;
    let spirv = wgsl_to_spirv(seed);
    let mut g = exec();
    if g.capabilities().gpu_features
        & hl_gpu::protocol::model::capability::gpu_feature::INDEPENDENT_BLEND
        == 0
    {
        return;
    }
    let texture = |id| {
        Cmd::CreateTexture(
            id,
            TextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                label: String::new(),
            },
        )
    };
    let s = run_batch(
        &mut g,
        &[
            texture(1),
            texture(2),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vs_main".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 1,
                        entry: "fs_main".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![
                        ColorTargetState {
                            format: TextureFormat::Rgba8Unorm,
                            blend: Some(BlendState {
                                src_color: 4,
                                dst_color: 5,
                                op_color: 0,
                                src_alpha: 1,
                                dst_alpha: 0,
                                op_alpha: 0,
                            }),
                            write_mask: 0xf,
                        },
                        ColorTargetState {
                            format: TextureFormat::Rgba8Unorm,
                            blend: None,
                            write_mask: 0x1,
                        },
                    ],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![
                            ColorAttachment {
                                texture: 1,
                                load: LoadOp::Clear,
                                clear: [0.0, 0.0, 1.0, 1.0],
                                store: true,
                            },
                            ColorAttachment {
                                texture: 2,
                                load: LoadOp::Clear,
                                clear: [0.0, 0.0, 1.0, 1.0],
                                store: true,
                            },
                        ],
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
                ],
                signal: None,
            }),
        ],
    );
    // The blue channel blends to exactly 0.5 of the 255 clear, and the fixed-function blender's rounding of
    // that half is backend-defined (Vulkan truncates to 127, Metal rounds to 128). Both are correct for this
    // IR, so allow one LSB there; every other channel is exact.
    let blended = g.read_texture(&s.resources, 1).unwrap();
    assert_eq!([blended[0], blended[1], blended[3]], [128, 0, 128]);
    assert!(
        blended[2] == 127 || blended[2] == 128,
        "independent blend must halve the blue clear (got {})",
        blended[2]
    );
    assert_eq!(
        g.read_texture(&s.resources, 2).unwrap(),
        vec![0, 0, 255, 255]
    );
}

#[test]
fn external_spirv_depth_bias_clamp_limits_a_large_positive_bias() {
    let seed = r#"
        @vertex
        fn vs_main(@builtin(vertex_index) vertex: u32) -> @builtin(position) vec4<f32> {
            var positions = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0),
                vec2<f32>(3.0, -1.0),
                vec2<f32>(-1.0, 3.0));
            return vec4<f32>(positions[vertex], 0.4, 1.0);
        }

        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(0.0, 1.0, 0.0, 1.0);
        }
    "#;
    let spirv = wgsl_to_spirv(seed);
    let mut g = exec();
    if g.capabilities().gpu_features
        & hl_gpu::protocol::model::capability::gpu_feature::DEPTH_BIAS_CLAMP
        == 0
    {
        return;
    }
    let color = |id| {
        Cmd::CreateTexture(
            id,
            TextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Rgba8Unorm,
                usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                label: String::new(),
            },
        )
    };
    let depth = |id| {
        Cmd::CreateTexture(
            id,
            TextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Depth32Float,
                usage: texture_usage::RENDER_TARGET,
                label: String::new(),
            },
        )
    };
    let pipeline = |id, clamp| {
        let mut depth =
            DepthState::depth_only(TextureFormat::Depth32Float, false, compare::GREATER);
        depth.bias_constant = i32::MAX;
        depth.bias_clamp = clamp;
        Cmd::CreateRenderPipeline(
            id,
            RenderPipelineDesc {
                vertex: ShaderRef {
                    module: 1,
                    entry: "vs_main".into(),
                },
                fragment: Some(ShaderRef {
                    module: 1,
                    entry: "fs_main".into(),
                }),
                vertex_buffers: vec![],
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xf,
                }],
                depth: Some(depth),
                topology: Topology::TriangleList,
                cull: 0,
                front_face: 0,
                sample_count: 1,
                label: String::new(),
            },
        )
    };
    let pass = |target, depth, pipeline| {
        vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: target,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 1.0, 1.0],
                    store: true,
                }],
                depth: Some(DepthAttachment {
                    texture: depth,
                    load: LoadOp::Clear,
                    clear_depth: 0.5,
                    clear_stencil: 0,
                }),
            },
            Enc::SetPipeline(pipeline),
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ]
    };
    let mut encoder = pass(1, 3, 1);
    encoder.extend(pass(2, 4, 2));
    let s = run_batch(
        &mut g,
        &[
            color(1),
            color(2),
            depth(3),
            depth(4),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            pipeline(1, 0.01),
            pipeline(2, 0.0),
            Cmd::Submit(CommandBuffer {
                encoder,
                signal: None,
            }),
        ],
    );
    assert_eq!(
        g.read_texture(&s.resources, 1).unwrap(),
        vec![0, 0, 255, 255],
        "clamping the huge positive bias to 0.01 must keep z=0.4 behind z=0.5"
    );
    assert_eq!(
        g.read_texture(&s.resources, 2).unwrap(),
        vec![0, 255, 0, 255],
        "without a clamp the same huge positive bias must pass"
    );
}

#[test]
fn external_spirv_samples_second_cube_from_a_cube_array() {
    let seed = r#"
        @group(0) @binding(0) var image: texture_cube<f32>;
        @group(0) @binding(1) var image_sampler: sampler;
        @group(0) @binding(2) var<storage, read_write> output: array<u32>;

        @compute @workgroup_size(1)
        fn cs_main() {
            let color = textureSampleLevel(
                image, image_sampler, vec3<f32>(1.0, 0.0, 0.0), 0.0);
            output[0] = u32(color.r * 255.0);
            output[1] = u32(color.g * 255.0);
            output[2] = u32(color.b * 255.0);
            output[3] = u32(color.a * 255.0);
        }
    "#;
    let spirv = wgsl_to_spirv(seed);
    let mut g = exec();
    if g.capabilities().gpu_features
        & hl_gpu::protocol::model::capability::gpu_feature::IMAGE_CUBE_ARRAY
        == 0
    {
        return;
    }
    let face = vec![17, 93, 201, 255];
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
            Cmd::CreateTexture(
                1,
                TextureDesc {
                    width: 2,
                    height: 2,
                    depth: 12,
                    mip_levels: 2,
                    sample_count: 1,
                    dim: TextureDim::Cube,
                    format: TextureFormat::Rgba8Unorm,
                    usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
                    label: String::new(),
                },
            ),
            Cmd::CreateSampler(
                1,
                SamplerDesc {
                    min_filter: Filter::Nearest,
                    mag_filter: Filter::Nearest,
                    mip_filter: Filter::Nearest,
                    address_u: AddressMode::ClampToEdge,
                    address_v: AddressMode::ClampToEdge,
                    address_w: AddressMode::ClampToEdge,
                    ..SamplerDesc::default()
                },
            ),
            Cmd::CreateTextureView(
                2,
                TextureViewDesc {
                    texture: 1,
                    dim: TextureDim::Cube,
                    format: TextureFormat::Rgba8Unorm,
                    aspect: hl_gpu::protocol::model::enums::TextureAspect::All,
                    base_mip: 1,
                    mip_count: 1,
                    base_layer: 6,
                    layer_count: 6,
                },
            ),
            Cmd::CreateBuffer(1, buf(4, buffer_usage::COPY_SRC)),
            Cmd::CreateBuffer(2, buf(16, buffer_usage::STORAGE | buffer_usage::COPY_SRC)),
            Cmd::CreateBuffer(3, buf(4, buffer_usage::COPY_DST | buffer_usage::COPY_SRC)),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: face,
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTextureRegion {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 4,
                        rows_per_image: 1,
                        dst: 1,
                        dst_sub: TextureSubresource {
                            mip: 1,
                            layer: 6,
                            aspect: hl_gpu::protocol::model::enums::TextureAspect::All,
                        },
                        dst_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                    },
                    Enc::CopyTextureToBufferRegion {
                        src: 1,
                        src_sub: TextureSubresource {
                            mip: 1,
                            layer: 6,
                            aspect: hl_gpu::protocol::model::enums::TextureAspect::All,
                        },
                        src_origin: Origin3d::default(),
                        extent: Extent3d {
                            width: 1,
                            height: 1,
                            depth: 1,
                        },
                        dst: 3,
                        dst_offset: 0,
                        bytes_per_row: 4,
                        rows_per_image: 1,
                    },
                ],
                signal: None,
            }),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Sampler { id: 1 },
                        },
                        BindEntry {
                            binding: 2,
                            resource: BindResource::Buffer {
                                id: 2,
                                offset: 0,
                                size: 16,
                            },
                        },
                    ],
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
            Cmd::DestroyBindGroup(1),
            Cmd::DestroyTextureView(2),
        ],
    );
    assert_eq!(read_u32s(&g, &s, 2, 4), vec![17, 93, 201, 255]);
    assert_eq!(
        g.read_buffer(&s.resources, BufferId(3), 0, 4).unwrap(),
        vec![17, 93, 201, 255]
    );
}
