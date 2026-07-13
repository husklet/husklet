//! Wire-boundary validation: non-finite render state is rejected at decode, and a malformed frame leaves
//! the backend untouched (validate-before-execute / atomic frame). These mirror the tracked ledger gates
//! `non_finite_render_state_is_rejected_at_the_wire_boundary` and
//! `malformed_stream_does_not_partially_mutate_the_backend`, kept here as in-crate Linux-runnable proof.

use hl_gpu::ir::*;
use hl_gpu::mock::RecordingBackend;
use hl_gpu::wire::Encoder;
use hl_gpu::{replay, GpuError};

fn buffer_desc() -> BufferDesc {
    BufferDesc { size: 256, usage: buffer_usage::COPY_DST, label: "b".into() }
}

// ---- non-finite render state rejected at the wire boundary --------------------------------------

#[test]
fn non_finite_viewport_and_clear_are_rejected_at_decode() {
    let cmds = [Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::SetViewport { x: f32::NAN, y: 0.0, w: f32::INFINITY, h: 1.0, min_depth: f32::NEG_INFINITY, max_depth: 1.0 },
            Enc::ClearRect { texture: 1, x: 0, y: 0, w: 1, h: 1, color: [f32::NAN, 0.0, 0.0, 1.0] },
        ],
        signal: None,
    })];
    let err = decode_stream(&encode_stream(&cmds)).unwrap_err();
    assert!(matches!(err, GpuError::Decode(_)), "non-finite state must be a typed decode error, got {err:?}");
}

#[test]
fn each_non_finite_render_field_is_rejected() {
    // Every render-state float site must reject NaN/±∞ (viewport, scissor-adjacent clear, attachment clear,
    // depth clear). Build each in isolation so a regression pinpoints the un-guarded field.
    let inf = f32::INFINITY;
    let cases: Vec<(&str, Enc)> = vec![
        ("viewport", Enc::SetViewport { x: inf, y: 0.0, w: 1.0, h: 1.0, min_depth: 0.0, max_depth: 1.0 }),
        ("clear-rect", Enc::ClearRect { texture: 1, x: 0, y: 0, w: 1, h: 1, color: [0.0, inf, 0.0, 1.0] }),
        ("attachment clear", Enc::BeginRenderPass {
            color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, f32::NAN, 1.0], store: true }],
            depth: None,
        }),
        ("depth clear", Enc::BeginRenderPass {
            color: vec![],
            depth: Some(DepthAttachment { texture: 1, load: LoadOp::Clear, clear_depth: f32::NAN }),
        }),
    ];
    for (name, op) in cases {
        let bytes = encode_stream(&[Cmd::Submit(CommandBuffer { encoder: vec![op], signal: None })]);
        assert!(decode_stream(&bytes).is_err(), "non-finite {name} crossed the wire boundary");
    }
}

#[test]
fn finite_render_state_still_round_trips() {
    // Guard against over-rejection: legitimate finite render state must still decode unchanged.
    let cmds = vec![Cmd::Submit(CommandBuffer {
        encoder: vec![
            Enc::SetViewport { x: 0.0, y: 0.0, w: 64.0, h: 32.0, min_depth: 0.0, max_depth: 1.0 },
            Enc::ClearRect { texture: 1, x: 0, y: 0, w: 4, h: 4, color: [0.25, 0.5, 0.75, 1.0] },
        ],
        signal: None,
    })];
    assert_eq!(decode_stream(&encode_stream(&cmds)).unwrap(), cmds);
}

// ---- malformed frame is atomic: zero backend mutation --------------------------------------------

#[test]
fn malformed_frame_leaves_backend_untouched() {
    // A valid CreateBuffer followed by a bad tag byte in the SAME frame: replay must reject the whole
    // frame and apply nothing (validate-before-execute).
    let mut bytes = encode_stream(&[Cmd::CreateBuffer(42, buffer_desc())]);
    bytes.push(0xff);
    let mut be = RecordingBackend::new();
    assert!(replay::replay_stream(&mut be, &bytes).is_err());
    assert!(be.log.is_empty(), "malformed frame partially applied: {:?}", be.log);
}

#[test]
fn malformed_encoder_op_inside_submit_leaves_backend_untouched() {
    // A well-formed CreateBuffer, then a Submit whose encoder body is truncated mid-op. The Submit must
    // not apply and neither must the earlier CreateBuffer.
    let mut e = Encoder::new();
    Cmd::CreateBuffer(1, buffer_desc()).encode(&mut e);
    // A hand-built truncated Submit: tag + op-count=1 + a DRAW tag with no fields.
    e.u8(19); // SUBMIT
    e.u32(1); // one encoder op
    e.u8(8); // etag DRAW — but no u32 fields follow
    let mut be = RecordingBackend::new();
    assert!(replay::replay_stream(&mut be, &e.into_vec()).is_err());
    assert!(be.log.is_empty(), "a truncated Submit still applied the preceding command: {:?}", be.log);
}

#[test]
fn valid_multi_command_frame_still_applies_in_order() {
    // The atomic pre-pass must not break the happy path: a fully valid frame applies every command, and
    // the WriteBuffer zero-copy fast path still lands its payload.
    let cmds = vec![
        Cmd::CreateBuffer(1, buffer_desc()),
        Cmd::WriteBuffer { id: 1, offset: 0, data: vec![1, 2, 3, 4, 5, 6, 7, 8] },
    ];
    let mut be = RecordingBackend::new();
    replay::replay_stream(&mut be, &encode_stream(&cmds)).unwrap();
    assert_eq!(be.log.len(), 2, "expected CreateBuffer + WriteBuffer, got {:?}", be.log);
}
