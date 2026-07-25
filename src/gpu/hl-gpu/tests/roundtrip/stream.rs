use super::*;

#[test]
fn stream_round_trips_unchanged() {
    let cmds = representative_stream();
    let bytes = hl_gpu::Encoder::stream(&cmds);
    let back = hl_gpu::Decoder::stream(&bytes).expect("decode");
    assert_eq!(cmds, back, "stream must survive encode→decode unchanged");
}

#[test]
fn each_command_frame_round_trips() {
    for c in representative_stream() {
        let framed = c.frame();
        let mut d = Decoder::new(&framed);
        assert_eq!(
            Cmd::decode_frame(&mut d).unwrap(),
            c,
            "per-command frame round-trip"
        );
    }
}

#[test]
fn shader_payload_kind_is_reclassified_by_neutral_magic() {
    // The wire carries no kind byte; the decoder re-derives the kind from the payload's leading word
    // against the NEUTRAL magics in model::kernel (never a CUDA/PTX constant).
    use hl_gpu::protocol::model::kernel::{KernelDescriptor, KERNEL_MAGIC, SPIRV_MAGIC};
    let kd = KernelDescriptor {
        ptx: "ret;".into(),
        entry: "k".into(),
        block: [64, 1, 1],
    };
    let cmds = vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv: vec![SPIRV_MAGIC, 0, 0],
        },
        Cmd::CreateShader {
            id: 2,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kd.to_words(),
        },
        Cmd::CreateShader {
            id: 3,
            kind: ShaderPayloadKind::Msl,
            spirv: vec![0x0000_00ff, 1, 2],
        },
    ];
    let back = hl_gpu::Decoder::stream(&hl_gpu::Encoder::stream(&cmds)).unwrap();
    assert!(matches!(
        back[0],
        Cmd::CreateShader {
            kind: ShaderPayloadKind::SpirV,
            ..
        }
    ));
    assert!(matches!(
        back[1],
        Cmd::CreateShader {
            kind: ShaderPayloadKind::PtxKernel,
            ..
        }
    ));
    assert!(matches!(
        back[2],
        Cmd::CreateShader {
            kind: ShaderPayloadKind::Msl,
            ..
        }
    ));
    assert_eq!(
        back[1].clone(),
        cmds[1],
        "kernel payload words survive intact"
    );
    assert_eq!(KERNEL_MAGIC, 0xDD6B_0001);
    assert_eq!(SPIRV_MAGIC, 0x0723_0203);
}

#[test]
fn decode_rejects_truncation_and_bad_tags() {
    let bytes = hl_gpu::Encoder::stream(&representative_stream());
    // truncate mid-stream -> contextual ShortBuffer, never a panic
    let err = hl_gpu::Decoder::stream(&bytes[..bytes.len() - 3]).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("command") && m.contains("short buffer"))
    );
    // a bogus leading tag byte
    let err = hl_gpu::Decoder::stream(&[250, 0, 0, 0, 0]).unwrap_err();
    assert!(
        matches!(&err, GpuError::Decode(m) if m.contains("command 0") && m.contains("bad command/encoder tag 250"))
    );
}

#[test]
fn framed_command_decode_rejects_trailing_bytes() {
    let cmd = Cmd::CreateFence(1);
    let good = cmd.frame();
    let mut d = Decoder::new(&good);
    assert_eq!(Cmd::decode_frame(&mut d).unwrap(), cmd);
    // trailing garbage inside the frame body is malformed
    let mut e = Encoder::new();
    e.frame(|inner| {
        cmd.encode(inner);
        inner.u8(0xEE);
    });
    let framed = e.into_vec();
    let mut d = Decoder::new(&framed);
    assert_eq!(Cmd::decode_frame(&mut d), Err(GpuError::TrailingBytes));
}

#[test]
fn decoder_does_not_preallocate_on_bogus_counts() {
    // A CreateBindGroup claiming ~4 billion entries but with an empty body must fail cleanly, never
    // attempt a multi-gigabyte reservation first.
    let mut e = Encoder::new();
    e.u8(hl_gpu::protocol::model::command::tag::CREATE_BIND_GROUP);
    e.u32(1); // id
    e.u32(0); // set
    e.u32(0xFFFF_FFFF); // entry count = ~4 billion, no entries follow
    let bytes = e.into_vec();
    let err = hl_gpu::Decoder::stream(&bytes).unwrap_err();
    assert!(matches!(&err, GpuError::Decode(m) if m.contains("short buffer")));
}

// ---------------------------------------------------------------------------------------------------
// EXHAUSTIVE enum-value codec coverage: every valid value of every wire enum round-trips byte-identical,
// both at the `to_u32`∘`from_u32` boundary (what encode/decode call) AND embedded in a real command that
// carries it on the wire. A single enum value that fails to survive is a producer/consumer desync.
// ---------------------------------------------------------------------------------------------------
