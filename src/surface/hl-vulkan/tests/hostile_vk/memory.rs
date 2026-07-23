use super::*;

// =====================================================================================================
// oversized / zero allocations — vkAllocateMemory (REGRESSION: over-heap size host-Vec capacity panic)
// =====================================================================================================

#[test]
fn allocate_memory_zero_over_budget_and_u64max_then_valid() {
    let mut d = dev();
    // A zero allocationSize is a spec usage error (VUID-VkMemoryAllocateInfo-allocationSize-00638).
    assert!(matches!(d.allocate_memory(0), Err(GpuError::Invalid(_))));
    // Over the modeled 8 GiB unified heap → an honest VK_ERROR_OUT_OF_DEVICE_MEMORY, NOT a fake success.
    let over = d.physical_device.memory_heap_bytes + 1;
    let err = d.allocate_memory(over).unwrap_err();
    assert!(matches!(err, GpuError::ResourceLimit(_)));
    assert_eq!(
        Status::from_error(&err),
        result::VK_ERROR_OUT_OF_DEVICE_MEMORY
    );
    // `u64::MAX` previously capacity-overflow-panicked `vec![0u8; size as usize]` in the host — now a
    // truthful error before any host allocation is attempted.
    assert!(d.allocate_memory(u64::MAX).is_err());
    // A valid allocation after every abuse still works.
    let mem = d.allocate_memory(4096).unwrap();
    assert!(d.memories.contains_key(&mem));
}

// =====================================================================================================
// vkMapMemory out-of-range write (REGRESSION: `offset as usize + len` add-overflow panic)
// =====================================================================================================

#[test]
fn write_mapped_offset_overflow_is_out_of_bounds_then_valid() {
    let mut d = dev();
    let mem = d.allocate_memory(16).unwrap();
    // `u64::MAX` offset previously overflow-panicked `offset as usize + bytes.len()`; now OutOfBounds.
    assert!(matches!(
        create::write_mapped(&mut d, mem, u64::MAX, &[0u8; 4]),
        Err(GpuError::OutOfBounds)
    ));
    assert!(matches!(
        create::write_mapped(&mut d, mem, u64::MAX - 2, &[0u8; 4]),
        Err(GpuError::OutOfBounds)
    ));
    // A plainly out-of-range (non-overflowing) write is also OutOfBounds.
    assert!(matches!(
        create::write_mapped(&mut d, mem, 14, &[0u8; 4]),
        Err(GpuError::OutOfBounds)
    ));
    // Unknown memory is a typed Invalid, never a panic.
    assert!(matches!(
        create::write_mapped(&mut d, 0xdead, 0, &[0u8; 4]),
        Err(GpuError::Invalid(_))
    ));
    // A valid in-range write still works.
    d.map_memory(mem).unwrap();
    create::write_mapped(&mut d, mem, 0, &[1, 2, 3, 4]).unwrap();
    assert_eq!(&d.memories.get(&mem).unwrap().data[..4], &[1, 2, 3, 4]);
}
