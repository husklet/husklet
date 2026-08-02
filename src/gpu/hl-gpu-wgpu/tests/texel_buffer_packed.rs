//! Packed sub-word texel-buffer stores through the real executor.

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ComputePipelineDesc, PipelineBinding,
    PipelineBindingKind, PipelineLayout, ShaderRef,
};
use hl_gpu::protocol::model::enums::{buffer_usage, TextureFormat};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

// glslang 16.3 output for the GLSL documented in the test below. It retains DimBuffer and uses
// gl_GlobalInvocationID.x as the imageStore coordinate.
const ADJACENT_R8: &[u32] = &[
    119734787,65536,524299,36,0,131089,1,131089,47,131089,49,393227,1,1280527431,
    1685353262,808793134,0,196622,0,1,393231,5,4,1852399981,0,11,393232,4,17,64,1,1,
    196611,2,450,262149,4,1852399981,0,196613,8,105,524293,11,1197436007,1633841004,
    1986939244,1952539503,1231974249,68,393221,19,1886680431,1767863413,1701273965,0,
    262215,11,11,28,262215,19,33,0,262215,19,34,0,262215,35,11,25,131091,2,196641,3,2,
    262165,6,32,0,262176,7,7,6,262167,9,6,3,262176,10,1,9,262203,10,11,1,262187,6,12,0,
    262176,13,1,6,196630,16,32,589849,17,16,5,0,0,0,2,15,262176,18,0,17,262203,18,19,0,
    262165,22,32,1,262187,6,25,251,262187,6,27,1,262187,16,30,1132396544,262167,32,16,4,
    262187,6,34,64,393260,9,35,34,27,27,327734,2,4,0,3,131320,5,262203,7,8,7,327745,13,
    14,11,12,262205,6,15,14,196670,8,15,262205,17,20,19,262205,6,21,8,262268,22,23,21,
    262205,6,24,8,327817,6,26,24,25,327808,6,28,26,27,262256,16,29,28,327816,16,31,29,
    30,458832,32,33,31,31,31,31,262243,20,23,33,65789,65592,
];

const STORE_CONSTANT: &[u32] = &[
    119734787,65536,524299,23,0,131089,1,131089,47,393227,1,1280527431,1685353262,
    808793134,0,196622,0,1,327695,5,4,1852399981,0,393232,4,17,1,1,1,196611,2,450,
    262149,4,1852399981,0,393221,9,1886680431,1767863413,1701273965,0,262215,9,33,0,
    262215,9,34,0,262215,22,11,25,131091,2,196641,3,2,196630,6,32,589849,7,6,5,0,0,0,
    2,4,262176,8,0,7,262203,8,9,0,262165,11,32,1,262187,11,12,0,262167,13,6,4,
    262187,6,14,1048576000,262187,6,15,1056964608,262187,6,16,1061158912,262187,6,17,
    1065353216,458796,13,18,14,15,16,17,262165,19,32,0,262167,20,19,3,262187,19,21,1,
    393260,20,22,21,21,21,327734,2,4,0,3,131320,5,262205,7,10,9,262243,10,12,18,65789,
    65592,
];

const ALIAS_BOTH_DIRECTIONS: &[u32] = &[119734787,65536,524299,39,0,131089,1,131089,47,393227,1,1280527431,1685353262,808793134,0,196622,0,1,327695,5,4,1852399981,0,393232,4,17,1,1,1,196611,2,450,262149,4,1852399981,0,262149,8,1685221207,115,327686,8,0,1685221239,0,262149,10,1685221239,115,262149,19,1702389108,29548,262215,7,6,4,196679,8,3,262216,8,0,23,327752,8,0,35,0,196679,10,23,262215,10,33,1,262215,10,34,0,196679,19,23,262215,19,33,0,262215,19,34,0,262215,38,11,25,131091,2,196641,3,2,262165,6,32,0,196637,7,6,196638,8,7,262176,9,2,8,262203,9,10,2,262165,11,32,1,262187,11,12,0,262187,6,13,4281344016,262176,14,2,6,196630,16,32,589849,17,16,5,0,0,0,2,4,262176,18,0,17,262203,18,19,0,262187,11,21,1,262167,23,16,4,262187,11,26,2,262187,16,27,1048576000,262187,16,28,1056964608,262187,16,29,1061158912,262187,16,30,1065353216,458796,23,31,27,28,29,30,262187,11,32,3,262167,36,6,3,262187,6,37,1,393260,36,38,37,37,37,327734,2,4,0,3,131320,5,393281,14,15,10,12,12,196670,15,13,262205,17,20,19,262205,17,22,19,327778,23,24,22,12,262243,20,21,24,262205,17,25,19,262243,25,26,31,393281,14,33,10,12,26,262205,6,34,33,393281,14,35,10,12,32,196670,35,34,65789,65592,];

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

