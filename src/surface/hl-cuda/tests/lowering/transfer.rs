use super::*;

// ---------------------------------------------------------------------------------------------------
// transfer
// ---------------------------------------------------------------------------------------------------

#[test]
fn htod_writes_at_resolved_offset() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let base = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    // write into the middle of the allocation via an offset device pointer.
    let dst = hl_cuda::DevicePtr(base.0 + 16);
    transfer::memcpy_htod(&mut c, &mut sink, dst, &[1, 2, 3, 4]).unwrap();

    match sink.batches.last().unwrap().as_slice() {
        [Cmd::WriteBuffer { id, offset, data }] => {
            assert_eq!(*id, 1);
            assert_eq!(*offset, 16);
            assert_eq!(data, &[1, 2, 3, 4]);
        }
        other => panic!("expected one WriteBuffer, got {other:?}"),
    }
}

#[test]
fn dtod_emits_copy_buffer_to_buffer() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let a = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let b = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    transfer::memcpy_dtod(&mut c, &mut sink, b, a, 128).unwrap();

    match sink.batches.last().unwrap().as_slice() {
        [Cmd::Submit(cb)] => match cb.encoder.as_slice() {
            [Enc::CopyBufferToBuffer {
                src,
                src_offset,
                dst,
                dst_offset,
                size,
            }] => {
                assert_eq!((*src, *src_offset), (1, 0));
                assert_eq!((*dst, *dst_offset), (2, 0));
                assert_eq!(*size, 128);
            }
            other => panic!("expected CopyBufferToBuffer, got {other:?}"),
        },
        other => panic!("expected one Submit, got {other:?}"),
    }
}

#[test]
fn dtoh_resolves_without_submitting() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = allocate::mem_alloc(&mut c, &mut sink, 256).unwrap();
    let batches_before = sink.batches.len();
    let (buf, off) = c.device_location(hl_cuda::DevicePtr(p.0 + 8)).unwrap();
    assert_eq!((buf.0, off), (1, 8));
    // no command was submitted for the readback.
    assert_eq!(sink.batches.len(), batches_before);
}
