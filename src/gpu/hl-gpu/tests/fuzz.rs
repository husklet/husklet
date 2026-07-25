//! Decoder robustness fuzz: the wire boundary faces UNTRUSTED guest bytes, so `decode_stream` must
//! never panic (out-of-bounds, huge length prefixes, bad tags) — it must return `Err` cleanly. This
//! complements the targeted `decode_rejects_truncation_and_bad_tags` case with broad coverage:
//! deterministic pseudo-random inputs, bit-flip mutations of valid streams, and every truncation prefix.
//! No `Math.random`/time — a fixed LCG keeps it reproducible.

use hl_gpu::protocol::model::command::*;
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use std::panic::catch_unwind;

/// Reproducible byte generator (SplitMix64-ish over an LCG); no external RNG.
fn lcg(state: &mut u64) -> u8 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*state >> 56) as u8
}

/// The decoder must return a value (Ok or Err) for ANY input — never unwind.
fn assert_no_panic(bytes: &[u8]) {
    let owned = bytes.to_vec();
    let r = catch_unwind(move || {
        let _ = hl_gpu::Decoder::stream(&owned);
    });
    assert!(
        r.is_ok(),
        "decode_stream PANICKED on {} bytes: {:02x?}",
        bytes.len(),
        bytes
    );
}

fn representative_streams() -> Vec<Vec<Cmd>> {
    vec![
        vec![
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 256,
                    usage: buffer_usage::VERTEX | buffer_usage::COPY_DST,
                    label: "b".into(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: vec![1, 2, 3, 4, 5, 6, 7, 8],
            },
            Cmd::CreateFence(8),
            Cmd::WaitFence { id: 8, value: 1 },
            Cmd::DestroyBuffer(1),
        ],
        vec![
            Cmd::CreateTexture(
                2,
                TextureDesc {
                    width: 4,
                    height: 4,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    format: TextureFormat::Rgba8Unorm,
                    dim: TextureDim::D2,
                    usage: texture_usage::COPY_DST | texture_usage::SAMPLED,
                    label: "t".into(),
                },
            ),
            Cmd::Present {
                surface: 2,
                texture: 2,
            },
            Cmd::DestroyTexture(2),
        ],
    ]
}

#[test]
fn decode_never_panics_on_random_bytes() {
    let mut state = 0x1234_5678_9abc_def0u64;
    for i in 0..20_000u32 {
        let len = (lcg(&mut state) as usize) % 300;
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            bytes.push(lcg(&mut state));
        }
        // Occasionally slam a giant little-endian length word up front to probe prealloc/overflow paths.
        if i % 97 == 0 && bytes.len() >= 5 {
            bytes[1..5].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        }
        assert_no_panic(&bytes);
    }
}

#[test]
fn decode_never_panics_on_bitflipped_valid_streams() {
    for stream in representative_streams() {
        let good = hl_gpu::Encoder::stream(&stream);
        // A valid stream must decode cleanly...
        assert!(
            hl_gpu::Decoder::stream(&good).is_ok(),
            "valid stream failed to decode"
        );
        // ...and every single-byte corruption must fail-or-decode, never panic.
        let mut state = 0xC0FFEE_u64;
        for pos in 0..good.len() {
            for _ in 0..4 {
                let mut bad = good.clone();
                bad[pos] ^= 1 << (lcg(&mut state) % 8);
                assert_no_panic(&bad);
            }
        }
    }
}

#[test]
fn decode_never_panics_on_truncations() {
    for stream in representative_streams() {
        let good = hl_gpu::Encoder::stream(&stream);
        for cut in 0..=good.len() {
            assert_no_panic(&good[..cut]);
        }
    }
}
