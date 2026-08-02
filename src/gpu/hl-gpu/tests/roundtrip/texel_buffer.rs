use super::*;

#[test]
fn texel_buffer_view_metadata_roundtrips_exactly() {
    let commands = vec![Cmd::CreateBindGroup(
        7,
        BindGroupDesc {
            set: 2,
            entries: vec![BindEntry {
                binding: 5,
                resource: BindResource::TexelBuffer {
                    id: 11,
                    offset: 16,
                    size: 64,
                    format: TextureFormat::Rgba8Unorm,
                    writable: true,
                },
            }],
        },
    )];
    assert_eq!(
        Decoder::stream(&Encoder::stream(&commands)).unwrap(),
        commands
    );
}
