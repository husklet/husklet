//! Exact-native formats added for Vulkan's mandatory table.

use super::*;
use hl_gpu::protocol::model::capability::NATIVE_FORMATS;
use hl_gpu::protocol::model::descriptor::{
    ComputePipelineDesc, PipelineBinding, PipelineBindingKind, PipelineLayout,
};

fn transfer_roundtrip(exec: &mut WgpuExecutor, format: TextureFormat, bytes: &[u8]) {
    let (width, height) = format.block_geometry().map_or((1, 1), |(w, h, _)| (w, h));
    let mut session = new_session(exec);
    hl_gpu::runtime::submit(
        &mut session,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    width,
                    height,
                    format,
                    texture_usage::COPY_DST | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: bytes.len() as u64,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: bytes.to_vec(),
            },
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::CopyBufferToTexture {
                    src: 1,
                    src_offset: 0,
                    bytes_per_row: bytes.len() as u32,
                    dst: 1,
                    mip: 0,
                    width,
                    height,
                }],
                signal: None,
            }),
        ],
    )
    .unwrap_or_else(|error| panic!("{format:?} transfer upload failed: {error}"));
    assert_eq!(
        exec.read_texture(&session.resources, 1).unwrap(),
        bytes,
        "{format:?}"
    );
}

#[test]
fn native_formats_transfer_roundtrip_exact_bytes() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("wgpu adapter");
    let cases: &[(TextureFormat, &[u8])] = &[
        (TextureFormat::R8Snorm, &[0x81]),
        (TextureFormat::Rg8Snorm, &[0x81, 0x7f]),
        (TextureFormat::Rgba8Snorm, &[0x81, 0xc0, 0x40, 0x7f]),
        (TextureFormat::Rg16Float, &[0x00, 0x38, 0x00, 0xb4]),
        (TextureFormat::R16Float, &[0x00, 0x38]),
        (TextureFormat::R16Uint, &[0x34, 0x12]),
        (TextureFormat::R16Sint, &[0xcc, 0xff]),
        (TextureFormat::Rg16Uint, &[1, 0, 2, 0]),
        (TextureFormat::Rg16Sint, &[0xff, 0xff, 2, 0]),
        (TextureFormat::Rgba16Uint, &[1, 0, 2, 0, 3, 0, 4, 0]),
        (
            TextureFormat::Rgba16Sint,
            &[0xff, 0xff, 2, 0, 0xfd, 0xff, 4, 0],
        ),
        (TextureFormat::Rg32Uint, &[1, 0, 0, 0, 2, 0, 0, 0]),
        (
            TextureFormat::Rg32Sint,
            &[0xff, 0xff, 0xff, 0xff, 2, 0, 0, 0],
        ),
        (TextureFormat::Rgb9e5Ufloat, &[0x01, 0x02, 0x03, 0x04]),
        (TextureFormat::Rgb10a2Unorm, &[0x01, 0x02, 0x03, 0x04]),
        (TextureFormat::Rgb10a2Uint, &[0x01, 0x02, 0x03, 0x04]),
        (TextureFormat::Rg11b10Ufloat, &[0x01, 0x02, 0x03, 0x04]),
        (TextureFormat::R5g6b5Unorm, &[0x08, 0xbc]),
        (TextureFormat::A1r5g5b5Unorm, &[0x08, 0xde]),
        (TextureFormat::B4g4r4a4Unorm, &[0xbf, 0x48]),
    ];
    assert_eq!(
        cases.len(),
        NATIVE_FORMATS.len(),
        "every advertised native format needs bytes"
    );
    for (format, bytes) in cases {
        assert!(NATIVE_FORMATS.contains(format));
        transfer_roundtrip(&mut exec, *format, bytes);
    }
}

#[test]
fn native_etc2_family_upload_roundtrips_exact_blocks() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("wgpu adapter");
    for &format in hl_gpu::protocol::model::capability::ETC2_FORMATS {
        let bytes = vec![0x5a; format.block_geometry().unwrap().2 as usize];
        transfer_roundtrip(&mut exec, format, &bytes);
    }
}

