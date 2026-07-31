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

/// A destroyed stream's id is never handed out again, so a stale `CUstream` cannot come back to life as
/// a live stream belonging to someone else.
///
/// Handle validation rejects an id that is not live, which is what makes use-after-destroy an error —
/// but only while the id stays dead. If `create` reused a freed id, a stale handle would pass every
/// validity check and address a different stream than its holder believes, and there is no observation
/// that separates that from correct use: the call succeeds, the work lands, and it lands on the wrong
/// queue. This is the handle analogue of a wrong answer arriving on a correct call.
///
/// `next_id` is monotonic, so this holds today; the test pins it because reuse is the natural thing to
/// add when someone later wants ids to stay small.
#[test]
fn a_destroyed_stream_id_is_never_reminted() {
    let mut c = ctx();

    let first = c.streams.create();
    let second = c.streams.create();
    assert!(c.streams.destroy(first));
    assert!(c.streams.destroy(second));

    // Both freed ids must stay dead, and neither may be reissued to a later create.
    let mut minted = Vec::new();
    for _ in 0..8 {
        minted.push(c.streams.create());
    }
    for dead in [first, second] {
        assert!(
            !minted.contains(&dead),
            "a destroyed stream id {dead:?} was reissued; a stale handle would pass validation and \
             silently address a different stream than its holder believes",
        );
    }
    // And the default stream is never minted as an explicit stream, or a stale explicit handle would
    // alias the implicit queue every guest already uses.
    assert!(
        !minted.contains(&StreamTable::DEFAULT),
        "create() minted the default stream id",
    );
}
