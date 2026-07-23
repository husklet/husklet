use super::*;

// ---------------------------------------------------------------------------------------------------
// allocate
// ---------------------------------------------------------------------------------------------------

#[test]
fn mem_alloc_emits_create_buffer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 4096).unwrap();

    assert_eq!(sink.batches.len(), 1);
    match &sink.batches[0][0] {
        Cmd::CreateBuffer(id, desc) => {
            assert_eq!(*id, 1);
            assert_eq!(
                *desc,
                BufferDesc {
                    size: 4096,
                    usage: buffer_usage::STORAGE
                        | buffer_usage::COPY_SRC
                        | buffer_usage::COPY_DST
                        | buffer_usage::MAP,
                    label: String::new(),
                }
            );
        }
        other => panic!("expected CreateBuffer, got {other:?}"),
    }
    // device pointer is well above zero and 256-aligned.
    assert_eq!(p.0 % 256, 0);
    assert!(p.0 >= 0x10_0000);
}

#[test]
fn second_alloc_gets_distinct_buffer_and_bumped_pointer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let a = allocate::mem_alloc(&mut c, &mut sink, 100).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 100).unwrap();
    assert_ne!(a.0, b.0);
    assert!(b.0 > a.0);
    // buffer ids 1 then 2
    assert!(matches!(sink.batches[0][0], Cmd::CreateBuffer(1, _)));
    assert!(matches!(sink.batches[1][0], Cmd::CreateBuffer(2, _)));
}

#[test]
fn mem_free_emits_destroy_buffer_and_rejects_bogus() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 64).unwrap();
    allocate::mem_free(&mut c, &mut sink, p).unwrap();
    assert!(matches!(
        sink.batches.last().unwrap()[0],
        Cmd::DestroyBuffer(1)
    ));

    // freeing again (or a bogus pointer) is a typed error, not a panic.
    let err = allocate::mem_free(&mut c, &mut sink, p).unwrap_err();
    assert!(matches!(err, GpuError::Invalid(_)));
}

#[test]
fn allocation_metadata_backs_pointer_and_mem_info_queries() {
    // The model data the driver's `cuPointerGetAttribute` / `cuMemGetAddressRange` / `cuMemGetInfo`
    // entry points read: `containing` resolves an interior pointer to its (base, size), and
    // `total_bytes` is the used figure `cuMemGetInfo` subtracts from total VRAM.
    use hl_cuda::model::device::DevicePtr;
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();

    assert_eq!(c.mem.total_bytes(), 0, "no allocations → nothing used");
    let a = allocate::mem_alloc(&mut c, &mut sink, 4096).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    assert_eq!(
        c.mem.total_bytes(),
        4096 + 256,
        "used = sum of live allocation sizes"
    );

    // An interior pointer resolves to the allocation's base + size.
    assert_eq!(c.mem.containing(DevicePtr(a.0 + 8)), Some((a.0, 4096)));
    assert_eq!(c.mem.containing(DevicePtr(b.0)), Some((b.0, 256)));
    // A dangling pointer resolves to nothing (→ CUDA_ERROR_INVALID_VALUE at the ABI seam).
    assert_eq!(c.mem.containing(DevicePtr(0xdead_beef)), None);

    // Free drops it from both the resolver and the used-bytes total (what cuMemGetInfo reflects).
    allocate::mem_free(&mut c, &mut sink, a).unwrap();
    assert_eq!(c.mem.total_bytes(), 256);
    assert_eq!(c.mem.containing(DevicePtr(a.0 + 8)), None);

    // The free/total cuMemGetInfo would report.
    let total = c.device.total_mem;
    let free = total - c.mem.total_bytes();
    assert_eq!(free, total - 256);
}
