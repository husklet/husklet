//! Round-trip coverage the canonical inventory in `every_encoder_op` cannot give.
//!
//! That inventory uses ONE shared `TextureSubresource::base()` (all zeros), one `Origin3d::default()` (all
//! zeros) and one shared extent for both the source and the destination of every region op. A value
//! round-trip over identical, mostly-zero operands is blind to field TRANSPOSITION: swapping `mip` with
//! `layer`, `x` with `y`, or the src selector with the dst selector in the encoder/decoder pair still
//! round-trips. These cases give every field a distinct value so a transposition is observable, and pin the
//! inventory's completeness against the negotiated command set.

use super::*;
use hl_gpu::protocol::model::descriptor::Mirror;

/// Distinct subresource / origin / extent triples so no two fields share a value.
fn sub(mip: u32, layer: u32, aspect: TextureAspect) -> TextureSubresource {
    TextureSubresource { mip, layer, aspect }
}

fn origin(x: u32, y: u32, z: u32) -> Origin3d {
    Origin3d { x, y, z }
}

fn extent(width: u32, height: u32, depth: u32) -> Extent3d {
    Extent3d {
        width,
        height,
        depth,
    }
}

/// One value of every region-addressing op, with EVERY field distinct.
fn distinct_region_ops() -> Vec<Enc> {
    vec![
        Enc::CopyBufferToTextureRegion {
            src: 11,
            src_offset: 12,
            bytes_per_row: 13,
            rows_per_image: 14,
            dst: 15,
            dst_sub: sub(16, 17, TextureAspect::StencilOnly),
            dst_origin: origin(18, 19, 20),
            extent: extent(21, 22, 23),
        },
        Enc::CopyTextureToBufferRegion {
            src: 31,
            src_sub: sub(32, 33, TextureAspect::DepthOnly),
            src_origin: origin(34, 35, 36),
            extent: extent(37, 38, 39),
            dst: 40,
            dst_offset: 41,
            bytes_per_row: 42,
            rows_per_image: 43,
        },
        Enc::CopyTextureToTexture {
            src: 51,
            src_sub: sub(52, 53, TextureAspect::DepthOnly),
            src_origin: origin(54, 55, 56),
            dst: 57,
            dst_sub: sub(58, 59, TextureAspect::StencilOnly),
            dst_origin: origin(60, 61, 62),
            extent: extent(63, 64, 65),
        },
        Enc::BlitTexture {
            src: 71,
            src_sub: sub(72, 73, TextureAspect::DepthOnly),
            src_origin: origin(74, 75, 76),
            src_extent: extent(77, 78, 79),
            dst: 80,
            dst_sub: sub(81, 82, TextureAspect::StencilOnly),
            dst_origin: origin(83, 84, 85),
            dst_extent: extent(86, 87, 88),
            filter: Filter::Linear,
            // Asymmetric on purpose: the two mirror bits must not be transposed or collapsed on the wire.
            mirror: Mirror { x: true, y: false },
        },
        Enc::ResolveTexture {
            src: 91,
            src_sub: sub(92, 93, TextureAspect::DepthOnly),
            src_origin: origin(94, 95, 96),
            dst: 97,
            dst_sub: sub(98, 99, TextureAspect::StencilOnly),
            dst_origin: origin(100, 101, 102),
            extent: extent(103, 104, 105),
        },
    ]
}

#[test]
fn region_ops_round_trip_with_every_field_distinct() {
    for op in distinct_region_ops() {
        let batch = vec![Cmd::Submit(CommandBuffer {
            encoder: vec![op.clone()],
            signal: None,
        })];
        let bytes = hl_gpu::Encoder::stream(&batch);
        assert_eq!(
            hl_gpu::Decoder::stream(&bytes).unwrap(),
            batch,
            "{op:?} must round-trip with no field transposed"
        );
    }
}

/// The other transposition-blind pair: a copy whose two buffer offsets, ids and size are all distinct.
#[test]
fn buffer_copy_operands_round_trip_without_transposition() {
    let batch = vec![Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::CopyBufferToBuffer {
                src: 1,
                src_offset: 2,
                dst: 3,
                dst_offset: 4,
                size: 5,
            },
            Enc::CopyBufferToTexture {
                src: 6,
                src_offset: 7,
                bytes_per_row: 8,
                dst: 9,
                mip: 10,
                width: 11,
                height: 12,
            },
            Enc::CopyTextureToBuffer {
                src: 13,
                mip: 14,
                width: 15,
                height: 16,
                dst: 17,
                dst_offset: 18,
                bytes_per_row: 19,
            },
            Enc::SetViewport {
                x: 1.0,
                y: 2.0,
                w: 3.0,
                h: 4.0,
                min_depth: 0.25,
                max_depth: 0.75,
            },
            Enc::SetScissor {
                x: 20,
                y: 21,
                w: 22,
                h: 23,
            },
        ],
        signal: Some((24, 25)),
    })];
    let bytes = hl_gpu::Encoder::stream(&batch);
    assert_eq!(hl_gpu::Decoder::stream(&bytes).unwrap(), batch);
}

/// Completeness: the round-trip inventory must cover EVERY encoder op the capability descriptor
/// advertises. A new etag added to `ALL_COMMANDS` without an inventory entry would otherwise ship with no
/// round-trip, truncation or byte-stability coverage at all.
#[test]
fn the_op_inventory_covers_every_negotiated_command() {
    let mut covered: Vec<u8> = every_encoder_op().iter().map(Enc::wire_tag).collect();
    covered.sort_unstable();
    covered.dedup();
    let mut advertised = hl_gpu::protocol::model::capability::ALL_COMMANDS.to_vec();
    advertised.sort_unstable();
    assert_eq!(
        covered, advertised,
        "every advertised encoder op needs a round-trip inventory entry"
    );
}

/// Headroom guard for the OTHER advertised bitset. `command_bits` is 64 slots keyed by etag number and
/// `Capabilities::command_bits` / `supports_command` both silently drop a tag at or beyond 64 — the same
/// silent-capability-lie shape the format bitset had. 25 etags are used, so there is room; this fails the
/// moment an etag is added that the advertisement cannot name.
#[test]
fn every_negotiated_command_is_representable_in_the_bitset() {
    let advertised = hl_gpu::protocol::model::capability::ALL_COMMANDS;
    let bits = hl_gpu::Capabilities::command_bits(advertised);
    assert_eq!(
        bits.count_ones() as usize,
        advertised.len(),
        "every advertised etag must occupy its own bit — none silently dropped"
    );
    let caps = hl_gpu::Capabilities::permissive_fixture("headroom");
    for tag in advertised {
        assert!(
            caps.supports_command(*tag),
            "etag {tag} is not representable in the advertised command bitset"
        );
    }
    assert!(
        advertised.iter().all(|t| *t < 64),
        "an etag at or beyond 64 cannot be advertised at all"
    );
}
