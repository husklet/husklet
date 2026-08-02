use super::*;

#[test]
fn framed_command_rejects_trailing_bytes() {
    let cmd = Cmd::CreateFence(1);
    let mut e = Encoder::new();
    e.frame(|inner| {
        cmd.encode(inner);
        inner.u8(0xEE); // trailing garbage inside the frame body
    });
    let framed = e.into_vec();
    let mut d = Decoder::new(&framed);
    assert_eq!(Cmd::decode_frame(&mut d), Err(GpuError::TrailingBytes));
}

#[test]
fn boundary_field_values_round_trip() {
    // Extreme-but-valid field values: u32/u64 saturation, i32 min/max base_vertex, empty + long strings,
    // empty vectors, and finite-float extremes.
    let big_source: String = "x".repeat(4096);
    let cmds = vec![
        Cmd::CreateBuffer(
            u32::MAX,
            BufferDesc {
                size: u64::MAX,
                usage: u32::MAX,
                label: String::new(),
            },
        ),
        Cmd::WriteBuffer {
            id: 0,
            offset: u64::MAX,
            data: vec![],
        },
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::DrawIndexed {
                    index_count: u32::MAX,
                    instance_count: u32::MAX,
                    first_index: u32::MAX,
                    base_vertex: i32::MIN,
                    first_instance: u32::MAX,
                },
                Enc::DrawIndexed {
                    index_count: 0,
                    instance_count: 0,
                    first_index: 0,
                    base_vertex: i32::MAX,
                    first_instance: 0,
                },
                Enc::SetVertexBuffer {
                    slot: u32::MAX,
                    buffer: u32::MAX,
                    offset: u64::MAX,
                },
                Enc::SetViewport {
                    x: f32::MIN,
                    y: f32::MAX,
                    w: f32::MIN_POSITIVE,
                    h: -0.0,
                    min_depth: 0.0,
                    max_depth: 1.0,
                },
                Enc::FillBuffer {
                    buffer: 0,
                    offset: u64::MAX,
                    size: u64::MAX,
                    value: u32::MAX,
                },
            ],
            signal: Some((u32::MAX, u64::MAX)),
        }),
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: GlslDescriptor {
                stage: glsl_stage::VERTEX,
                entry: String::new(),
                source: big_source,
            }
            .to_words(),
        },
    ];
    let bytes = hl_gpu::Encoder::stream(&cmds);
    assert_eq!(
        hl_gpu::Decoder::stream(&bytes).unwrap(),
        cmds,
        "boundary values survive the wire unchanged"
    );
    assert_eq!(
        hl_gpu::Encoder::stream(&hl_gpu::Decoder::stream(&bytes).unwrap()),
        bytes,
        "and are byte-stable"
    );
}

#[test]
fn wire_version_and_magics_are_pinned() {
    // A version bump or a magic change must be a deliberate, reviewed edit (matches the frozen goldens).
    // Version 10 adds typed descriptor arrays and negotiated array capabilities.
    // Version 15 appends a per-axis `Mirror` to `BlitTexture`, which no v14 decoder can frame.
    assert_eq!(WIRE_VERSION, 16);
    assert_eq!(SPIRV_MAGIC, 0x0723_0203);
    assert_eq!(KERNEL_MAGIC, 0xDD6B_0001);
    assert_eq!(GLSL_MAGIC, 0xDD67_0001);
    // The three magics are mutually distinct so payload classification is unambiguous.
    assert_ne!(SPIRV_MAGIC, KERNEL_MAGIC);
    assert_ne!(KERNEL_MAGIC, GLSL_MAGIC);
    assert_ne!(SPIRV_MAGIC, GLSL_MAGIC);
}

// ---------------------------------------------------------------------------------------------------
// 4. the neutral kernel/GLSL descriptor decoders (executor-facing) reject malformed payloads
// ---------------------------------------------------------------------------------------------------

#[test]
fn kernel_and_glsl_descriptor_from_words_are_robust() {
    // Wrong / missing magic -> None (not this kind).
    assert!(KernelDescriptor::from_words(&[SPIRV_MAGIC, 0]).is_none());
    assert!(KernelDescriptor::from_words(&[]).is_none());
    assert!(GlslDescriptor::from_words(&[KERNEL_MAGIC, 0]).is_none());
    assert!(GlslDescriptor::from_words(&[GLSL_MAGIC]).is_none()); // < 2 words

    // Declared byte length exceeds the payload -> a typed truncation error, never a panic/OOB.
    match KernelDescriptor::from_words(&[KERNEL_MAGIC, 0xFFFF_FFFF, 1, 2]) {
        Some(Err(GpuError::Kernel(_))) => {}
        other => panic!("kernel truncation must be a typed Kernel error, got {other:?}"),
    }
    match GlslDescriptor::from_words(&[GLSL_MAGIC, 0xFFFF_FFFF, 1, 2]) {
        Some(Err(GpuError::Kernel(_))) => {}
        other => panic!("glsl truncation must be a typed error, got {other:?}"),
    }

    // A real descriptor survives the words round-trip.
    let kd = KernelDescriptor {
        ptx: "mov;".into(),
        entry: "e".into(),
        block: [1, 2, 3],
    };
    assert_eq!(
        KernelDescriptor::from_words(&kd.to_words())
            .unwrap()
            .unwrap(),
        kd
    );
    let gd = GlslDescriptor {
        stage: glsl_stage::COMPUTE,
        entry: "c".into(),
        source: "void main(){}".into(),
    };
    assert_eq!(
        GlslDescriptor::from_words(&gd.to_words()).unwrap().unwrap(),
        gd
    );

    // A GLSL payload with a truncated INNER body (byte_len fits the words, but the framed strings run past
    // the declared length) yields a typed error, never a panic.
    let mut words = gd.to_words();
    if let Some(last) = words.last_mut() {
        *last = 0xFFFF_FFFF; // corrupt the tail
    }
    catch_unwind(move || {
        let _ = GlslDescriptor::from_words(&words);
    })
    .expect("GLSL descriptor decode must not panic on a corrupted tail");
}
