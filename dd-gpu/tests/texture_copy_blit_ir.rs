//! Wire-encoding contract for the v2 texture-to-texture copy + blit IR ops (Phase-3 slice).
//!
//! These pin the malformed-stream guarantees §3 of `docs/codex-rendering.md` requires for every new tag:
//! the new `Enc::CopyTextureToTexture` / `Enc::BlitTexture` round-trip byte-exactly, every truncated prefix
//! is rejected without panic, and a bogus `TextureAspect`/`Filter` enum value is a typed `GpuError` rather
//! than undefined behavior. A stale (v1) decoder rejecting the new etags as `BadTag` is the standing
//! "never let a stale guest/backend pair interpret a new tag" guard until the connection handshake lands.

use dd_gpu::ir::*;
use dd_gpu::wire::Encoder;
use dd_gpu::GpuError;

fn copy_op() -> Enc {
    Enc::CopyTextureToTexture {
        src: 1,
        src_sub: TextureSubresource { mip: 0, layer: 0, aspect: TextureAspect::All },
        src_origin: Origin3d { x: 1, y: 2, z: 0 },
        dst: 2,
        dst_sub: TextureSubresource { mip: 0, layer: 0, aspect: TextureAspect::All },
        dst_origin: Origin3d { x: 3, y: 4, z: 0 },
        extent: Extent3d { width: 5, height: 6, depth: 1 },
    }
}

fn blit_op() -> Enc {
    Enc::BlitTexture {
        src: 7,
        src_sub: TextureSubresource { mip: 0, layer: 0, aspect: TextureAspect::All },
        src_origin: Origin3d { x: 0, y: 0, z: 0 },
        src_extent: Extent3d { width: 2, height: 2, depth: 1 },
        dst: 8,
        dst_sub: TextureSubresource { mip: 0, layer: 0, aspect: TextureAspect::All },
        dst_origin: Origin3d { x: 4, y: 4, z: 0 },
        dst_extent: Extent3d { width: 8, height: 8, depth: 1 },
        filter: Filter::Linear,
    }
}

fn submit_frame(ops: Vec<Enc>) -> Vec<u8> {
    encode_stream(&[Cmd::Submit(CommandBuffer { encoder: ops, signal: None })])
}

#[test]
fn wire_version_includes_typed_shader_payloads_after_v2_copy_ops() {
    assert_eq!(WIRE_VERSION, 3, "typed shader payload origins add the third wire version");
}

#[test]
fn copy_and_blit_ops_round_trip_byte_exactly() {
    let cmds = vec![Cmd::Submit(CommandBuffer { encoder: vec![copy_op(), blit_op()], signal: None })];
    let bytes = encode_stream(&cmds);
    assert_eq!(decode_stream(&bytes).expect("decode v2 copy/blit"), cmds);
}

#[test]
fn every_truncated_prefix_of_copy_and_blit_is_rejected_without_panic() {
    for op in [copy_op(), blit_op()] {
        let encoded = submit_frame(vec![op]);
        for cut in 1..encoded.len() {
            assert!(decode_stream(&encoded[..cut]).is_err(), "accepted truncated prefix {cut}/{}", encoded.len());
        }
    }
}

#[test]
fn bogus_texture_aspect_enum_is_a_typed_error_not_ub() {
    // Encode a CopyTextureToTexture by hand with an out-of-range src aspect (99) and confirm the decoder
    // rejects it rather than materializing an invalid enum.
    let mut e = Encoder::new();
    // Submit(tag 19 wait no) — build the frame: encode_stream form is tag+body. Cmd::Submit tag = 19 is a
    // top-level command; the encoder body is a length-prefixed... no: Submit uses enc_command_buffer with a
    // u32 op count then ops. Reproduce: top-level SUBMIT, one op, COPY_T2T etag, then fields.
    e.u8(19); // tag::SUBMIT
    e.u32(1); // one encoder op
    e.u8(18); // etag::COPY_T2T
    e.u32(1); // src
    e.u32(0); // src mip
    e.u32(0); // src layer
    e.u32(99); // src aspect — invalid
    // (decode fails here before reading the rest)
    assert!(matches!(
        decode_stream(&e.into_vec()),
        Err(GpuError::Decode(msg)) if msg.contains("TextureAspect")
    ));
}

#[test]
fn stale_v1_decoder_rejects_the_new_etags_as_bad_tag() {
    // A hand-built Submit whose single op carries an etag one past the v2 set (20) must be rejected — the
    // standing guarantee that a decoder never silently reinterprets a tag it predates.
    let mut e = Encoder::new();
    e.u8(19); // SUBMIT
    e.u32(1); // one op
    e.u8(20); // unknown etag
    assert!(matches!(decode_stream(&e.into_vec()), Err(GpuError::Decode(msg)) if msg.contains("bad command/encoder tag 20")));
}

#[test]
fn decoder_reads_exactly_the_bytes_the_encoder_wrote() {
    // A frame with a valid copy op plus one trailing junk byte must fail rather than accept-and-discard.
    let mut bytes = submit_frame(vec![copy_op()]);
    bytes.push(0xAB);
    assert!(decode_stream(&bytes).is_err(), "trailing junk after a copy op was silently accepted");
}
