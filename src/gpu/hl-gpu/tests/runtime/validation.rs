use super::*;

#[test]
fn validation_rejects_over_limit_batch_before_any_execute() {
    // Advertise a tiny per-buffer ceiling so a large buffer fails validation.
    let mut caps = Capabilities::full("fake");
    caps.max_buffer_bytes = 1024;
    let mut exec = FakeExecutor::new(caps.clone());
    let mut s = session(Limits::from_capabilities(caps), GlobalLedger::unbounded());

    let batch = vec![buffer(1, 4096)]; // 4096 > max_buffer_bytes(1024)
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 64, &batch).unwrap_err();

    assert_eq!(err, GpuError::ResourceLimit("buffer bytes"));
    assert_eq!(
        exec.command_count(),
        0,
        "rejected batch never reached the executor"
    );
    assert_eq!(
        s.residency_bytes(),
        0,
        "rejected batch charged no residency"
    );
    assert_eq!(s.object_count(), 0);
    assert_eq!(s.resources.live_count(), 0);
}

/// Failure atomicity at VALIDATE for a malformed batch: an encoder op whose command tag is not in the
/// negotiated set is rejected before execute/charge.
#[test]
fn validation_rejects_unnegotiated_command_before_any_execute() {
    // Advertise a command set WITHOUT Dispatch, then submit a Dispatch.
    let mut caps = Capabilities::full("fake");
    caps.command_bits = hl_gpu::Capabilities::command_bits(&[
        hl_gpu::protocol::model::command::etag::BEGIN_RENDER_PASS,
        hl_gpu::protocol::model::command::etag::CLEAR_RECT,
    ]);
    let mut exec = FakeExecutor::new(caps.clone());
    let mut s = session(Limits::from_capabilities(caps), GlobalLedger::unbounded());

    let batch = vec![Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::Dispatch { x: 1, y: 1, z: 1 }],
        signal: None,
    })];
    let err = hl_gpu::runtime::submit(&mut s, &mut exec, 64, &batch).unwrap_err();

    assert_eq!(err, GpuError::ResourceLimit("encoder command"));
    assert_eq!(exec.command_count(), 0);
    assert_eq!(s.residency_bytes(), 0);
}
