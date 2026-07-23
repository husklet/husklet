use super::*;

#[test]
fn every_shader_payload_kind_classifies_deterministically() {
    // The wire carries NO kind byte; the decoder re-derives the kind from the payload's leading magic.
    // SpirV/PtxKernel/Glsl are magic-led (round-trip exactly); DemoBuiltin + LegacyMsl are magic-less and
    // BOTH classify as LegacyMsl on decode (documented lossy classification — no magic to distinguish).
    use hl_gpu::protocol::model::kernel::{
        glsl_stage, GlslDescriptor, KernelDescriptor, SPIRV_MAGIC,
    };
    let kd = KernelDescriptor {
        ptx: "ret;".into(),
        entry: "k".into(),
        block: [1, 1, 1],
    };
    let gd = GlslDescriptor {
        stage: glsl_stage::VERTEX,
        entry: "v".into(),
        source: "x".into(),
    };
    let magic_led = vec![
        (
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: vec![SPIRV_MAGIC, 0],
            },
            ShaderPayloadKind::SpirV,
        ),
        (
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::PtxKernel,
                spirv: kd.to_words(),
            },
            ShaderPayloadKind::PtxKernel,
        ),
        (
            Cmd::CreateShader {
                id: 3,
                kind: ShaderPayloadKind::Glsl,
                spirv: gd.to_words(),
            },
            ShaderPayloadKind::Glsl,
        ),
    ];
    for (cmd, want) in magic_led {
        let back =
            hl_gpu::Decoder::stream(&hl_gpu::Encoder::stream(std::slice::from_ref(&cmd))).unwrap();
        match &back[0] {
            Cmd::CreateShader { kind, .. } => {
                assert_eq!(*kind, want, "magic-led payload must reclassify to {want:?}")
            }
            other => panic!("expected CreateShader, got {other:?}"),
        }
    }
    // DemoBuiltin (no magic) decodes as LegacyMsl — the only kinds that do not round-trip by design.
    for kind in [ShaderPayloadKind::DemoBuiltin, ShaderPayloadKind::LegacyMsl] {
        let back = hl_gpu::Decoder::stream(&hl_gpu::Encoder::stream(&[Cmd::CreateShader {
            id: 1,
            kind,
            spirv: vec![0x4141_4141, 0x4242_4242],
        }]))
        .unwrap();
        assert!(
            matches!(
                back[0],
                Cmd::CreateShader {
                    kind: ShaderPayloadKind::LegacyMsl,
                    ..
                }
            ),
            "magic-less payload classifies as LegacyMsl"
        );
    }
}
