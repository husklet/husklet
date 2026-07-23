use super::*;

#[test]
fn every_command_and_op_value_round_trips() {
    let s = every_command();
    let bytes = hl_gpu::Encoder::stream(&s);
    assert_eq!(
        hl_gpu::Decoder::stream(&bytes).unwrap(),
        s,
        "the full tag/etag inventory round-trips by value"
    );
}

#[test]
fn full_inventory_is_byte_stable() {
    // encode(decode(encode(x))) == encode(x): the decoder consumes exactly and re-encodes identically.
    let bytes = hl_gpu::Encoder::stream(&every_command());
    let decoded = hl_gpu::Decoder::stream(&bytes).unwrap();
    assert_eq!(
        hl_gpu::Encoder::stream(&decoded),
        bytes,
        "decode∘encode is byte-stable across every op"
    );
}

#[test]
fn truncating_each_op_at_every_prefix_never_panics() {
    // Each encoder op alone, wrapped in a Submit; truncate the bytes at every prefix — no panic, and the
    // untruncated form round-trips exactly.
    for op in every_encoder_op() {
        let cb = CommandBuffer {
            encoder: vec![op.clone()],
            signal: None,
        };
        let bytes = hl_gpu::Encoder::stream(&[Cmd::Submit(cb.clone())]);
        assert_eq!(
            hl_gpu::Decoder::stream(&bytes).unwrap(),
            vec![Cmd::Submit(cb)],
            "op {op:?} round-trips"
        );
        for cut in 0..bytes.len() {
            let _ = no_panic(&bytes[..cut]); // Err is fine; a panic is not.
        }
    }
}

#[test]
fn truncating_each_command_at_every_prefix_never_panics() {
    for cmd in every_command() {
        let bytes = hl_gpu::Encoder::stream(std::slice::from_ref(&cmd));
        assert_eq!(
            hl_gpu::Decoder::stream(&bytes).unwrap(),
            vec![cmd.clone()],
            "cmd round-trips"
        );
        for cut in 0..bytes.len() {
            let _ = no_panic(&bytes[..cut]);
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// 2. byte-stability under adversarial mutation — the core invariant: ANY decodable bytes re-encode to
//    themselves. A failure here is a real normalization/desync bug.
// ---------------------------------------------------------------------------------------------------
