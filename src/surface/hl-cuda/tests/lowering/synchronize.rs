use super::*;

// ---------------------------------------------------------------------------------------------------
// synchronize
// ---------------------------------------------------------------------------------------------------

#[test]
fn ctx_synchronize_emits_fence_barrier() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    c.synchronize(&mut sink).unwrap();

    // batch 0: CreateFence + signalling Submit. batch 1: DestroyFence. one recorded wait.
    match sink.batches[0].as_slice() {
        [Cmd::CreateFence(fid), Cmd::Submit(cb)] => {
            assert_eq!(cb.signal, Some((*fid, 1)));
            assert!(cb.encoder.is_empty());
        }
        other => panic!("expected CreateFence + Submit, got {other:?}"),
    }
    assert!(matches!(sink.batches[1].as_slice(), [Cmd::DestroyFence(_)]));
    assert_eq!(sink.waits.len(), 1);
    assert_eq!(sink.waits[0].1, 1);
}

#[test]
fn stream_synchronize_validates_handle() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // the default stream is always valid.
    c.synchronize_stream(&mut sink, hl_cuda::model::stream::StreamTable::DEFAULT)
        .unwrap();
    // a created stream is valid.
    let s = c.streams.create();
    c.synchronize_stream(&mut sink, s).unwrap();
    // a bogus handle errors.
    let err = c.synchronize_stream(&mut sink, Stream(9999)).unwrap_err();
    assert!(matches!(err, GpuError::Invalid(_)));
}
