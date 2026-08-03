//! Formatted Vulkan buffer views through the real executor. The glslang 16.2 SPIR-V deliberately retains
//! `OpTypeImage DimBuffer`, exercising Naga lowering, native dispatch and direct packed stores.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    ComputePipelineDesc, PipelineBinding, PipelineBindingKind, PipelineLayout, RenderMultisample,
    RenderPipelineDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

#[test]
fn native_device_accepts_overlapping_writable_storage_bindings() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
            .expect("wgpu adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("texel writable alias probe"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .expect("wgpu device");
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("writable alias probe"),
        source: wgpu::ShaderSource::Wgsl(
            "@group(0) @binding(0) var<storage, read_write> a: array<u32>;
             @group(0) @binding(1) var<storage, read_write> b: array<u32>;
             @compute @workgroup_size(1) fn main() { a[0] = 1u; b[0] = b[0] + 1u; }"
                .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("writable alias probe"),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aliased writable storage"),
        size: 4,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("alias result"),
        size: 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let layout = pipeline.get_bind_group_layout(0);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("overlapping writable aliases"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: buffer.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&buffer, 0, &staging, 0, 4);
    queue.submit([encoder.finish()]);
    device.poll(wgpu::Maintain::Wait);
    let validation = pollster::block_on(device.pop_error_scope());
    assert!(
        validation.is_none(),
        "overlapping writable bindings were refused: {validation:?}"
    );
    let slice = staging.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).unwrap()
    });
    device.poll(wgpu::Maintain::Wait);
    receiver.recv().unwrap().unwrap();
    assert_eq!(&*slice.get_mapped_range(), &2u32.to_le_bytes());
}

