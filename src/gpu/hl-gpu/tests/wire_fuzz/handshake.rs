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
    let good = Capabilities::full("truncate-me").to_handshake();
    // The intact handshake decodes to exactly the source caps.
    assert_eq!(
        Capabilities::from_handshake(&good).unwrap(),
        Capabilities::full("truncate-me")
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
    let good = Capabilities::full("flip-me").to_handshake();
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
        let mut caps = Capabilities::full("mismatch");
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
    let caps = Capabilities::full("match");
    let req = FeatureRequest {
        wire_version: caps.wire_version,
        ..Default::default()
    };
    assert_eq!(caps.negotiate(&req), Ok(()));
}

// ---------------------------------------------------------------------------------------------------
// 3. end-to-end: hostile bytes → decode_stream → runtime::submit (decode → validate → account → dispatch)
// ---------------------------------------------------------------------------------------------------
