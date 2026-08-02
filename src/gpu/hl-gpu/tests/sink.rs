//! `CommandSink` port test: a driver-style flow drives a [`RecordingSink`] test double and we assert it
//! captured the negotiation, the submitted batch contents, and the fence wait — no socket, no GPU.

use hl_gpu::protocol::model::capability::{shader_payload, ALL_COMMANDS, COLOR_FORMATS};
use hl_gpu::protocol::model::command::*;
use hl_gpu::protocol::model::descriptor::*;
use hl_gpu::protocol::model::enums::*;
use hl_gpu::protocol::model::id::FenceId;
use hl_gpu::{CommandSink, FeatureRequest, GpuError, RecordingSink, WIRE_VERSION};

fn feature_request() -> FeatureRequest {
    FeatureRequest {
        wire_version: WIRE_VERSION,
        shader_payloads: shader_payload::SPIRV,
        command_bits: hl_gpu::Capabilities::command_bits(ALL_COMMANDS),
        texture_formats: TextureFormat::bits(COLOR_FORMATS),
        ..FeatureRequest::default()
    }
}

#[test]
fn recording_sink_captures_negotiate_submit_and_wait() {
    let mut sink = RecordingSink::with_full_caps();

    // 1) negotiate — the sink honours the real contract and returns its advertised caps.
    let caps = sink.negotiate(&feature_request()).expect("negotiate ok");
    assert_eq!(caps.wire_version, WIRE_VERSION);
    assert!(caps.supports_shader_payload(shader_payload::SPIRV));
    assert_eq!(sink.negotiated.len(), 1);

    // 2) submit a batch.
    let batch = vec![
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: 64,
                usage: buffer_usage::STORAGE,
                label: "b".into(),
            },
        ),
        Cmd::CreateFence(2),
        Cmd::Submit(CommandBuffer {
            encoder: vec![
                Enc::BeginComputePass,
                Enc::Dispatch { x: 4, y: 1, z: 1 },
                Enc::EndComputePass,
            ],
            signal: Some((2, 5)),
        }),
    ];
    sink.submit(&batch).unwrap();

    // 3) wait a fence.
    sink.wait(FenceId(2), 5).unwrap();

    // assertions on the recorded contents
    assert_eq!(sink.batches.len(), 1);
    assert_eq!(sink.batches[0], batch, "submitted batch recorded verbatim");
    assert_eq!(sink.command_count(), 3);
    assert!(matches!(
        sink.commands().next(),
        Some(Cmd::CreateBuffer(1, _))
    ));
    assert_eq!(sink.waits, vec![(FenceId(2), 5)]);
}

#[test]
fn recording_sink_negotiate_rejects_unsatisfiable_request() {
    let mut sink = RecordingSink::with_full_caps();
    // A wire-version mismatch fails negotiation with a typed error, exactly like a real sink.
    let bad = FeatureRequest {
        wire_version: WIRE_VERSION + 1,
        ..feature_request()
    };
    assert_eq!(
        sink.negotiate(&bad),
        Err(GpuError::Unsupported("capability: wire version mismatch"))
    );
    // the request was still recorded even though it was rejected
    assert_eq!(sink.negotiated.len(), 1);
    assert!(sink.batches.is_empty());
}

#[test]
fn texel_buffer_wire_version_rejects_a_v16_peer_and_accepts_v17() {
    assert_eq!(WIRE_VERSION, 17);
    let mut sink = RecordingSink::with_full_caps();
    let stale = FeatureRequest {
        wire_version: 16,
        ..feature_request()
    };
    assert_eq!(
        sink.negotiate(&stale),
        Err(GpuError::Unsupported("capability: wire version mismatch"))
    );
    assert_eq!(sink.negotiate(&feature_request()).unwrap().wire_version, 17);
}

#[test]
fn command_sink_is_object_safe() {
    // The port must be usable as `&mut dyn CommandSink` (drivers hold a boxed sink).
    let mut sink = RecordingSink::with_full_caps();
    let dynsink: &mut dyn CommandSink = &mut sink;
    dynsink.submit(&[Cmd::CreateFence(1)]).unwrap();
    assert_eq!(sink.batches.len(), 1);
}
