use super::*;
use hl_gpu::protocol::model::capability::BC_FORMATS;

#[test]
fn executor_advertises_exactly_the_formats_this_suite_proves() {
    let exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let advertised = exec.capabilities().texture_formats;

    // The suite round-trips the uncompressed formats here and every advertised BC format in
    // `native_bc_family_upload_roundtrips_exact_blocks`.
    let proven = TextureFormat::bits(COLOR_FORMATS)
        | TextureFormat::bits(DEPTH_FORMATS)
        | TextureFormat::bits(&[TextureFormat::Depth24PlusStencil8])
        // Proven by `integer::every_integer_format_stores_exact_integer_texels`.
        | TextureFormat::bits(hl_gpu::protocol::model::capability::INTEGER_FORMATS)
        // Proven by `native::native_formats_transfer_roundtrip_exact_bytes` plus the role tests there.
        | TextureFormat::bits(hl_gpu::protocol::model::capability::NATIVE_FORMATS)
        | (advertised & TextureFormat::bits(BC_FORMATS));

    assert_eq!(advertised, proven,
        "the executor's advertised texture_formats bitset ({advertised:#b}) must equal EXACTLY the set this \
         suite round-trips ({proven:#b}) — any advertised-but-unproven (or proven-but-unadvertised) format is a bug");

    // Every advertised uncompressed color format maps to a texel footprint.
    for &fmt in COLOR_FORMATS {
        assert!(
            fmt.bytes_per_texel().is_some(),
            "{fmt:?}: advertised color format must have a texel footprint"
        );
    }
}
