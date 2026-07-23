use super::*;

// ===================================================================================================
// allocate — lifecycle invariants: double free, interior-base free, cross-kind free
// ===================================================================================================

#[test]
fn free_of_interior_pointer_is_rejected() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    // Freeing an interior (non-base) pointer must fail — only the exact allocation base frees.
    assert!(allocate::mem_free(&mut c, &mut sink, DevicePtr(p.0 + 8)).is_err());
    // The allocation is still live (the bogus free did not destroy it).
    assert!(c.mem.containing(p).is_some());
    // The real base frees cleanly, then a repeat is a double-free error.
    allocate::mem_free(&mut c, &mut sink, p).unwrap();
    assert!(allocate::mem_free(&mut c, &mut sink, p).is_err());
    assert!(c.resolve(p).is_none(), "a freed pointer no longer resolves");
}

#[test]
fn host_free_of_a_device_pointer_and_vice_versa_are_rejected() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let dev = allocate::mem_alloc(&mut c, &mut sink, 128).unwrap();
    let host = c.host_alloc(128).unwrap();
    // Cross-kind frees are rejected (a device pointer is not a pinned host base, and vice versa).
    assert!(
        c.host_free(dev.0).is_err(),
        "device ptr is not a pinned host base"
    );
    assert!(
        allocate::mem_free(&mut c, &mut sink, DevicePtr(host)).is_err(),
        "host base is not a device alloc"
    );
    // Each frees correctly through its own path.
    c.host_free(host).unwrap();
    allocate::mem_free(&mut c, &mut sink, dev).unwrap();
}

#[test]
fn pitch_alloc_overflow_is_a_typed_error_not_a_panic() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // pitch(width) * height overflows u64 → a typed error, never a wrapping allocation.
    let huge = u64::MAX / 2;
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, huge, huge, 4).is_err());
    // zero extents are rejected too.
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, 0, 8, 4).is_err());
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, 8, 0, 4).is_err());
}

#[test]
fn managed_and_device_allocations_do_not_alias_managed_flag() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let managed = allocate::mem_alloc_managed(&mut c, &mut sink, 256).unwrap();
    let device = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    assert!(c.mem.is_managed(managed));
    assert!(
        c.mem.is_managed(DevicePtr(managed.0 + 100)),
        "interior pointer is managed too"
    );
    assert!(!c.mem.is_managed(device));
    // Freeing the managed allocation clears its managed flag (no stale membership).
    allocate::mem_free(&mut c, &mut sink, managed).unwrap();
    assert!(!c.mem.is_managed(managed));
}

#[test]
fn host_get_device_pointer_bounds_the_backing_buffer_to_the_host_size() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = c.host_alloc(200).unwrap();
    let dptr = c.host_get_device_pointer(&mut sink, base).unwrap();
    // The backing device buffer is exactly the host allocation size and resolves as a live allocation.
    assert_eq!(c.mem.containing(dptr), Some((dptr.0, 200)));
    // Freeing the pinned host allocation drops its device mapping; a re-map mints a NEW device buffer.
    c.host_free(base).unwrap();
    assert!(
        c.host_get_device_pointer(&mut sink, base).is_err(),
        "freed host base unmaps"
    );
}
