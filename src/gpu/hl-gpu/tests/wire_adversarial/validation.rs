use super::*;

#[test]
fn empty_stream_decodes_to_no_commands() {
    assert_eq!(hl_gpu::Decoder::stream(&[]).unwrap(), Vec::<Cmd>::new());
}

#[test]
fn unknown_top_level_tag_is_bad_tag() {
    for bad_tag in [0u8, 26, 100, 255] {
        let err = hl_gpu::Decoder::stream(&[bad_tag]).unwrap_err();
        assert!(
            matches!(&err, GpuError::Decode(m) if m.contains(&format!("bad command/encoder tag {bad_tag}"))),
            "tag {bad_tag} -> {err:?}"
        );
    }
}

#[test]
fn unknown_encoder_tag_inside_submit_is_bad_tag() {
    for bad_etag in [0u8, 26, 99, 255] {
        // Submit with one op whose etag byte is unknown.
        let mut e = Encoder::new();
        e.u8(tag::SUBMIT);
        e.u32(1); // encoder len
        e.u8(bad_etag); // the op tag
        let err = hl_gpu::Decoder::stream(&e.into_vec()).unwrap_err();
        assert!(
            matches!(&err, GpuError::Decode(m) if m.contains(&format!("bad command/encoder tag {bad_etag}"))),
            "etag {bad_etag} -> {err:?}"
        );
    }
}

#[test]
fn out_of_range_enums_are_typed_bad_enum() {
    // Every wire enum's `from_u32` rejects an out-of-range value and accepts every in-range one, and
    // to_u32∘from_u32 is the identity on the valid domain.
    assert!(matches!(
        TextureFormat::from_u32(0),
        Err(GpuError::BadEnum {
            what: "TextureFormat",
            ..
        })
    ));
    assert!(matches!(
        TextureFormat::from_u32(26),
        Err(GpuError::BadEnum { .. })
    ));
    for v in 1..=11 {
        assert_eq!(TextureFormat::from_u32(v).unwrap().to_u32(), v);
    }
    assert!(matches!(
        TextureDim::from_u32(0),
        Err(GpuError::BadEnum {
            what: "TextureDim",
            ..
        })
    ));
    assert!(matches!(
        TextureDim::from_u32(5),
        Err(GpuError::BadEnum { .. })
    ));
    assert!(matches!(
        IndexFormat::from_u32(0),
        Err(GpuError::BadEnum {
            what: "IndexFormat",
            ..
        })
    ));
    assert!(matches!(
        IndexFormat::from_u32(3),
        Err(GpuError::BadEnum { .. })
    ));
    assert!(matches!(
        Topology::from_u32(5),
        Err(GpuError::BadEnum {
            what: "Topology",
            ..
        })
    ));
    assert!(matches!(
        LoadOp::from_u32(3),
        Err(GpuError::BadEnum { what: "LoadOp", .. })
    ));
    assert!(matches!(
        Filter::from_u32(2),
        Err(GpuError::BadEnum { what: "Filter", .. })
    ));
    assert!(matches!(
        TextureAspect::from_u32(3),
        Err(GpuError::BadEnum {
            what: "TextureAspect",
            ..
        })
    ));
    assert!(matches!(
        AddressMode::from_u32(3),
        Err(GpuError::BadEnum {
            what: "AddressMode",
            ..
        })
    ));
}

#[test]
fn bad_enum_in_a_real_stream_is_rejected() {
    // A CreateTexture whose `dim` word is out of range must fail decode with a BadEnum context. Build the
    // bytes by hand so the exact field is corrupted.
    let mut e = Encoder::new();
    e.u8(tag::CREATE_TEXTURE);
    e.u32(2); // id
    e.u32(4); // width
    e.u32(4); // height
    e.u32(1); // depth
    e.u32(1); // mip_levels
    e.u32(1); // sample_count
    e.u32(99); // dim <-- out of range
    e.u32(1); // format
    e.u32(0); // usage
    e.str(""); // label
    let err = hl_gpu::Decoder::stream(&e.into_vec()).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("bad TextureDim enum value 99")),
        "{err:?}"
    );
}

#[test]
fn non_finite_render_floats_are_rejected_at_the_wire() {
    // The encoder does NOT reject NaN/±inf (a local producer bug), but the decoder MUST — a hostile
    // viewport/clear/depth float can never reach a backend. Each finite-float site is guarded.
    let bad_floats = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
    for bad in bad_floats {
        let sites = [
            Enc::SetViewport {
                x: bad,
                y: 0.0,
                w: 1.0,
                h: 1.0,
                min_depth: 0.0,
                max_depth: 1.0,
            },
            Enc::ClearRect {
                texture: 1,
                x: 0,
                y: 0,
                w: 1,
                h: 1,
                color: [bad, 0.0, 0.0, 1.0],
            },
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, bad, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::BeginRenderPass {
                color: vec![],
                depth: Some(DepthAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear_depth: bad,
                    clear_stencil: 0,
                }),
            },
        ];
        for op in sites {
            let bytes = hl_gpu::Encoder::stream(&[Cmd::Submit(CommandBuffer {
                encoder: vec![op.clone()],
                signal: None,
            })]);
            let err = hl_gpu::Decoder::stream(&bytes).unwrap_err();
            assert!(
                matches!(&err, GpuError::Decode(m) if m.contains("non-finite")),
                "op {op:?} with {bad} must reject non-finite: {err:?}"
            );
        }
    }
}

#[test]
fn non_canonical_bool_byte_is_rejected() {
    // A Submit with a single EndRenderPass op and a signal-present bool of 2 (neither 0 nor 1).
    let mut e = Encoder::new();
    e.u8(tag::SUBMIT);
    e.u32(1); // encoder len
    e.u8(etag::END_RENDER_PASS);
    e.u8(2); // signal-present bool <-- non-canonical
    let err = hl_gpu::Decoder::stream(&e.into_vec()).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("non-canonical boolean wire byte 2")),
        "{err:?}"
    );
}

#[test]
fn bad_bindresource_tag_is_typed_bad_enum() {
    // A CreateBindGroup entry whose resource discriminant byte is unknown (only 0/1/2 are valid).
    let mut e = Encoder::new();
    e.u8(tag::CREATE_BIND_GROUP);
    e.u32(1); // id
    e.u32(0); // set
    e.u32(1); // one entry
    e.u32(0); // binding
    e.u8(9); // resource tag <-- unknown
    let err = hl_gpu::Decoder::stream(&e.into_vec()).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("bad BindResource enum value 9")),
        "{err:?}"
    );
}

#[test]
fn bogus_length_prefix_is_short_buffer_not_a_giant_prealloc() {
    // WriteBuffer with a ~4-billion-byte data length but no body must fail cleanly (no multi-GB reserve).
    let mut e = Encoder::new();
    e.u8(tag::WRITE_BUFFER);
    e.u32(1); // id
    e.u64(0); // offset
    e.u32(0xFFFF_FFF0); // data length, nothing follows
    let err = hl_gpu::Decoder::stream(&e.into_vec()).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("short buffer")),
        "{err:?}"
    );

    // Same for a CreateShader claiming ~4 billion words.
    let mut e = Encoder::new();
    e.u8(tag::CREATE_SHADER);
    e.u32(1); // id
    e.u32(0xFFFF_FFF0); // word count, no words follow
    let err = hl_gpu::Decoder::stream(&e.into_vec()).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("short buffer")),
        "{err:?}"
    );
}
