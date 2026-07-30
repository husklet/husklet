use super::*;

fn no_panic_handshake(bytes: &[u8]) {
    let owned = bytes.to_vec();
    let r = catch_unwind(move || {
        let _ = Capabilities::from_handshake(&owned);
    });
    assert!(
        r.is_ok(),
        "from_handshake PANICKED on {} bytes: {:02x?}",
        bytes.len(),
        bytes
    );
}

#[test]
fn handshake_truncated_at_every_prefix_never_panics() {
    let good = Capabilities::permissive_fixture("truncate-me").to_handshake();
    // The intact handshake decodes to exactly the source caps.
    assert_eq!(
        Capabilities::from_handshake(&good).unwrap(),
        Capabilities::permissive_fixture("truncate-me")
    );
    for cut in 0..good.len() {
        no_panic_handshake(&good[..cut]); // Err is fine; a panic/hang is not.
    }
}

#[test]
fn handshake_random_and_bitflipped_bytes_never_panic() {
    // Random bytes of varied length.
    let mut state = 0x00C0_FFEE_1234_5678u64;
    for _ in 0..20_000u32 {
        let len = (lcg(&mut state) as usize) % 96;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            bytes.push(lcg(&mut state));
        }
        no_panic_handshake(&bytes);
    }
    // Bit-flips of a valid handshake at every position (present-bit and unknown-field mutations may
    // decode to a DIFFERENT-but-valid descriptor; we assert only that decode never panics — a handshake
    // is not byte-stable under mutation because unknown present bits are dropped on re-encode).
    let good = Capabilities::permissive_fixture("flip-me").to_handshake();
    for pos in 0..good.len() {
        for bit in 0..8u32 {
            let mut bad = good.clone();
            bad[pos] ^= 1 << bit;
            no_panic_handshake(&bad);
        }
    }
}

#[test]
fn handshake_wire_version_mismatch_is_a_typed_version_error() {
    // A backend advertising a version the guest does not speak must fail cleanly at negotiation with a
    // typed version error — NOT a later runtime BadTag after the app committed to a path. Both the
    // too-new and too-old directions, and a guest that is itself newer than the backend, are rejected.
    for backend_version in [WIRE_VERSION + 1, WIRE_VERSION - 1, WIRE_VERSION + 7] {
        let mut caps = Capabilities::permissive_fixture("mismatch");
        caps.wire_version = backend_version;
        // The handshake still DECODES structurally (the version check is a negotiation concern).
        let bytes = caps.to_handshake();
        let decoded = Capabilities::from_handshake(&bytes).expect("handshake decodes structurally");
        assert_eq!(decoded.wire_version, backend_version);
        // A guest pinned at WIRE_VERSION negotiating against it gets a typed Unsupported version error.
        let req = FeatureRequest {
            wire_version: WIRE_VERSION,
            ..Default::default()
        };
        assert_eq!(
            decoded.negotiate(&req),
            Err(GpuError::Unsupported("capability: wire version mismatch")),
            "backend v{backend_version} vs guest v{WIRE_VERSION} must be a typed version mismatch"
        );
    }
    // The matching version negotiates cleanly (the mismatch check is not over-eager).
    let caps = Capabilities::permissive_fixture("match");
    let req = FeatureRequest {
        wire_version: caps.wire_version,
        ..Default::default()
    };
    assert_eq!(caps.negotiate(&req), Ok(()));
}

// ---------------------------------------------------------------------------------------------------
// 3. end-to-end: hostile bytes → decode_stream → runtime::submit (decode → validate → account → dispatch)
// ---------------------------------------------------------------------------------------------------

/// The format bitset must survive the handshake across its FULL width. `texture_formats` is keyed by
/// `TextureFormat::to_u32()` and 25 of its slots are already used; while the bitset was 32 bits wide the
/// 33rd format would have been silently un-advertisable, with `supports_format` quietly answering false and
/// no error anywhere. A high bit therefore has to round-trip, and the encoding must stay BYTE-IDENTICAL for
/// a descriptor that uses only the low 32 — that is what keeps a pinned older guest working.
#[test]
fn a_high_format_bit_survives_the_handshake_and_low_only_stays_byte_identical() {
    let mut caps = Capabilities::permissive_fixture("wide");

    // A descriptor using only the low 32 slots encodes exactly as it always did: no tail word.
    let low_only = caps.to_handshake();
    let low_len = low_only.len();
    assert_eq!(
        Capabilities::from_handshake(&low_only).unwrap(),
        caps,
        "a low-only descriptor must round-trip"
    );

    // A format above bit 31 round-trips rather than being truncated away.
    caps.texture_formats |= 1u64 << 40;
    let wide = caps.to_handshake();
    let decoded = Capabilities::from_handshake(&wide).unwrap();
    assert_eq!(
        decoded.texture_formats, caps.texture_formats,
        "the high half of the format bitset must survive the handshake"
    );
    assert_eq!(decoded, caps);

    // The wide form costs exactly one extra 4-byte word, and ONLY when a high bit is set — so the byte
    // stream a guest that predates the widening sees is unchanged until a format it cannot name exists.
    assert_eq!(
        wide.len(),
        low_len + 4,
        "the high half is an optional tail, not a reshaped message"
    );
}

/// Representability guard: every `TextureFormat` the IR declares must fit the advertised bitset and be
/// recognized by `supports_format`. This is the test that fails loudly when someone adds the format that
/// overflows the bitset, instead of it being silently dropped at runtime.
#[test]
fn every_declared_texture_format_is_representable_in_the_bitset() {
    use hl_gpu::protocol::model::enums::TextureFormat;
    // Every discriminant the enum declares, walked by value so a new variant is picked up automatically.
    let declared: Vec<TextureFormat> = (0..=u32::from(u16::MAX))
        .filter_map(|v| TextureFormat::from_u32(v).ok())
        .collect();
    assert!(
        declared.len() >= 25,
        "the format list should not have shrunk"
    );

    let mut caps = Capabilities::permissive_fixture("all");
    caps.texture_formats = TextureFormat::bits(&declared);
    for format in &declared {
        assert!(
            caps.supports_format(*format),
            "{format:?} is not representable in the advertised bitset"
        );
    }
    assert_eq!(
        caps.texture_formats.count_ones() as usize,
        declared.len(),
        "every declared format must occupy its own bit — none silently dropped"
    );
}