const UNIFORM: &[u32] = &[
    119734787, 65536, 524299, 27, 0, 131089, 1, 131089, 46, 393227, 1, 1280527431, 1685353262,
    808793134, 0, 196622, 0, 1, 327695, 5, 4, 1852399981, 0, 393232, 4, 17, 1, 1, 1, 196611, 2,
    450, 262149, 4, 1852399981, 0, 262149, 9, 1886680399, 29813, 327686, 9, 0, 1970037110, 29541,
    393221, 11, 1886680431, 1985967221, 1702194273, 115, 393221, 17, 1970302569, 1635147636,
    1936029036, 0, 262215, 8, 6, 16, 196679, 9, 3, 327752, 9, 0, 35, 0, 262215, 11, 33, 1, 262215,
    11, 34, 0, 262215, 17, 33, 0, 262215, 17, 34, 0, 262215, 26, 11, 25, 131091, 2, 196641, 3, 2,
    196630, 6, 32, 262167, 7, 6, 4, 196637, 8, 7, 196638, 9, 8, 262176, 10, 2, 9, 262203, 10, 11,
    2, 262165, 12, 32, 1, 262187, 12, 13, 0, 589849, 14, 6, 5, 0, 0, 0, 1, 0, 196635, 15, 14,
    262176, 16, 0, 15, 262203, 16, 17, 0, 262176, 21, 2, 7, 262165, 23, 32, 0, 262167, 24, 23, 3,
    262187, 23, 25, 1, 393260, 24, 26, 25, 25, 25, 327734, 2, 4, 0, 3, 131320, 5, 262205, 15, 18,
    17, 262244, 14, 19, 18, 327775, 7, 20, 19, 13, 393281, 21, 22, 11, 13, 13, 196670, 22, 20,
    65789, 65592,
];
const STORAGE: &[u32] = &[
    119734787, 65536, 524299, 23, 0, 131089, 1, 131089, 47, 393227, 1, 1280527431, 1685353262,
    808793134, 0, 196622, 0, 1, 327695, 5, 4, 1852399981, 0, 393232, 4, 17, 1, 1, 1, 196611, 2,
    450, 262149, 4, 1852399981, 0, 262149, 9, 1970037110, 29541, 262215, 9, 33, 0, 262215, 9, 34,
    0, 262215, 22, 11, 25, 131091, 2, 196641, 3, 2, 196630, 6, 32, 589849, 7, 6, 5, 0, 0, 0, 2, 4,
    262176, 8, 0, 7, 262203, 8, 9, 0, 262165, 11, 32, 1, 262187, 11, 12, 0, 262167, 14, 6, 4,
    262187, 6, 16, 1048576000, 458796, 14, 17, 16, 16, 16, 16, 262165, 19, 32, 0, 262167, 20, 19,
    3, 262187, 19, 21, 1, 393260, 20, 22, 21, 21, 21, 327734, 2, 4, 0, 3, 131320, 5, 262205, 7, 10,
    9, 262205, 7, 13, 9, 327778, 14, 15, 13, 12, 327809, 14, 18, 15, 17, 262243, 10, 12, 18, 65789,
    65592,
];
const RENDER_VERTEX: &[u32] = &[
    119734787, 65536, 524299, 28, 0, 131089, 1, 131089, 46, 393227, 1, 1280527431, 1685353262,
    808793134, 0, 196622, 0, 1, 458767, 0, 4, 1852399981, 0, 13, 22, 196611, 2, 450, 262149, 4,
    1852399981, 0, 393221, 11, 1348430951, 1700164197, 2019914866, 0, 393222, 11, 0, 1348430951,
    1953067887, 7237481, 458758, 11, 1, 1348430951, 1953393007, 1702521171, 0, 458758, 11, 2,
    1130327143, 1148217708, 1635021673, 6644590, 458758, 11, 3, 1130327143, 1147956341, 1635021673,
    6644590, 196613, 13, 0, 327685, 19, 1769172848, 1852795252, 115, 393221, 22, 1449094247,
    1702130277, 1684949368, 30821, 196679, 11, 2, 327752, 11, 0, 11, 0, 327752, 11, 1, 11, 1,
    327752, 11, 2, 11, 3, 327752, 11, 3, 11, 4, 262215, 19, 33, 0, 262215, 19, 34, 0, 262215, 22,
    11, 42, 131091, 2, 196641, 3, 2, 196630, 6, 32, 262167, 7, 6, 4, 262165, 8, 32, 0, 262187, 8,
    9, 1, 262172, 10, 6, 9, 393246, 11, 7, 6, 10, 10, 262176, 12, 3, 11, 262203, 12, 13, 3, 262165,
    14, 32, 1, 262187, 14, 15, 0, 589849, 16, 6, 5, 0, 0, 0, 1, 0, 196635, 17, 16, 262176, 18, 0,
    17, 262203, 18, 19, 0, 262176, 21, 1, 14, 262203, 21, 22, 1, 262176, 26, 3, 7, 327734, 2, 4, 0,
    3, 131320, 5, 262205, 17, 20, 19, 262205, 14, 23, 22, 262244, 16, 24, 20, 327775, 7, 25, 24,
    23, 327745, 26, 27, 13, 15, 196670, 27, 25, 65789, 65592,
];
const RENDER_FRAGMENT: &[u32] = &[119734787,65536,524299,17,0,131089,1,131089,47,393227,1,1280527431,1685353262,808793134,0,196622,0,1,393231,4,4,1852399981,0,9,196624,4,7,196611,2,450,262149,4,1852399981,0,196613,9,111,262149,12,1869377379,29554,262215,9,30,0,196679,12,24,262215,12,33,1,262215,12,34,0,131091,2,196641,3,2,196630,6,32,262167,7,6,4,262176,8,3,7,262203,8,9,3,589849,10,6,5,0,0,0,2,4,262176,11,0,10,262203,11,12,0,262165,14,32,1,262187,14,15,0,327734,2,4,0,3,131320,5,262205,10,13,12,327778,7,16,13,15,196670,9,16,65789,65592,];

fn buffer(size: u64) -> BufferDesc {
    BufferDesc {
        size,
        usage: buffer_usage::STORAGE | buffer_usage::COPY_SRC | buffer_usage::COPY_DST,
        label: String::new(),
    }
}

fn run(commands: &[Cmd]) -> (WgpuExecutor, Session) {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("wgpu adapter");
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut session, &mut executor, 0, commands).expect("texel program");
    (executor, session)
}

