use super::*;

/// Reproducible LCG byte source (no external RNG / time).
fn lcg(state: &mut u64) -> u8 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*state >> 56) as u8
}

#[test]
fn any_decodable_mutation_re_encodes_to_itself() {
    let base = hl_gpu::Encoder::stream(&every_command());
    let mut state = 0xA5A5_1234_DEAD_0001u64;
    let mut decodable = 0u64;
    for _ in 0..40_000u32 {
        let mut bad = base.clone();
        // Apply 1..=3 single-byte writes at random positions.
        let muts = 1 + (lcg(&mut state) % 3) as usize;
        for _ in 0..muts {
            if bad.is_empty() {
                break;
            }
            let pos = (lcg(&mut state) as usize) % bad.len();
            bad[pos] = lcg(&mut state);
        }
        // Occasionally truncate to probe the framing boundary too.
        if lcg(&mut state).is_multiple_of(8) {
            let keep = (lcg(&mut state) as usize) % (bad.len() + 1);
            bad.truncate(keep);
        }
        if let Ok(cmds) = no_panic(&bad) {
            // THE INVARIANT: a stream the decoder accepted must re-encode to the exact same bytes it
            // consumed. decode_stream drains the whole input, so equality is total, not prefix.
            assert_eq!(
                hl_gpu::Encoder::stream(&cmds),
                bad,
                "decode accepted bytes that re-encode differently (normalization/desync bug)"
            );
            decodable += 1;
        }
    }
    // Sanity: the corpus actually exercised the accept path, not only rejections.
    assert!(
        decodable > 0,
        "no mutation ever decoded — fuzz corpus is not exercising the accept path"
    );
}

#[test]
fn random_bytes_never_panic_and_are_byte_stable_when_accepted() {
    let mut state = 0x0BAD_F00D_C0FF_EE00u64;
    for _ in 0..20_000u32 {
        let len = (lcg(&mut state) as usize) % 260;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            bytes.push(lcg(&mut state));
        }
        if let Ok(cmds) = no_panic(&bytes) {
            assert_eq!(
                hl_gpu::Encoder::stream(&cmds),
                bytes,
                "accepted random bytes must be byte-stable"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// 3. typed rejection of every malformed shape
// ---------------------------------------------------------------------------------------------------
