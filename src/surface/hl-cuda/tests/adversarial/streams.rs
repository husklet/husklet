use super::*;

// ===================================================================================================
// streams + synchronize — handle validity state machine
// ===================================================================================================

#[test]
fn stream_lifecycle_and_synchronize_validation() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();

    // default stream is always valid + not destroyable.
    assert!(c.streams.is_valid(StreamTable::DEFAULT));
    assert!(
        !c.streams.destroy(StreamTable::DEFAULT),
        "the default stream cannot be destroyed"
    );

    let s = c.streams.create();
    assert!(c.streams.is_valid(s));
    c.synchronize_stream(&mut sink, s).unwrap();

    // destroy → no longer valid → synchronize + async ops reject it.
    assert!(c.streams.destroy(s));
    assert!(!c.streams.destroy(s), "double-destroy is rejected");
    assert!(!c.streams.is_valid(s));
    assert!(c.synchronize_stream(&mut sink, s).is_err());

    let base = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    assert!(transfer::memcpy_htod_async(&mut c, &mut sink, s, base, &[1, 2]).is_err());
    assert!(transfer::memset_async(&mut c, &mut sink, s, base, &[0u8; 4]).is_err());
    // a never-minted handle is invalid too.
    assert!(!c.streams.is_valid(Stream(4242)));
}

#[test]
fn ctx_synchronize_barrier_uses_a_fresh_fence_value_each_time() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    c.synchronize(&mut sink).unwrap();
    c.synchronize(&mut sink).unwrap();
    // two barriers → two distinct, monotonically increasing fence signal values.
    assert_eq!(sink.waits.len(), 2);
    assert!(
        sink.waits[1].1 > sink.waits[0].1,
        "fence values are monotonic across barriers"
    );
}
