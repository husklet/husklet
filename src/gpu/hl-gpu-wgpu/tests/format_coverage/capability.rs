use super::*;

#[test]
fn executor_advertises_exactly_the_formats_this_suite_proves() {
    let exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };
    let advertised = exec.capabilities().texture_formats;

    // The set this file round-trips: all COLOR_FORMATS + Depth32Float (DEPTH_FORMATS) + Depth24PlusStencil8
    // (the combined depth+stencil format this stencil-lowering executor additionally advertises).
    let proven = TextureFormat::bits(COLOR_FORMATS)
        | TextureFormat::bits(DEPTH_FORMATS)
        | TextureFormat::bits(&[TextureFormat::Depth24PlusStencil8]);

    assert_eq!(advertised, proven,
        "the executor's advertised texture_formats bitset ({advertised:#b}) must equal EXACTLY the set this \
         suite round-trips ({proven:#b}) — any advertised-but-unproven (or proven-but-unadvertised) format is a bug");

    // And every advertised color-format bit maps to a real wgpu format + a texel footprint (no silent alias).
    for &fmt in COLOR_FORMATS {
        assert!(
            fmt.bytes_per_texel().is_some(),
            "{fmt:?}: advertised color format must have a texel footprint"
        );
    }
}
