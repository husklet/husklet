//! Golden byte-vector snapshots + wire-compatibility guard.
//!
//! The expected byte arrays below were captured from the SHIPPING `hl-gpu` crate encoding the identical
//! command streams (verified byte-for-byte during authoring). Any change to the wire layout that would
//! desync this staging crate from the deployed `hl-gpu` breaks these tests. [`WIRE_VERSION`] is asserted
//! explicitly so a version bump is a deliberate, reviewed change.

use hl_gpu::protocol::model::command::*;
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::{decode_stream, encode_stream, WIRE_VERSION};

#[test]
fn wire_version_is_pinned_at_4() {
    // A change here must be intentional: it is the negotiated handshake version that keeps a stale
    // guest/backend pair from reinterpreting a tag it predates.
    assert_eq!(WIRE_VERSION, 4);
}

/// Stream A: buffer create + write + fence create + wait + destroy.
fn stream_a() -> Vec<Cmd> {
    vec![
        Cmd::CreateBuffer(1, BufferDesc { size: 256, usage: buffer_usage::VERTEX | buffer_usage::COPY_DST, label: "vb".into() }),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vec![1, 2, 3, 4] },
        Cmd::CreateFence(8),
        Cmd::WaitFence { id: 8, value: 1 },
        Cmd::DestroyBuffer(1),
    ]
}

/// Byte-exact snapshot of `encode_stream(stream_a())`, captured from the shipping `hl-gpu`.
const GOLDEN_A: &[u8] = &[
    0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x21, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x76, 0x62, 0x03,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x04, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x11, 0x08, 0x00, 0x00,
    0x00, 0x14, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x02, 0x01, 0x00, 0x00, 0x00,
];

/// Stream B: texture + surface + a Submit command buffer with a clear render pass + present.
fn stream_b() -> Vec<Cmd> {
    vec![
        Cmd::CreateTexture(2, TextureDesc { width: 64, height: 32, depth: 1, mip_levels: 1, sample_count: 1, dim: TextureDim::D2, format: TextureFormat::Bgra8Unorm, usage: texture_usage::RENDER_TARGET | texture_usage::PRESENT, label: "rt".into() }),
        Cmd::CreateSurface(7, SurfaceDesc { width: 64, height: 32, format: TextureFormat::Bgra8Unorm, hlp_surface: 100 }),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginRenderPass { color: vec![ColorAttachment { texture: 2, load: LoadOp::Clear, clear: [0.1, 0.2, 0.3, 1.0], store: true }], depth: None },
                Enc::EndRenderPass,
            ],
            signal: Some((8, 1)),
        }),
        Cmd::Present { surface: 7, texture: 2 },
    ]
}

/// Byte-exact snapshot of `encode_stream(stream_b())`, captured from the shipping `hl-gpu`.
const GOLDEN_B: &[u8] = &[
    0x04, 0x02, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
    0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x24, 0x00, 0x00,
    0x00, 0x02, 0x00, 0x00, 0x00, 0x72, 0x74, 0x0f, 0x07, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    0x64, 0x00, 0x00, 0x00, 0x13, 0x02, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0xcd, 0xcc,
    0xcc, 0x3d, 0xcd, 0xcc, 0x4c, 0x3e, 0x9a, 0x99, 0x99, 0x3e, 0x00, 0x00,
    0x80, 0x3f, 0x01, 0x00, 0x02, 0x01, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x15, 0x07, 0x00, 0x00, 0x00, 0x02,
    0x00, 0x00, 0x00,
];

/// Stream C: a SPIR-V shader + a neutral kernel-descriptor shader — the shader-magic classification path.
/// Snapshot captured from the shipping `hl-gpu` (`ptx::KernelDescriptor::to_words`).
const GOLDEN_C: &[u8] = &[
    0x08, 0x03, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x03, 0x02, 0x23,
    0x07, 0x00, 0x00, 0x01, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x08, 0x04, 0x00,
    0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x6b, 0xdd, 0x16, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x78, 0x01, 0x00, 0x00, 0x00, 0x65,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00,
];

fn stream_c() -> Vec<Cmd> {
    use hl_gpu::protocol::model::kernel::KernelDescriptor;
    let kd = KernelDescriptor { ptx: "x".into(), entry: "e".into(), block: [1, 1, 1] };
    vec![
        Cmd::CreateShader { id: 3, kind: ShaderPayloadKind::SpirV, spirv: vec![0x0723_0203, 0x0001_0000, 42] },
        Cmd::CreateShader { id: 4, kind: ShaderPayloadKind::PtxKernel, spirv: kd.to_words() },
    ]
}

#[test]
fn golden_a_bytes_are_stable() {
    assert_eq!(encode_stream(&stream_a()), GOLDEN_A);
    // and the snapshot decodes back to the source stream.
    assert_eq!(decode_stream(GOLDEN_A).unwrap(), stream_a());
}

#[test]
fn golden_b_bytes_are_stable() {
    assert_eq!(encode_stream(&stream_b()), GOLDEN_B);
    assert_eq!(decode_stream(GOLDEN_B).unwrap(), stream_b());
}

#[test]
fn golden_c_shader_magic_bytes_are_stable() {
    assert_eq!(encode_stream(&stream_c()), GOLDEN_C);
    // decoding the shipping-hl-gpu bytes classifies both shader payloads correctly here.
    let back = decode_stream(GOLDEN_C).unwrap();
    assert!(matches!(back[0], Cmd::CreateShader { kind: ShaderPayloadKind::SpirV, .. }));
    assert!(matches!(back[1], Cmd::CreateShader { kind: ShaderPayloadKind::PtxKernel, .. }));
}

#[test]
fn capability_handshake_round_trips() {
    use hl_gpu::protocol::codec::wire::{Decoder, Encoder};
    use hl_gpu::Capabilities;
    let caps = Capabilities::full("golden-backend");
    // inline encode/decode
    let mut e = Encoder::new();
    caps.encode(&mut e);
    let bytes = e.into_vec();
    let mut d = Decoder::new(&bytes);
    assert_eq!(Capabilities::decode(&mut d).unwrap(), caps);
    // framed handshake round-trip (u32 length + body)
    let frame = caps.to_handshake();
    assert_eq!(Capabilities::from_handshake(&frame).unwrap(), caps);
}
