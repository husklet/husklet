//! `cuMemcpyDtoD` is byte-granular; the on-device copy command is not.
//!
//! CUDA places no alignment requirement on either pointer or on the length of a device→device copy. The
//! [`Enc::CopyBufferToBuffer`] it lowers to does: `wgpu`'s `copy_buffer_to_buffer` requires both offsets
//! and the size to be multiples of [`Limits::DEFAULT_COPY_ALIGNMENT`], and the shared host validator
//! (`hl-gpu/src/runtime/service/validate.rs`) rejects the whole transfer before either executor sees it.
//!
//! Passing the raw extents through therefore refused every copy whose size or either offset was not a
//! multiple of four. Measured against `/Applications/Husklet.app` binary `55c957a1…`: **3488 of 3488**
//! non-word-aligned cases refused with `CUDA_ERROR_INVALID_VALUE`, against **112 of 112** word-aligned
//! cases byte-exact — on the Metal executor AND on the reference interpreter, because the check is in
//! the shared session validator rather than in either backend. Host↔device copies were unaffected
//! (300 of 300 odd sizes and offsets byte-exact), which is why the existing suites, and rung 6's 1 MiB
//! aligned round trip, never saw it.
//!
//! The host constraint is real, so `memcpy_dtod` handles the remainder instead of relaxing anything:
//! an aligned middle goes on-device as one copy, and the unaligned edges go through the byte-granular
//! readback + `WriteBuffer` pair. Two pointers at DIFFERENT offsets within their word share no aligned
//! middle at all — a device copy cannot shift bytes inside a word — so that case is entirely edges.
//!
//! The matrix below drives every combination of the two alignment families in both roles. Payloads are
//! position-dependent so a copy that is shifted, truncated or rounded to a word boundary produces wrong
//! bytes rather than identical ones, and the destination is poisoned before every copy so a copy that
//! moves nothing cannot pass on the previous contents.

use hl_cuda::service::{allocate, transfer};
use hl_cuda::{CudaContext, CudaDeviceDesc, DevicePtr};

use hl_gpu::protocol::model::capability::{shader_payload, Capabilities, COLOR_FORMATS};
use hl_gpu::protocol::model::command::etag;
use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::{
    BufferId, CommandSink, CpuExecutor, FeatureRequest, InProcessCommandSink, Limits, WIRE_VERSION,
};

const CUDA_COMMANDS: &[u8] = &[
    etag::BEGIN_COMPUTE_PASS,
    etag::END_COMPUTE_PASS,
    etag::DISPATCH,
    etag::COPY_B2B,
];

const A: u64 = Limits::DEFAULT_COPY_ALIGNMENT;

fn harness() -> InProcessCommandSink<CpuExecutor> {
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    let request = FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::KERNEL,
        command_bits: Capabilities::command_bits(CUDA_COMMANDS),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
        ..FeatureRequest::default()
    };
    sink.negotiate(&request).expect("negotiate against CpuExecutor");
    sink
}

fn readback(
    sink: &mut InProcessCommandSink<CpuExecutor>,
    ctx: &CudaContext,
    p: DevicePtr,
    len: usize,
) -> Vec<u8> {
    let (buf, off): (BufferId, u64) = ctx.device_location(p).unwrap();
    sink.read_buffer(buf, off, len).unwrap()
}

/// Depends on the payload identity AND the position, so no two cases share a byte sequence and a copy
/// off by any number of bytes lands on values that belong to a different position.
fn pattern(size: usize, offset: u64, i: usize) -> u8 {
    (i.wrapping_mul(31)
        .wrapping_add(size.wrapping_mul(17))
        .wrapping_add(offset as usize * 7)
        .wrapping_add(0x5D)) as u8
}

/// Every `(size, source offset, destination offset)` a byte-granular copy must serve. Sizes and offsets
/// deliberately mix multiples of the alignment with every non-zero residue, so the aligned family is a
/// control inside the same matrix rather than a separate test that could quietly stop running.
#[test]
fn device_to_device_is_byte_granular_at_every_alignment() {
    const SIZES: &[usize] = &[1, 2, 3, 4, 5, 7, 8, 13, 16, 31, 32, 63, 64, 127, 128, 255, 256, 257];
    const OFFSETS: &[u64] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 13, 16, 63, 64];

    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    let slab = 8192usize;
    let src_base = allocate::mem_alloc(&mut ctx, &mut sink, slab as u64).unwrap();
    let dst_base = allocate::mem_alloc(&mut ctx, &mut sink, slab as u64).unwrap();

    let mut aligned_cases = 0usize;
    let mut unaligned_cases = 0usize;

    for &size in SIZES {
        for &so in OFFSETS {
            for &dofs in OFFSETS {
                assert!(so as usize + size <= slab && dofs as usize + size <= slab);

                let payload: Vec<u8> = (0..size).map(|i| pattern(size, so, i)).collect();
                transfer::memcpy_htod(
                    &mut ctx,
                    &mut sink,
                    DevicePtr(src_base.0 + so),
                    &payload,
                )
                .unwrap();

                // Poison the destination plus a guard byte either side, so a copy that writes nothing
                // cannot pass on what was already there and one that overruns is visible.
                let poisoned = vec![0x5Au8; size + 2];
                transfer::memcpy_htod(
                    &mut ctx,
                    &mut sink,
                    DevicePtr(dst_base.0 + dofs),
                    &poisoned,
                )
                .unwrap();

                transfer::memcpy_dtod(
                    &mut ctx,
                    &mut sink,
                    DevicePtr(dst_base.0 + dofs),
                    DevicePtr(src_base.0 + so),
                    size as u64,
                )
                .unwrap_or_else(|error| {
                    panic!("size={size} src_off={so} dst_off={dofs} refused: {error:?}")
                });

                let got = readback(&mut sink, &ctx, DevicePtr(dst_base.0 + dofs), size + 1);
                assert_eq!(
                    &got[..size],
                    &payload[..],
                    "size={size} src_off={so} dst_off={dofs}: copied bytes differ"
                );
                assert_eq!(
                    got[size], 0x5A,
                    "size={size} src_off={so} dst_off={dofs}: the copy wrote past its extent"
                );

                if so % A == 0 && dofs % A == 0 && size as u64 % A == 0 {
                    aligned_cases += 1;
                } else {
                    unaligned_cases += 1;
                }
            }
        }
    }

    // A matrix that generated only one family would let the other's tally read as a clean number. An
    // earlier revision of the equivalent probe listed no multiple of four at all, so its aligned control
    // silently tallied zero cases; this is the guard that makes that impossible to miss.
    assert!(
        aligned_cases > 0 && unaligned_cases > 0,
        "the matrix must exercise both families: {aligned_cases} aligned, {unaligned_cases} unaligned"
    );
    assert!(
        unaligned_cases > 1000,
        "only {unaligned_cases} unaligned cases; this is the family the defect lived in"
    );
}

