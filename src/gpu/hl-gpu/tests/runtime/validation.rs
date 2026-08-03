use super::*;

#[test]
fn validation_rejects_over_limit_batch_before_any_execute() {
    // Advertise a tiny per-buffer ceiling so a large buffer fails validation.
    let mut caps = Capabilities::permissive_fixture("fake");
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
    let mut caps = Capabilities::permissive_fixture("fake");
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

#[test]
fn validation_keeps_buffer_copy_alignment_without_rejecting_packed_texture_rows() {
    use hl_gpu::protocol::model::descriptor::{Extent3d, Origin3d, TextureSubresource};

    let caps = Capabilities::permissive_fixture("fake");
    let mut exec = FakeExecutor::new(caps.clone());
    let mut s = session(Limits::from_capabilities(caps), GlobalLedger::unbounded());
    let packed_texture_row = vec![Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyBufferToTextureRegion {
            src: 1,
            src_offset: 0,
            bytes_per_row: 2,
            rows_per_image: 1,
            dst: 1,
            dst_sub: TextureSubresource::base(),
            dst_origin: Origin3d::default(),
            extent: Extent3d { width: 1, height: 1, depth: 1 },
        }],
        signal: None,
    })];
    hl_gpu::runtime::submit(&mut s, &mut exec, 64, &packed_texture_row)
        .expect("a packed two-byte texel row is valid even though buffer copies require four bytes");
    assert_eq!(exec.command_count(), 1, "the packed texture upload reached the executor");

    let unaligned_buffer_copy = vec![Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyBufferToBuffer {
            src: 1,
            src_offset: 0,
            dst: 2,
            dst_offset: 0,
            size: 2,
        }],
        signal: None,
    })];
    let error = hl_gpu::runtime::submit(&mut s, &mut exec, 64, &unaligned_buffer_copy).unwrap_err();
    assert_eq!(error, GpuError::ResourceLimit("copy alignment"));
    assert_eq!(exec.command_count(), 1, "the unaligned buffer copy was rejected before execute");
}
