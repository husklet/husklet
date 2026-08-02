//! Golden byte-vector snapshots + wire-compatibility guard.
//!
//! The expected byte arrays below were captured from the SHIPPING `hl-gpu` crate encoding the identical
//! command streams (verified byte-for-byte during authoring). Any change to the wire layout that would
//! desync this staging crate from the deployed `hl-gpu` breaks these tests. [`WIRE_VERSION`] is asserted
//! explicitly so a version bump is a deliberate, reviewed change.

use hl_gpu::protocol::model::command::*;
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::WIRE_VERSION;

#[test]
fn wire_version_is_pinned_at_16() {
    // A change here must be intentional: it is the negotiated handshake version that keeps a stale
    // guest/backend pair from reinterpreting a tag it predates. Bumped 5 → 6 when the additive `Glsl`
    // shader-payload channel (leading `GLSL_MAGIC`) was introduced, and 6 → 7 when stencil test/write was
    // added (front+back `StencilFaceState` + masks on `DepthState`, `clear_stencil` on `DepthAttachment`,
    // and the dynamic `SetStencilReference` etag 22). The stencil fields append AFTER the existing depth
    // fields and only when a pipeline/pass actually carries a depth attachment, so a `depth: None` stream
    // (like the goldens below) is byte-for-byte unchanged. Bumped 7 → 8 when MSAA added
    // `RenderPipelineDesc.sample_count` (appended after `front_face`); no golden stream below encodes a
    // render pipeline, so every GOLDEN_* byte array is still valid unchanged. Bumped 8 → 9 when the
    // presentation identity became a non-zero u64 and Present gained its u64 frame serial. Bumped 9 → 10
    // when bind groups gained typed buffer/texture/sampler array resources. Bumped 14 → 15 when
    // `BlitTexture` gained its per-axis `Mirror` (appended after `filter`); no golden stream below
    // encodes a blit, so every GOLDEN_* byte array remained valid unchanged. Bumped 15 → 16 when
    // colour-clear payloads widened from f32 to f64 so integer render targets retain exact values above
    // the f32 integer precision limit; stream B therefore has a deliberately updated snapshot below.
    assert_eq!(WIRE_VERSION, 16);
}

/// Stream A: buffer create + write + fence create + wait + destroy.
fn stream_a() -> Vec<Cmd> {
    vec![
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 256,
                usage: buffer_usage::VERTEX | buffer_usage::COPY_DST,
                label: "vb".into(),
            },
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: vec![1, 2, 3, 4],
        },
        Cmd::CreateFence(8),
        Cmd::WaitFence { id: 8, value: 1 },
        Cmd::DestroyBuffer(1),
    ]
}

/// Byte-exact snapshot of `hl_gpu::Encoder::stream(stream_a())`, captured from the shipping `hl-gpu`.
const GOLDEN_A: &[u8] = &[
    0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x21, 0x00, 0x00,
    0x00, 0x02, 0x00, 0x00, 0x00, 0x76, 0x62, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x11, 0x08, 0x00, 0x00,
    0x00, 0x14, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x01,
    0x00, 0x00, 0x00,
];

/// Stream B: texture + surface + a Submit command buffer with a clear render pass + present.
fn stream_b() -> Vec<Cmd> {
    vec![
        Cmd::CreateTexture(
            2,
            TextureDesc {
                width: 64,
                height: 32,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format: TextureFormat::Bgra8Unorm,
                usage: texture_usage::RENDER_TARGET | texture_usage::PRESENT,
                label: "rt".into(),
            },
        ),
        Cmd::CreateSurface(
            7,
            SurfaceDesc {
                width: 64,
                height: 32,
                format: TextureFormat::Bgra8Unorm,
                token: hl_gpu::SurfaceToken::new(100).unwrap(),
            },
        ),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass {
                    color: vec![ColorAttachment {
                        texture: 2,
                        load: LoadOp::Clear,
                        clear: [0.1, 0.2, 0.3, 1.0],
                        store: true,
                    }],
                    depth: None,
                },
                Enc::EndRenderPass,
            ],
            signal: Some((8, 1)),
        }),
        Cmd::Present {
            surface: 7,
            texture: 2,
            serial: hl_gpu::FrameSerial::new(101).unwrap(),
        },
    ]
}