/// The case with no aligned middle at all: the two pointers sit at different positions within their
/// word, so no part of the range can be moved by a device copy and every byte takes the edge path.
#[test]
fn a_copy_between_differently_aligned_pointers_is_exact() {
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    let src = allocate::mem_alloc(&mut ctx, &mut sink, 4096).unwrap();
    let dst = allocate::mem_alloc(&mut ctx, &mut sink, 4096).unwrap();

    // 1 and 2 are different residues modulo 4, and 1023 is not a whole number of words.
    let size = 1023usize;
    let payload: Vec<u8> = (0..size).map(|i| pattern(size, 1, i)).collect();
    transfer::memcpy_htod(&mut ctx, &mut sink, DevicePtr(src.0 + 1), &payload).unwrap();
    transfer::memcpy_htod(&mut ctx, &mut sink, DevicePtr(dst.0 + 2), &vec![0x5A; size]).unwrap();

    transfer::memcpy_dtod(
        &mut ctx,
        &mut sink,
        DevicePtr(dst.0 + 2),
        DevicePtr(src.0 + 1),
        size as u64,
    )
    .expect("a shifted copy must be served, not refused");

    assert_eq!(readback(&mut sink, &ctx, DevicePtr(dst.0 + 2), size), payload);
}

/// A zero-length copy succeeds and disturbs nothing — the split arithmetic must not produce a stray
/// edge write when there is nothing to move.
#[test]
fn a_zero_length_device_copy_is_inert() {
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    let src = allocate::mem_alloc(&mut ctx, &mut sink, 256).unwrap();
    let dst = allocate::mem_alloc(&mut ctx, &mut sink, 256).unwrap();
    let before = vec![0xA5u8; 16];
    transfer::memcpy_htod(&mut ctx, &mut sink, DevicePtr(dst.0 + 3), &before).unwrap();

    transfer::memcpy_dtod(&mut ctx, &mut sink, DevicePtr(dst.0 + 3), DevicePtr(src.0 + 1), 0)
        .expect("a zero-length copy is legal");

    assert_eq!(readback(&mut sink, &ctx, DevicePtr(dst.0 + 3), 16), before);
}

/// The bounds guard still applies to every byte, edges included: a copy that runs past the end of
/// either allocation is refused, and refused BEFORE anything is written.
#[test]
fn an_out_of_bounds_device_copy_is_still_refused_and_writes_nothing() {
    let mut sink = harness();
    let mut ctx = CudaContext::new(CudaDeviceDesc::apple_default(8 << 30));

    let src = allocate::mem_alloc(&mut ctx, &mut sink, 64).unwrap();
    let dst = allocate::mem_alloc(&mut ctx, &mut sink, 64).unwrap();
    let sentinel = vec![0x5Au8; 64];
    transfer::memcpy_htod(&mut ctx, &mut sink, dst, &sentinel).unwrap();

    // Positive control first: an in-bounds unaligned copy through the same path succeeds, so the
    // refusal below cannot be this setup failing for every input.
    transfer::memcpy_dtod(&mut ctx, &mut sink, DevicePtr(dst.0 + 1), DevicePtr(src.0 + 1), 7)
        .expect("the in-bounds unaligned control must succeed");

    transfer::memcpy_htod(&mut ctx, &mut sink, dst, &sentinel).unwrap();
    let error = transfer::memcpy_dtod(&mut ctx, &mut sink, DevicePtr(dst.0 + 33), src, 63)
        .expect_err("a copy running past the destination allocation must be refused");
    assert!(matches!(error, hl_gpu::GpuError::OutOfBounds), "got {error:?}");

    assert_eq!(
        readback(&mut sink, &ctx, dst, 64),
        sentinel,
        "a refused copy must not have written any edge byte"
    );
}