fn uniform_texel_load(format: TextureFormat, bytes: Vec<u8>) -> Vec<u8> {
    let commands = vec![
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv: UNIFORM.to_vec() },
        Cmd::CreateComputePipelineLayout(1,
            ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "main".into() }, label: String::new() },
            PipelineLayout { bindings: vec![
                PipelineBinding { group: 0, binding: 0, count: 1, kind: PipelineBindingKind::UniformTexelBuffer },
                PipelineBinding { group: 0, binding: 1, count: 1, kind: PipelineBindingKind::StorageBuffer },
            ] },
        ),
        Cmd::CreateBuffer(1, buffer(bytes.len() as u64)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: bytes.clone() },
        Cmd::CreateBuffer(2, buffer(16)),
        Cmd::CreateBindGroup(1, BindGroupDesc { set: 0, entries: vec![
            BindEntry { binding: 0, resource: BindResource::TexelBuffer {
                id: 1, offset: 0, size: bytes.len() as u64, format, writable: false,
            } },
            BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: 16 } },
        ] }),
        Cmd::Submit(CommandBuffer { encoder: vec![
            Enc::BeginComputePass, Enc::SetPipeline(1), Enc::SetBindGroup { index: 0, group: 1 },
            Enc::Dispatch { x: 1, y: 1, z: 1 }, Enc::EndComputePass,
        ], signal: None }),
    ];
    let (executor, session) = run(&commands);
    executor.read_buffer(&session.resources, BufferId(2), 0, 16).unwrap()
}

#[test]
fn vulkan_native_formats_uniform_texel_load_exact_values() {
    let f32s = |bytes: Vec<u8>| bytes.chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap())).collect::<Vec<_>>();
    let rg8 = f32s(uniform_texel_load(TextureFormat::Rg8Snorm, vec![0x40, 0xc0]));
    assert!((rg8[0] - 64.0 / 127.0).abs() < 1e-6 && (rg8[1] + 64.0 / 127.0).abs() < 1e-6 && rg8[2..] == [0.0, 1.0]);
    let rgba8 = f32s(uniform_texel_load(TextureFormat::Rgba8Snorm, vec![0x20, 0x40, 0x60, 0x7f]));
    assert!((rgba8[0] - 32.0 / 127.0).abs() < 1e-6 && rgba8[3] == 1.0);
    let rg16f = f32s(uniform_texel_load(TextureFormat::Rg16Float, vec![0x00, 0x38, 0x00, 0xb4]));
    assert_eq!(rg16f, [0.5, -0.25, 0.0, 1.0]);
}

fn pipeline(shader: &[u32], kind: PipelineBindingKind, extra_output: bool) -> Vec<Cmd> {
    let mut bindings = vec![PipelineBinding {
        group: 0,
        binding: 0,
        count: 1,
        kind,
    }];
    if extra_output {
        bindings.push(PipelineBinding {
            group: 0,
            binding: 1,
            count: 1,
            kind: PipelineBindingKind::StorageBuffer,
        });
    }
    vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv: shader.to_vec(),
        },
        Cmd::CreateComputePipelineLayout(
            1,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: 1,
                    entry: "main".into(),
                },
                label: String::new(),
            },
            PipelineLayout { bindings },
        ),
    ]
}