/// Byte-exact snapshot of `hl_gpu::Encoder::stream(stream_b())`, captured from the shipping `hl-gpu`.
const GOLDEN_B: &[u8] = &[
    0x04, 0x02, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
    0x00, 0x24, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x72, 0x74, 0x0f, 0x07, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x13, 0x02, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x02, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x9a, 0x99, 0x99, 0x99, 0x99, 0x99, 0xb9, 0x3f, 0x9a, 0x99,
    0x99, 0x99, 0x99, 0x99, 0xc9, 0x3f, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0xd3, 0x3f, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0xf0, 0x3f, 0x01, 0x00, 0x02, 0x01, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x15, 0x07, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x65,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// Stream C: a SPIR-V shader + a neutral kernel-descriptor shader — the shader-magic classification path.
/// Snapshot captured from the shipping `hl-gpu` (`ptx::KernelDescriptor::to_words`).
const GOLDEN_C: &[u8] = &[
    0x08, 0x03, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x03, 0x02, 0x23, 0x07, 0x00, 0x00, 0x01,
    0x00, 0x2a, 0x00, 0x00, 0x00, 0x08, 0x04, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00,
    0x6b, 0xdd, 0x16, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x78, 0x01, 0x00, 0x00, 0x00, 0x65,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
];

fn stream_c() -> Vec<Cmd> {
    use hl_gpu::protocol::model::kernel::KernelDescriptor;
    let kd = KernelDescriptor {
        ptx: "x".into(),
        entry: "e".into(),
        block: [1, 1, 1],
    };
    vec![
        Cmd::CreateShader {
            id: 3,
            kind: ShaderPayloadKind::SpirV,
            spirv: vec![0x0723_0203, 0x0001_0000, 42],
        },
        Cmd::CreateShader {
            id: 4,
            kind: ShaderPayloadKind::PtxKernel,
            spirv: kd.to_words(),
        },
    ]
}

#[test]
fn golden_a_bytes_are_stable() {
    assert_eq!(hl_gpu::Encoder::stream(&stream_a()), GOLDEN_A);
    // and the snapshot decodes back to the source stream.
    assert_eq!(hl_gpu::Decoder::stream(GOLDEN_A).unwrap(), stream_a());
}

#[test]
fn golden_b_bytes_are_stable() {
    assert_eq!(hl_gpu::Encoder::stream(&stream_b()), GOLDEN_B);
    assert_eq!(hl_gpu::Decoder::stream(GOLDEN_B).unwrap(), stream_b());
}

#[test]
fn golden_c_shader_magic_bytes_are_stable() {
    assert_eq!(hl_gpu::Encoder::stream(&stream_c()), GOLDEN_C);
    // decoding the shipping-hl-gpu bytes classifies both shader payloads correctly here.
    let back = hl_gpu::Decoder::stream(GOLDEN_C).unwrap();
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
}

/// Stream D: a forwarded GLSL vertex shader — the `Glsl` payload channel added at WIRE_VERSION 6. The
/// payload leads with `GLSL_MAGIC` and carries `(stage, entry, source)`; the decoder re-derives the kind
/// from the magic exactly as it does for SPIR-V / kernel. Frozen so a wire change to this new channel is a
/// deliberate, reviewed edit.
const GOLDEN_D: &[u8] = &[
    0x08, 0x05, 0x00, 0x00, 0x00, 0x0d, 0x00, 0x00, 0x00, 0x01, 0x00, 0x67, 0xdd, 0x2c, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x76, 0x6d, 0x61, 0x69, 0x6e, 0x1b, 0x00,
    0x00, 0x00, 0x23, 0x76, 0x65, 0x72, 0x73, 0x69, 0x6f, 0x6e, 0x20, 0x34, 0x36, 0x30, 0x0a, 0x76,
    0x6f, 0x69, 0x64, 0x20, 0x6d, 0x61, 0x69, 0x6e, 0x28, 0x29, 0x7b, 0x7d, 0x0a,
];

fn stream_d() -> Vec<Cmd> {
    use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
    let gd = GlslDescriptor {
        stage: glsl_stage::VERTEX,
        entry: "vmain".into(),
        source: "#version 460\nvoid main(){}\n".into(),
    };
    vec![Cmd::CreateShader {
        id: 5,
        kind: ShaderPayloadKind::Glsl,
        spirv: gd.to_words(),
    }]
}

#[test]
fn golden_d_glsl_payload_bytes_are_stable() {
    assert_eq!(hl_gpu::Encoder::stream(&stream_d()), GOLDEN_D);
    // The magic-led payload round-trips AND classifies as Glsl on decode.
    assert_eq!(hl_gpu::Decoder::stream(GOLDEN_D).unwrap(), stream_d());
    let back = hl_gpu::Decoder::stream(GOLDEN_D).unwrap();
    assert!(matches!(
        back[0],
        Cmd::CreateShader {
            kind: ShaderPayloadKind::Glsl,
            ..
        }
    ));
}

#[test]
fn capability_handshake_round_trips() {
    use hl_gpu::protocol::codec::wire::Encoder;
    use hl_gpu::Capabilities;
    let caps = Capabilities::permissive_fixture("golden-backend");
    // The encoded body is exactly the framed handshake's payload. Decoding is only offered framed (the
    // trailing format half is presence-gated on `remaining()`), so this leg checks the body bytes and the
    // leg below checks the round-trip.
    let mut e = Encoder::new();
    caps.encode(&mut e);
    let body = e.into_vec();
    let frame = caps.to_handshake();
    assert_eq!(
        u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize,
        body.len()
    );
    assert_eq!(&frame[4..], body.as_slice());
    // framed handshake round-trip (u32 length + body)
    let frame = caps.to_handshake();
    assert_eq!(Capabilities::from_handshake(&frame).unwrap(), caps);
}