#[test]
fn native_normalized_and_float_formats_sample_exact_values() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("wgpu adapter");
    let rg8 = super::sample::sample_stored(&mut exec, TextureFormat::Rg8Snorm, &[0x40, 0xc0]);
    // 0x40 is +64/127 ~= 0.504, while 0xc0 is negative and clamps to zero in the Unorm target.
    assert!(
        near_tol(rg8, [129, 0, 0, 255], 2),
        "RG8 snorm sample: {rg8:?}"
    );
    let rgba8 = super::sample::sample_stored(
        &mut exec,
        TextureFormat::Rgba8Snorm,
        &[0x20, 0x40, 0x60, 0x7f],
    );
    assert!(
        near_tol(rgba8, [64, 129, 193, 255], 2),
        "RGBA8 snorm sample: {rgba8:?}"
    );
    let rg16 = super::sample::sample_stored(
        &mut exec,
        TextureFormat::Rg16Float,
        &[0x00, 0x38, 0x00, 0x34],
    );
    assert!(
        near_tol(rg16, [128, 64, 0, 255], 2),
        "RG16 float sample: {rg16:?}"
    );

    for (format, bytes) in [
        (TextureFormat::R5g6b5Unorm, &[0x00, 0xf8][..]),
        (TextureFormat::A1r5g5b5Unorm, &[0x00, 0xfc][..]),
        (TextureFormat::B4g4r4a4Unorm, &[0xff, 0x00][..]),
    ] {
        let sampled = super::sample::sample_stored(&mut exec, format, bytes);
        assert!(
            near_tol(sampled, [255, 0, 0, 255], 1),
            "{format:?} must sample Vulkan red as red: {sampled:?}"
        );
    }
}

#[test]
fn native_formats_are_real_color_attachments() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("wgpu adapter");

    let rg8 = super::color::draw_const(&mut exec, TextureFormat::Rg8Snorm);
    assert_eq!(
        &rg8[..2],
        &[95, 64],
        "0.75 and 0.5 encode as exact RG8 snorm values"
    );

    let rgba8 = super::color::draw_const(&mut exec, TextureFormat::Rgba8Snorm);
    assert_eq!(
        &rgba8[..4],
        &[95, 64, 32, 127],
        "RGBA8 snorm attachment stores every channel"
    );

    let rg16 = super::color::draw_const(&mut exec, TextureFormat::Rg16Float);
    assert_eq!(
        &rg16[..4],
        &[0x00, 0x3a, 0x00, 0x38],
        "RG16 float attachment stores exact halves"
    );

    for (format, expected) in [
        (TextureFormat::R5g6b5Unorm, [0x08, 0xbc]),
        (TextureFormat::A1r5g5b5Unorm, [0x08, 0xde]),
    ] {
        let bytes = super::color::draw_const(&mut exec, format);
        assert_eq!(&bytes[..2], &expected, "{format:?} attachment packing");
    }
}

#[test]
fn rgba8snorm_storage_image_writes_exact_native_bytes() {
    let source = r#"
@group(0) @binding(0) var image: texture_storage_2d<rgba8snorm, write>;
@compute @workgroup_size(1) fn main() {
    textureStore(image, vec2<i32>(0, 0), vec4<f32>(0.25, 0.5, 0.75, 1.0));
}
"#;
    let module = naga::front::wgsl::parse_str(source).expect("storage image WGSL");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("storage image validation");
    let spirv =
        naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
            .expect("storage image SPIR-V");

    let mut exec = WgpuExecutor::new(DeviceConfig::default()).expect("wgpu adapter");
    let mut session = new_session(&exec);
    hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
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
                        entry: "main".into(),
                    },
                    label: String::new(),
                },
                PipelineLayout {
                    bindings: vec![PipelineBinding {
                        group: 0,
                        binding: 0,
                        count: 1,
                        kind: PipelineBindingKind::StorageTexture,
                    }],
                },
            ),
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Snorm,
                    texture_usage::STORAGE | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Texture { id: 1 },
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
    )
    .expect("RGBA8 snorm storage image executes");
    assert_eq!(
        exec.read_texture(&session.resources, 1).unwrap(),
        [32, 64, 95, 127]
    );
}