fn atomic_texel_spirv(format: &str, signed: bool) -> Vec<u32> {
    let suffix = if signed { "i" } else { "u" };
    let minimum = if signed { "-3i" } else { "3u" };
    let source = format!(
        "@group(0) @binding(0) var image: texture_storage_2d<{format}, atomic>;
         @compute @workgroup_size(1) fn main() {{
             let a = textureAtomicAdd(image, vec2<i32>(0, 0), 7{suffix});
             textureAtomicExchange(image, vec2<i32>(1, 0), a);
             let b = textureAtomicMin(image, vec2<i32>(0, 0), {minimum});
             textureAtomicExchange(image, vec2<i32>(2, 0), b);
             let c = textureAtomicMax(image, vec2<i32>(0, 0), 11{suffix});
             textureAtomicExchange(image, vec2<i32>(3, 0), c);
             let d = textureAtomicCompareExchangeWeak(image, vec2<i32>(0, 0), 11{suffix}, 13{suffix});
             textureAtomicExchange(image, vec2<i32>(4, 0), d);
         }}"
    );
    let mut module = naga::front::wgsl::parse_str(&source).expect("atomic texel seed parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("atomic texel seed validates");
    let images = module.global_variables.iter().filter_map(|(handle, variable)| {
        matches!(module.types[variable.ty].inner, naga::TypeInner::Image { .. })
            .then_some((handle, variable.ty))
    }).collect::<Vec<_>>();
    for (global, old_ty) in images {
        let mut ty = module.types[old_ty].clone();
        let naga::TypeInner::Image { ref mut dim, .. } = ty.inner else { unreachable!() };
        *dim = naga::ImageDimension::Buffer;
        module.global_variables[global].ty = module.types.insert(ty, naga::Span::default());
    }
    let function = &mut module.entry_points[0].function;
    let coordinates = function
        .body
        .iter()
        .filter_map(|statement| match statement {
            naga::Statement::ImageAtomic { coordinate, .. } => {
                let naga::Expression::Compose { components, .. } = &function.expressions[*coordinate]
                    else { return None };
                Some((*coordinate, components[0]))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for statement in function.body.iter_mut() {
        if let naga::Statement::ImageAtomic { coordinate, .. } = statement {
            if let Some((_, scalar)) = coordinates.iter().find(|(vector, _)| vector == coordinate) {
                *coordinate = *scalar;
            }
        }
    }
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("atomic texel seed emits SPIR-V")
}

fn atomic_texel_case(format: TextureFormat, signed: bool) -> Vec<u8> {
    let shader = atomic_texel_spirv(
        if signed { "r32sint" } else { "r32uint" },
        signed,
    );
    let mut commands = pipeline(&shader, PipelineBindingKind::StorageTexelBuffer, false);
    commands.extend([
        Cmd::CreateBuffer(1, buffer(20)),
        Cmd::WriteBuffer { id: 1, offset: 0, data: [5u32, 0, 0, 0, 0].into_iter().flat_map(u32::to_le_bytes).collect() },
        Cmd::CreateBindGroup(1, BindGroupDesc {
            set: 0,
            entries: vec![
                BindEntry { binding: 0, resource: BindResource::TexelBuffer {
                    id: 1, offset: 0, size: 20, format, writable: true,
                }},
            ],
        }),
        Cmd::Submit(CommandBuffer { encoder: vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(1),
            Enc::SetBindGroup { index: 0, group: 1 },
            Enc::Dispatch { x: 1, y: 1, z: 1 },
            Enc::EndComputePass,
        ], signal: None }),
    ]);
    let (executor, session) = run(&commands);
    executor.read_buffer(&session.resources, BufferId(1), 0, 20).unwrap()
}

#[test]
fn r32_storage_texel_atomics_return_old_values_with_signed_ordering() {
    let uint = atomic_texel_case(TextureFormat::R32Uint, false);
    assert_eq!(
        uint,
        [13u32, 5, 12, 3, 11].into_iter().flat_map(u32::to_le_bytes).collect::<Vec<_>>()
    );

    let sint = atomic_texel_case(TextureFormat::R32Sint, true);
    assert_eq!(
        sint,
        [13i32, 5, 12, -3, 11].into_iter().flat_map(i32::to_le_bytes).collect::<Vec<_>>()
    );
}

#[test]
fn uniform_texel_load_executes_and_writes_exact_vec4() {
    let mut commands = pipeline(UNIFORM, PipelineBindingKind::UniformTexelBuffer, true);
    commands.extend([
        Cmd::CreateBuffer(1, buffer(4)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: [64u8, 128, 255, 32].to_vec(),
        },
        Cmd::CreateBuffer(2, buffer(16)),
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry {
                        binding: 0,
                        resource: BindResource::TexelBuffer {
                            id: 1,
                            offset: 0,
                            size: 4,
                            format: TextureFormat::Rgba8Unorm,
                            writable: false,
                        },
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
    ]);
    let (executor, session) = run(&commands);
    let bytes = executor
        .read_buffer(&session.resources, BufferId(2), 0, 16)
        .unwrap();
    let got = bytes
        .chunks_exact(4)
        .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
        .collect::<Vec<_>>();
    assert_eq!(got, vec![64.0 / 255.0, 128.0 / 255.0, 1.0, 32.0 / 255.0]);
}

#[test]
fn storage_texel_writeback_and_two_dispatches_are_ordered() {
    let mut commands = pipeline(STORAGE, PipelineBindingKind::StorageTexelBuffer, false);
    commands.extend([
        Cmd::CreateBuffer(1, buffer(4)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vec![0, 64, 128, 128],
        },
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![BindEntry {
                    binding: 0,
                    resource: BindResource::TexelBuffer {
                        id: 1,
                        offset: 0,
                        size: 4,
                        format: TextureFormat::Rgba8Unorm,
                        writable: true,
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
                Enc::Dispatch { x: 1, y: 1, z: 1 },
                Enc::EndComputePass,
            ],
            signal: None,
        }),
    ]);
    let (executor, session) = run(&commands);
    assert_eq!(
        executor
            .read_buffer(&session.resources, BufferId(1), 0, 4)
            .unwrap(),
        // Every imageStore quantizes to the bound RGBA8 UNORM representation. An exact-rational control
        // gives [64,128,192,192] after dispatch one, then [128,192,255,255] after dispatch two. The old
        // expanded-f32 shadow incorrectly postponed quantization until pass end and produced green 191.
        vec![128, 192, 255, 255]
    );
}

#[test]
fn packed_r8_store_preserves_adjacent_bytes() {
    let mut commands = pipeline(STORAGE, PipelineBindingKind::StorageTexelBuffer, false);
    commands.extend([
        Cmd::CreateBuffer(1, buffer(4)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vec![64, 11, 22, 33],
        },
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![BindEntry {
                    binding: 0,
                    resource: BindResource::TexelBuffer {
                        id: 1,
                        offset: 0,
                        size: 4,
                        format: TextureFormat::R8Unorm,
                        writable: true,
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
                Enc::Dispatch { x: 1, y: 1, z: 1 },
                Enc::EndComputePass,
            ],
            signal: None,
        }),
    ]);
    let (executor, session) = run(&commands);
    assert_eq!(
        executor
            .read_buffer(&session.resources, BufferId(1), 0, 4)
            .unwrap(),
        vec![192, 11, 22, 33]
    );
}

#[test]
fn vertex_stage_uniform_texel_buffer_renders_on_the_native_device() {
    let positions = [
        -1.0f32, -1.0, 0.0, 1.0, 3.0, -1.0, 0.0, 1.0, -1.0, 3.0, 0.0, 1.0,
    ]
    .into_iter()
    .flat_map(f32::to_le_bytes)
    .collect::<Vec<_>>();
    let pipeline = RenderPipelineDesc {
        vertex: ShaderRef {
            module: 1,
            entry: "main".into(),
        },
        fragment: Some(ShaderRef {
            module: 2,
            entry: "main".into(),
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
    };
    let commands = vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv: RENDER_VERTEX.to_vec(),
        },
        Cmd::CreateShader {
            id: 2,
            kind: ShaderPayloadKind::SpirV,
            spirv: RENDER_FRAGMENT.to_vec(),
        },
        Cmd::CreateRenderPipelineLayout(
            1,
            pipeline,
            PipelineLayout {
                bindings: vec![
                    PipelineBinding {
                        group: 0,
                        binding: 0,
                        count: 1,
                        kind: PipelineBindingKind::UniformTexelBuffer,
                    },
                    PipelineBinding {
                        group: 0,
                        binding: 1,
                        count: 1,
                        kind: PipelineBindingKind::UniformTexelBuffer,
                    },
                ],
            },
            RenderMultisample::default(),
        ),
        Cmd::CreateBuffer(1, buffer(48)),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: positions,
        },
        Cmd::CreateBuffer(2, buffer(4)),
        Cmd::WriteBuffer { id: 2, offset: 0, data: vec![0, 255, 0, 255] },
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
                usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                label: String::new(),
            },
        ),
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![
                    BindEntry {
                        binding: 0,
                        resource: BindResource::TexelBuffer {
                            id: 1,
                            offset: 0,
                            size: 48,
                            format: TextureFormat::Rgba32Float,
                            writable: false,
                        },
                    },
                    BindEntry {
                        binding: 1,
                        resource: BindResource::TexelBuffer {
                            id: 2,
                            offset: 0,
                            size: 4,
                            format: TextureFormat::Rgba8Unorm,
                            writable: false,
                        },
                    },
                ],
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
    ];
    let (executor, session) = run(&commands);
    assert_eq!(
        executor.read_texture(&session.resources, 1).unwrap(),
        vec![0, 255, 0, 255].repeat(4)
    );
}
