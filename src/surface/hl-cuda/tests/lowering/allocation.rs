use super::*;

// ---------------------------------------------------------------------------------------------------
// memory: managed / pitched / host (pinned + registered) / memset / async
// ---------------------------------------------------------------------------------------------------

#[test]
fn managed_alloc_emits_create_buffer_and_flags_managed() {
    use hl_cuda::model::device::DevicePtr;
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc_managed(&mut c, &mut sink, 4096).unwrap();

    // same single CreateBuffer as a plain device allocation …
    assert!(matches!(
        sink.batches.last().unwrap()[0],
        Cmd::CreateBuffer(1, _)
    ));
    // … but the model flags it managed (an interior pointer resolves managed too).
    assert!(c.mem.is_managed(p));
    assert!(c.mem.is_managed(DevicePtr(p.0 + 16)));

    // a plain device allocation is NOT managed.
    let d = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    assert!(!c.mem.is_managed(d));
    // free clears the managed flag.
    allocate::mem_free(&mut c, &mut sink, p).unwrap();
    assert!(!c.mem.is_managed(p));
}

#[test]
fn pitch_alloc_aligns_pitch_and_sizes_buffer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // 100-byte rows round up to a 512-byte pitch; buffer = pitch*height.
    let (p, pitch) = allocate::mem_alloc_pitch(&mut c, &mut sink, 100, 8, 4).unwrap();
    assert_eq!(pitch, 512);
    match sink.batches.last().unwrap()[0] {
        Cmd::CreateBuffer(1, ref desc) => assert_eq!(desc.size, 512 * 8),
        ref other => panic!("expected CreateBuffer sized to pitch*height, got {other:?}"),
    }
    // an already-aligned width is unchanged.
    let (_, pitch2) = allocate::mem_alloc_pitch(&mut c, &mut sink, 1024, 2, 4).unwrap();
    assert_eq!(pitch2, 1024);
    // zero extent is a typed error.
    assert!(allocate::mem_alloc_pitch(&mut c, &mut sink, 0, 8, 4).is_err());
    // the base pointer is a live device allocation.
    assert!(c.mem.containing(p).is_some());
}

#[test]
fn host_alloc_gives_usable_buffer_and_frees() {
    let mut c = ctx();
    // a pinned allocation is real, writable host memory of the requested size.
    let base = c.host_alloc(64).unwrap();
    assert_ne!(base, 0);
    assert_eq!(c.host.size_of(base), Some(64));
    unsafe {
        let p = base as *mut u8;
        for i in 0..64u8 {
            *p.add(i as usize) = i;
        }
        assert_eq!(*p.add(10), 10);
    }
    // free reclaims it; a second free is a typed error.
    c.host_free(base).unwrap();
    assert!(c.host_free(base).is_err());
}

#[test]
fn host_register_unregister_tracks_guest_range() {
    let mut c = ctx();
    let mut buf = [0u8; 32];
    let base = buf.as_mut_ptr() as u64;
    c.host_register(base, 32).unwrap();
    assert_eq!(c.host.size_of(base), Some(32));
    // double-register is rejected.
    assert!(c.host_register(base, 32).is_err());
    c.host_unregister(base).unwrap();
    // unregister of an unknown base is rejected.
    assert!(c.host_unregister(base).is_err());
}

#[test]
fn host_get_device_pointer_maps_to_a_device_buffer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = c.host_alloc(128).unwrap();

    let dptr = c.host_get_device_pointer(&mut sink, base).unwrap();
    // it created exactly one backing device buffer sized to the host allocation …
    match sink.batches.last().unwrap()[0] {
        Cmd::CreateBuffer(id, ref desc) => {
            assert_eq!(id, 1);
            assert_eq!(desc.size, 128);
        }
        ref other => panic!("expected CreateBuffer, got {other:?}"),
    }
    // … and the device pointer resolves as a live device allocation.
    assert_eq!(c.mem.containing(dptr), Some((dptr.0, 128)));

    // repeat calls return the SAME device pointer and emit no new buffer.
    let batches = sink.batches.len();
    let dptr2 = c.host_get_device_pointer(&mut sink, base).unwrap();
    assert_eq!(dptr, dptr2);
    assert_eq!(
        sink.batches.len(),
        batches,
        "cached mapping emits no new CreateBuffer"
    );

    // an unknown host pointer is a typed error.
    assert!(c.host_get_device_pointer(&mut sink, 0xdead_beef).is_err());
}

#[test]
fn memset_d32_writes_repeated_pattern() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    // cuMemsetD32(base, 0xAABBCCDD, 4) → a WriteBuffer of the 4-times-repeated little-endian word.
    let word: u32 = 0xAABB_CCDD;
    let pattern: Vec<u8> = (0..4).flat_map(|_| word.to_le_bytes()).collect();
    transfer::memset(&mut c, &mut sink, base, &pattern).unwrap();

    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset, data }] => {
            assert_eq!((*id, *offset), (1, 0));
            assert_eq!(data.len(), 16);
            assert_eq!(data, &pattern);
            // every 4-byte lane is the pattern word.
            for chunk in data.chunks_exact(4) {
                assert_eq!(u32::from_le_bytes(chunk.try_into().unwrap()), word);
            }
        }
        other => panic!("expected one WriteBuffer, got {other:?}"),
    }

    // a dangling destination is a typed error (no fake success).
    assert!(transfer::memset(&mut c, &mut sink, hl_cuda::DevicePtr(0xdead), &pattern).is_err());
}

#[test]
fn htod_async_records_same_write_as_sync_and_validates_stream() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let dst = hl_cuda::DevicePtr(base.0 + 8);

    // async on a live stream records exactly the sync HtoD WriteBuffer.
    let s = c.streams.create();
    transfer::memcpy_htod_async(&mut c, &mut sink, s, dst, &[9, 8, 7, 6]).unwrap();
    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset, data }] => {
            assert_eq!((*id, *offset), (1, 8));
            assert_eq!(data, &[9, 8, 7, 6]);
        }
        other => panic!("expected one WriteBuffer, got {other:?}"),
    }
    // the default stream is valid too.
    transfer::memcpy_htod_async(&mut c, &mut sink, Stream(0), dst, &[1]).unwrap();
    // a bogus stream handle is a typed error.
    assert!(transfer::memcpy_htod_async(&mut c, &mut sink, Stream(9999), dst, &[1]).is_err());
}

#[test]
fn dtoh_async_reads_bytes_back_through_the_sink() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let s = c.streams.create();

    let out =
        transfer::read_dtoh_async(&c, &mut sink, s, hl_cuda::DevicePtr(base.0 + 4), 16).unwrap();
    assert_eq!(out.len(), 16);
    // the readback request was recorded against the resolved (buffer, offset, len).
    assert_eq!(
        sink.reads.last().copied(),
        Some((hl_gpu::BufferId(1), 4, 16))
    );
    // a bogus stream is rejected.
    assert!(transfer::read_dtoh_async(&c, &mut sink, Stream(9999), base, 4).is_err());
}

#[test]
fn dtod_async_emits_copy_buffer_to_buffer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let a = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let s = c.streams.create();
    transfer::memcpy_dtod_async(&mut c, &mut sink, s, b, a, 64).unwrap();

    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cb)] => match cb.encoder.as_slice() {
            [Enc::CopyBufferToBuffer { src, dst, size, .. }] => {
                assert_eq!((*src, *dst, *size), (1, 2, 64));
            }
            other => panic!("expected CopyBufferToBuffer, got {other:?}"),
        },
        other => panic!("expected one Submit, got {other:?}"),
    }
}
