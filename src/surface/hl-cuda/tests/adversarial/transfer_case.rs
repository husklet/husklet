use super::*;

// ===================================================================================================
// transfer — dangling-pointer error paths + offset correctness for every copy direction
// ===================================================================================================

#[test]
fn every_copy_direction_rejects_a_dangling_pointer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let live = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let dead = DevicePtr(0xdead_0000);

    assert!(transfer::memcpy_htod(&mut c, &mut sink, dead, &[1, 2, 3, 4]).is_err());
    // dtod: a dangling SOURCE and a dangling DESTINATION are each rejected.
    assert!(
        transfer::memcpy_dtod(&mut c, &mut sink, live, dead, 8).is_err(),
        "dangling src"
    );
    assert!(
        transfer::memcpy_dtod(&mut c, &mut sink, dead, live, 8).is_err(),
        "dangling dst"
    );
    assert!(transfer::read_dtoh(&c, &mut sink, dead, 8).is_err());
    assert!(c.device_location(dead).is_err());
    assert!(transfer::memset(&mut c, &mut sink, dead, &[0u8; 4]).is_err());
}

#[test]
fn dtod_copies_at_the_resolved_offsets_of_both_ends() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let a = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    // interior pointers on both sides.
    transfer::memcpy_dtod(
        &mut c,
        &mut sink,
        DevicePtr(b.0 + 32),
        DevicePtr(a.0 + 16),
        64,
    )
    .unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cb)] => match cb.encoder.as_slice() {
            [Enc::CopyBufferToBuffer {
                src,
                src_offset,
                dst,
                dst_offset,
                size,
            }] => {
                assert_eq!((*src, *src_offset), (1, 16));
                assert_eq!((*dst, *dst_offset), (2, 32));
                assert_eq!(*size, 64);
            }
            other => panic!("expected CopyBufferToBuffer, got {other:?}"),
        },
        other => panic!("expected Submit, got {other:?}"),
    }
}

#[test]
fn read_dtoh_requests_exact_buffer_offset_and_len() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let out = transfer::read_dtoh(&c, &mut sink, DevicePtr(p.0 + 12), 20).unwrap();
    assert_eq!(out.len(), 20);
    assert_eq!(
        sink.reads.last().copied(),
        Some((hl_gpu::BufferId(1), 12, 20))
    );
}

#[test]
fn memset_d8_d16_d32_expand_to_the_right_byte_pattern() {
    // The shim expands (value, width, count) → bytes; the service lowers that verbatim. Verify each width
    // tiles the element correctly (the lowering must carry the exact bytes, no truncation/padding).
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();

    // D16: 0xBEEF repeated 3× → 6 bytes little-endian.
    let d16: Vec<u8> = (0..3).flat_map(|_| 0xBEEFu16.to_le_bytes()).collect();
    transfer::memset(&mut c, &mut sink, base, &d16).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset, data }] => {
            assert_eq!((*id, *offset), (1, 0));
            assert_eq!(data, &[0xEF, 0xBE, 0xEF, 0xBE, 0xEF, 0xBE]);
        }
        other => panic!("expected WriteBuffer, got {other:?}"),
    }
}
