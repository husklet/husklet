use super::*;

#[test]
fn tracking_allocator_detects_a_real_large_allocation() {
    // Integrity guard: prove the tracking allocator actually observes a deliberate 64 MiB allocation, so
    // the bounded-growth assertions below cannot pass vacuously (e.g. if TLS tracking silently no-oped).
    let (_, growth) = peak_growth_during(|| {
        let v = vec![0u8; 64 << 20];
        std::hint::black_box(v.len())
    });
    assert!(
        growth >= 64 << 20,
        "allocator failed to observe a 64 MiB allocation (growth={growth})"
    );
}

#[test]
fn every_count_field_is_a_bounded_error_not_a_decode_bomb() {
    for (name, bytes) in command_bombs() {
        // The bytes must be tiny so cloning/setup contributes nothing to the measured growth.
        assert!(
            bytes.len() < 64,
            "{name}: bomb frame should be tiny, got {} bytes",
            bytes.len()
        );
        let (result, growth) = peak_growth_during(|| hl_gpu::Decoder::stream(&bytes));
        assert!(
            result.is_err(),
            "{name}: a count claiming ~4 billion entries with no backing bytes must error, got Ok"
        );
        assert!(
            growth < BOMB_ALLOC_CEIL,
            "{name}: decoding grew the heap by {growth} bytes (>= {BOMB_ALLOC_CEIL}) — a decode-bomb \
             prealloc regression (a bounded cap_count reservation was likely replaced by a raw \
             Vec::with_capacity(attacker_count))"
        );
    }
}

#[test]
fn handshake_name_length_is_a_bounded_error_not_a_decode_bomb() {
    // A handshake body whose name string claims ~4 billion bytes with none following.
    let mut body = Encoder::new();
    body.u32(WIRE_VERSION);
    body.u32(BOMB); // name str length; nothing follows
    let mut e = Encoder::new();
    e.bytes(&body.into_vec()); // wrap as a (u32 length + body) handshake frame
    let bytes = e.into_vec();

    let (result, growth) = peak_growth_during(|| Capabilities::from_handshake(&bytes));
    assert!(
        result.is_err(),
        "a handshake name-length bomb must error, got Ok"
    );
    assert!(
        growth < BOMB_ALLOC_CEIL,
        "handshake name decode grew the heap by {growth} bytes (>= {BOMB_ALLOC_CEIL}) — decode-bomb regression"
    );
}

// ---------------------------------------------------------------------------------------------------
// 2. capability-handshake robustness (truncation, fuzz, version mismatch)
// ---------------------------------------------------------------------------------------------------