fn store_constant_commands(
    format: TextureFormat,
    buffer_size: u64,
    offset: u64,
    view_size: u64,
) -> Vec<Cmd> {
    vec![
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv: STORE_CONSTANT.to_vec() },
        Cmd::CreateComputePipelineLayout(
            1,
            ComputePipelineDesc {
                compute: ShaderRef { module: 1, entry: "main".into() },
                label: String::new(),
            },
            PipelineLayout {
                bindings: vec![PipelineBinding {
                    group: 0,
                    binding: 0,
                    count: 1,
                    kind: PipelineBindingKind::StorageTexelBuffer,
                }],
            },
        ),
        Cmd::CreateBuffer(1, buffer(buffer_size)),
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![BindEntry {
                    binding: 0,
                    resource: BindResource::TexelBuffer {
                        id: 1, offset, size: view_size, format, writable: true,
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
    ]
}

fn store_constant(format: TextureFormat, size: u64) -> Vec<u8> {
    let commands = store_constant_commands(format, size, 0, size);
    let (executor, session) = run(&commands);
    executor.read_buffer(&session.resources, BufferId(1), 0, size as usize).unwrap()
}

fn submit_error(commands: &[Cmd]) -> hl_gpu::GpuError {
    let mut executor = WgpuExecutor::new(DeviceConfig::default()).expect("wgpu adapter");
    let limits = Limits::from_capabilities(executor.capabilities());
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut session, &mut executor, 0, commands).unwrap_err()
}

#[test]
fn invalid_texel_ranges_refuse_before_native_submission() {
    for commands in [
        store_constant_commands(TextureFormat::Rgba8Unorm, 4, 0, 8),
        store_constant_commands(TextureFormat::Rgba8Unorm, 8, 1, 4),
        store_constant_commands(TextureFormat::Rgba8Unorm, 4, 0, 0),
    ] {
        assert!(matches!(submit_error(&commands), hl_gpu::GpuError::OutOfBounds));
    }
}

#[test]
fn r8_single_texel_range_and_vulkan_aligned_offset_are_native_valid() {
    assert_eq!(store_constant(TextureFormat::R8Unorm, 1), vec![64]);

    let commands = store_constant_commands(TextureFormat::R8Unorm, 17, 16, 1);
    let (executor, session) = run(&commands);
    let bytes = executor.read_buffer(&session.resources, BufferId(1), 0, 17).unwrap();
    assert_eq!(&bytes[..16], &[0; 16]);
    assert_eq!(bytes[16], 64);
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .expect("wgpu adapter");
    assert!(adapter.limits().min_storage_buffer_offset_alignment >= 16);
}

#[test]
fn bgra8_store_uses_physical_bgra_component_order() {
    assert_eq!(store_constant(TextureFormat::Bgra8Unorm, 4), vec![191, 128, 64, 255]);
}

#[test]
fn rgba16float_store_packs_each_half_component() {
    let bytes = store_constant(TextureFormat::Rgba16Float, 8);
    assert_eq!(bytes, vec![0x00, 0x34, 0x00, 0x38, 0x00, 0x3a, 0x00, 0x3c]);
}

#[test]
fn ssbo_and_texel_alias_are_visible_in_both_ordered_directions() {
    // One invocation performs each write then read in program order through coherent storage variables.
    // This deliberately proves defined intra-invocation visibility in both alias directions; it does not
    // claim behavior for an unsynchronized overlapping race between invocations.
    let commands = vec![
        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::SpirV, spirv: ALIAS_BOTH_DIRECTIONS.to_vec() },
        Cmd::CreateComputePipelineLayout(
            1,
            ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "main".into() }, label: String::new() },
            PipelineLayout { bindings: vec![
                PipelineBinding { group: 0, binding: 0, count: 1, kind: PipelineBindingKind::StorageTexelBuffer },
                PipelineBinding { group: 0, binding: 1, count: 1, kind: PipelineBindingKind::StorageBuffer },
            ] },
        ),
        Cmd::CreateBuffer(1, buffer(16)),
        Cmd::CreateBindGroup(1, BindGroupDesc { set: 0, entries: vec![
            BindEntry { binding: 0, resource: BindResource::TexelBuffer {
                id: 1, offset: 0, size: 16, format: TextureFormat::Rgba8Unorm, writable: true,
            } },
            BindEntry { binding: 1, resource: BindResource::Buffer { id: 1, offset: 0, size: 16 } },
        ] }),
        Cmd::Submit(CommandBuffer { encoder: vec![
            Enc::BeginComputePass, Enc::SetPipeline(1), Enc::SetBindGroup { index: 0, group: 1 },
            Enc::Dispatch { x: 1, y: 1, z: 1 }, Enc::EndComputePass,
        ], signal: None }),
    ];
    let (executor, session) = run(&commands);
    assert_eq!(
        executor.read_buffer(&session.resources, BufferId(1), 0, 16).unwrap(),
        vec![0x10,0x20,0x30,0xff, 0x10,0x20,0x30,0xff, 64,128,191,255, 64,128,191,255]
    );
}

#[test]
fn concurrent_r8_stores_preserve_every_adjacent_lane() {
    const TEXELS: u32 = 4096;
    // GLSL source for ADJACENT_R8:
    // layout(local_size_x=64) in; layout(set=0,binding=0,r8) uniform imageBuffer output_image;
    // imageStore(output_image, int(gl_GlobalInvocationID.x),
    //            vec4(float((gl_GlobalInvocationID.x % 251u) + 1u) / 255.0));
    let commands = vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv: ADJACENT_R8.to_vec(),
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
            PipelineLayout {
                bindings: vec![PipelineBinding {
                    group: 0,
                    binding: 0,
                    count: 1,
                    kind: PipelineBindingKind::StorageTexelBuffer,
                }],
            },
        ),
        Cmd::CreateBuffer(1, buffer(TEXELS.into())),
        Cmd::CreateBindGroup(
            1,
            BindGroupDesc {
                set: 0,
                entries: vec![BindEntry {
                    binding: 0,
                    resource: BindResource::TexelBuffer {
                        id: 1,
                        offset: 0,
                        size: TEXELS.into(),
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
                Enc::Dispatch {
                    x: TEXELS / 64,
                    y: 1,
                    z: 1,
                },
                Enc::EndComputePass,
            ],
            signal: None,
        }),
    ];
    let (executor, session) = run(&commands);
    let actual = executor
        .read_buffer(&session.resources, BufferId(1), 0, TEXELS as usize)
        .unwrap();
    let expected = (0..TEXELS)
        .map(|i| ((i % 251) + 1) as u8)
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}
